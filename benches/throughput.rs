use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use amro::{InputType, QualityEncoding, RecordRules, SequenceBytes, Source};
use criterion::measurement::WallTime;
use criterion::{BenchmarkGroup, Criterion, Throughput};
use rand::SeedableRng;
use rand::rngs::StdRng;

const FASTQ_BYTES: u64 = 1024 * 1024 * 1024;

/// cli default
const RULES: RecordRules = RecordRules {
    header_too_long: true,
    alphabet: SequenceBytes::Standard,
    lowercase_sequence: true,
    encoding: QualityEncoding::Phred33,
    min_length: 1,
    sequence_all_same_byte: true,
    quality_all_same_byte: true,
};

fn throughput(criterion: &mut Criterion) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let mut group = criterion.benchmark_group("throughput");
    group.sample_size(10);

    for read_length in [10_000, 2_000] {
        let path = directory
            .path()
            .join(format!("nanopore_{read_length}bp.fastq"));
        let bytes = write_fastq(&path, read_length, FASTQ_BYTES, None).expect("FASTQ written");
        bench_run(
            &mut group,
            &format!("nanopore/single/{read_length}bp"),
            bytes,
            &InputType::Single(Source::Local(path)),
        );
    }

    let r1 = directory.path().join("illumina_R1.fastq");
    let r2 = directory.path().join("illumina_R2.fastq");
    let bytes = write_fastq(&r1, 150, FASTQ_BYTES / 2, Some(1)).expect("R1 written")
        + write_fastq(&r2, 150, FASTQ_BYTES / 2, Some(2)).expect("R2 written");
    bench_run(
        &mut group,
        "illumina/paired/150bp",
        bytes,
        &InputType::Paired {
            r1: Source::Local(r1),
            r2: Source::Local(r2),
        },
    );

    group.finish();
}

fn bench_run(group: &mut BenchmarkGroup<'_, WallTime>, name: &str, bytes: u64, layout: &InputType) {
    assert!(
        amro::run(layout, &RULES)
            .expect("the run finishes")
            .is_none()
    );
    group.throughput(Throughput::BytesDecimal(bytes));
    group.bench_function(name, |bencher| {
        bencher.iter(|| amro::run(layout, &RULES).expect("the run finishes"));
    });
}

fn write_fastq(path: &Path, read_length: usize, target: u64, mate: Option<u8>) -> io::Result<u64> {
    use rand::Rng;

    let mut random = StdRng::seed_from_u64(1);
    let mut file = BufWriter::new(File::create(path)?);
    let mut sequence = vec![0; read_length];
    let mut quality = vec![0; read_length];
    let mut written = 0;
    let mut number = 0;
    while written < target {
        number += 1;
        random.fill_bytes(&mut sequence);
        random.fill_bytes(&mut quality);
        for (base, score) in sequence.iter_mut().zip(&mut quality) {
            *base = b"ACGT"[usize::from(*base & 3)];
            *score = b'!' + *score % 41;
        }
        let header = match mate {
            Some(mate) => format!("@read{number}/{mate}\n"),
            None => format!("@read{number}\n"),
        };
        file.write_all(header.as_bytes())?;
        file.write_all(&sequence)?;
        file.write_all(b"\n+\n")?;
        file.write_all(&quality)?;
        file.write_all(b"\n")?;
        written += (header.len() + 2 * read_length + 4) as u64;
    }
    file.flush()?;
    Ok(written)
}

criterion::criterion_group!(benches, throughput);
criterion::criterion_main!(benches);
