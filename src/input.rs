use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::Scope;
use std::time::Duration;
use std::{env, fmt};

use rusty_s3::{Bucket, Credentials, UrlStyle};
use sirocco::inflate::Decoder;

const DECODE_BUFFER_SIZE: usize = 256 * 1024;
const FILE_BUFFER_SIZE: usize = 64 * 1024;
const CHUNK_SIZE: usize = 1024 * 1024;
const PLAIN_READERS: usize = 4;
const READ_AHEAD_CHUNKS: usize = 4;
const GZIP_MAGIC_BYTES: [u8; 2] = [0x1f, 0x8b];

/// input source
#[derive(Debug, Clone)]
pub enum Source {
    Stdin,
    Local(PathBuf),
    Http(String),
    S3 { bucket: String, key: String },
}

impl Source {
    /// Parses a command line input string into http, S3 or local
    pub fn parse(argument: OsString) -> Result<Self, String> {
        if argument == "-" {
            return Ok(Self::Stdin);
        }
        let Some(input) = argument.to_str() else {
            return Ok(Self::Local(argument.into()));
        };
        if input.starts_with("http://") || input.starts_with("https://") {
            return Ok(Self::Http(input.to_owned()));
        }
        let Some(object) = input.strip_prefix("s3://") else {
            return Ok(Self::Local(argument.into()));
        };
        match object.split_once('/') {
            Some((bucket, key)) if !bucket.is_empty() && !key.is_empty() => Ok(Self::S3 {
                bucket: bucket.to_owned(),
                key: key.to_owned(),
            }),
            _ => Err("expected s3://<bucket>/<key>".to_owned()),
        }
    }

    /// The path on disk, will be None for stdin or a remote source.
    pub fn local_path(&self) -> Option<&Path> {
        match self {
            Self::Local(path) => Some(path),
            Self::Stdin | Self::Http(_) | Self::S3 { .. } => None,
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdin => formatter.write_str("stdin"),
            Self::Local(path) => write!(formatter, "{}", path.display()),
            Self::Http(url) => formatter.write_str(url),
            Self::S3 { bucket, key } => write!(formatter, "s3://{bucket}/{key}"),
        }
    }
}

/// Opens an input and starts threaded reading.
pub fn spawn_readers<'scope>(
    scope: &'scope Scope<'scope, '_>,
    source: &Source,
) -> io::Result<Chunks> {
    match source {
        Source::Stdin => Ok(spawn_stream_reader(scope, io::stdin())),
        Source::Local(path) => spawn_file_readers(scope, path),
        Source::Http(url) => spawn_http_reader(scope, url),
        Source::S3 { bucket, key } => spawn_http_reader(scope, &s3_url(bucket, key)?),
    }
}

pub struct Chunks {
    receivers: Vec<Receiver<io::Result<Vec<u8>>>>,
    next: usize,
}

impl Chunks {
    fn new(receivers: Vec<Receiver<io::Result<Vec<u8>>>>) -> Self {
        Self { receivers, next: 0 }
    }
}

impl Iterator for Chunks {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        let receiver = &self.receivers[self.next % self.receivers.len()];
        self.next += 1;
        receiver.recv().ok()
    }
}

struct GzipReader<R> {
    inner: R,
    decoder: Box<Decoder>,
    buffer: Vec<u8>,
    pending: Range<usize>,
    full: bool,
    done: bool,
}

impl<R: BufRead> GzipReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            decoder: Box::new(Decoder::new(sirocco::Format::Gzip)),
            buffer: vec![0; DECODE_BUFFER_SIZE],
            pending: 0..0,
            full: false,
            done: false,
        }
    }

    fn fill(&mut self) -> io::Result<()> {
        while self.pending.is_empty() {
            let input = self.inner.fill_buf()?;
            let end_of_input = input.is_empty();
            if end_of_input && self.done {
                return Ok(());
            }
            if self.full {
                self.decoder.make_room(&mut self.buffer);
            }

            let base = self.buffer.as_ptr().addr();
            let progress = self
                .decoder
                .decompress(input, &mut self.buffer, end_of_input)?;
            let start = progress.output.as_ptr().addr() - base;

            self.pending = start..start + progress.output.len();
            self.full = progress.full;
            self.done = progress.done;
            let consumed = progress.consumed;
            self.inner.consume(consumed);
        }
        Ok(())
    }
}

impl<R: BufRead> Read for GzipReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.fill()?;
        let pending = &self.buffer[self.pending.clone()];
        let length = pending.len().min(output.len());

        output[..length].copy_from_slice(&pending[..length]);
        self.pending.start += length;
        Ok(length)
    }
}

pub fn is_gzip(file: &File) -> io::Result<bool> {
    let mut magic = [0; 2];
    let length = file.read_at(&mut magic, 0)?;
    Ok(magic[..length] == GZIP_MAGIC_BYTES)
}

fn s3_url(bucket: &str, key: &str) -> io::Result<String> {
    use rusty_s3::S3Action;

    let (endpoint, style) = match env::var("AWS_ENDPOINT_URL") {
        Ok(endpoint) => (endpoint, UrlStyle::Path),
        Err(_) => ("https://s3.amazonaws.com".to_owned(), UrlStyle::VirtualHost),
    };
    let endpoint = endpoint.parse().map_err(io::Error::other)?;
    let region = env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_owned());
    let bucket =
        Bucket::new(endpoint, style, bucket.to_owned(), region).map_err(io::Error::other)?;
    let credentials = Credentials::from_env();
    Ok(bucket
        .get_object(credentials.as_ref(), key)
        .sign(Duration::from_hours(1))
        .into())
}

fn spawn_file_readers<'scope>(scope: &'scope Scope<'scope, '_>, path: &Path) -> io::Result<Chunks> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;

    if !metadata.is_file() || is_gzip(&file)? {
        return Ok(spawn_stream_reader(scope, file));
    }

    let length = metadata.len();
    let mut receivers = Vec::with_capacity(PLAIN_READERS);
    for first in 0..PLAIN_READERS {
        let (sender, receiver) = mpsc::sync_channel(READ_AHEAD_CHUNKS / PLAIN_READERS);
        let file = file.try_clone()?;
        scope.spawn(move || send_every_nth_chunk(&file, length, first, &sender));
        receivers.push(receiver);
    }
    Ok(Chunks::new(receivers))
}

fn spawn_http_reader<'scope>(scope: &'scope Scope<'scope, '_>, url: &str) -> io::Result<Chunks> {
    let response = ureq::get(url).call().map_err(ureq::Error::into_io)?;
    Ok(spawn_stream_reader(
        scope,
        response.into_body().into_reader(),
    ))
}

fn spawn_stream_reader<'scope>(
    scope: &'scope Scope<'scope, '_>,
    stream: impl Read + Send + 'scope,
) -> Chunks {
    let (sender, receiver) = mpsc::sync_channel(READ_AHEAD_CHUNKS);
    scope.spawn(move || {
        let mut stream = BufReader::with_capacity(FILE_BUFFER_SIZE, stream);
        let mut magic = Vec::with_capacity(GZIP_MAGIC_BYTES.len());

        if let Err(error) = stream.by_ref().take(2).read_to_end(&mut magic) {
            let _ = sender.send(Err(error));
            return;
        }

        let gzip = magic == GZIP_MAGIC_BYTES;
        let stream = io::Cursor::new(magic).chain(stream);
        if gzip {
            send_all_chunks(GzipReader::new(stream), &sender);
        } else {
            send_all_chunks(stream, &sender);
        }
    });
    Chunks::new(vec![receiver])
}

fn send_every_nth_chunk(
    file: &File,
    length: u64,
    first: usize,
    sender: &SyncSender<io::Result<Vec<u8>>>,
) {
    let chunk_size = CHUNK_SIZE as u64;
    let mut offset = first as u64 * chunk_size;
    while offset < length {
        let size = usize::try_from(length - offset).map_or(CHUNK_SIZE, |rest| rest.min(CHUNK_SIZE));
        let mut chunk = vec![0; size];
        let result = file.read_exact_at(&mut chunk, offset).map(|()| chunk);
        let failed = result.is_err();
        // if send fails the validator has already stopped so no point reading any more
        if sender.send(result).is_err() || failed {
            return;
        }
        offset += PLAIN_READERS as u64 * chunk_size;
    }
}

fn send_all_chunks(mut reader: impl Read, sender: &SyncSender<io::Result<Vec<u8>>>) {
    loop {
        let mut chunk = Vec::with_capacity(CHUNK_SIZE);
        let result = reader
            .by_ref()
            .take(CHUNK_SIZE as u64)
            .read_to_end(&mut chunk);
        let full = chunk.len() == CHUNK_SIZE;
        if !chunk.is_empty() && sender.send(Ok(chunk)).is_err() {
            return;
        }
        if let Err(error) = result {
            let _ = sender.send(Err(error));
            return;
        }
        // if the chunk wasnt full then we hit the end of the input
        if !full {
            return;
        }
    }
}
