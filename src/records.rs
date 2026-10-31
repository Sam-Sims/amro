use std::ops::ControlFlow;

use serde::Serialize;

#[derive(Default)]
pub struct Records {
    /// Complete records found so far.
    count: u64,
    /// The start of a record that is in the next chunk.
    carry: Vec<u8>,
    /// Number of newlines in the carry (0-3).
    carry_lines: usize,
}

impl Records {
    /// Splits a chunk of FASTQ bytes into complete records and passes each to `on_record`.
    ///
    /// An incomplete record at the end is carried over and completed by the next chunk.
    #[inline(always)]
    pub fn push<B>(
        &mut self,
        chunk: &[u8],
        mut on_record: impl FnMut(Record<'_>) -> ControlFlow<B>,
    ) -> ControlFlow<B> {
        let mut chunk = chunk;
        if !self.carry.is_empty() {
            // how many more newlines from this chunk do we need to complete the previous
            // incomplete record
            let missing = 4 - self.carry_lines;
            // the chunk may or may not contain the rest of the record
            // e.g very long ONT reads
            let Some(record_end) = memchr::memchr_iter(b'\n', chunk).nth(missing - 1) else {
                // if it doesnt, absorb the whole chunk and continue to next chunk
                self.carry_lines += memchr::memchr_iter(b'\n', chunk).count();
                self.carry.extend_from_slice(chunk);
                return ControlFlow::Continue(());
            };
            // we found the complete record
            self.carry.extend_from_slice(&chunk[..=record_end]);
            search_for_records(&self.carry, &mut self.count, &mut on_record)?;
            self.carry.clear();
            // remove the bytes that completed the previous record
            chunk = &chunk[record_end + 1..];
        }
        let consumed = search_for_records(chunk, &mut self.count, &mut on_record)?;
        // any leftover bytes that do not contain a full record
        let leftover = &chunk[consumed..];
        // carry will always be empty here so record how many lines we need from the next chunk
        self.carry_lines = memchr::memchr_iter(b'\n', leftover).count();
        self.carry.extend_from_slice(leftover);
        ControlFlow::Continue(())
    }

    pub fn next_record_number(&self) -> u64 {
        self.count + 1
    }

    /// Called once all chunks have been pushed to deal with whatever is left in the carry.
    ///
    /// If the file doesnt end with a newline the last record still gets checked as normal.
    #[inline(always)]
    pub fn finish<B>(
        &mut self,
        mut on_record: impl FnMut(Record<'_>) -> ControlFlow<B>,
    ) -> ControlFlow<B, Leftover<'_>> {
        if self.carry.is_empty() {
            return ControlFlow::Continue(if self.count == 0 {
                Leftover::Empty
            } else {
                Leftover::Clean
            });
        }
        if !self.carry.ends_with(b"\n") {
            self.carry.push(b'\n');
        }
        // carry is only ever part of one record (max 3 lines) so even with the newline added
        // there is at most one record to check
        let consumed = search_for_records(&self.carry, &mut self.count, &mut on_record)?;
        if consumed == self.carry.len() {
            return ControlFlow::Continue(Leftover::Clean);
        }
        let header_end = memchr::memchr(b'\n', &self.carry).expect("the carry ends with a newline");
        // if we got here the record is incomplete so it only has 1-3 lines - meaning the last
        // line we have can never be the quality
        let lines = memchr::memchr_iter(b'\n', &self.carry).count();
        ControlFlow::Continue(Leftover::Truncated {
            header: line(&self.carry[..header_end]),
            last_field: [Field::Header, Field::Sequence, Field::Separator][lines - 1],
        })
    }
}

/// A FASTQ record (set of 4 lines) which is borrowed from the underlying bytes.
///
/// Every line has its line ending removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record<'bytes> {
    /// 1-based record number from the input.
    pub number: u64,
    /// FASTQ header, including the description.
    pub header: &'bytes [u8],
    pub sequence: &'bytes [u8],
    pub separator: &'bytes [u8],
    pub quality: &'bytes [u8],
}

/// What is left once input has been read to the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leftover<'bytes> {
    /// Input had 0 bytes.
    Empty,
    /// The input ended after a complete record.
    Clean,
    /// The input ended partway through its final record. The header is still checked so
    /// junk or blank lines at the end of the file doesnt get reported as a truncated record.
    Truncated {
        header: &'bytes [u8],
        last_field: Field,
    },
}

/// The 4 lines of a FASTQ record - the value is how many lines after the header it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, strum::Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum Field {
    Header = 0,
    Sequence = 1,
    Separator = 2,
    Quality = 3,
}

impl Field {
    /// The 1-based line number of this field in record `record_number`.
    pub fn line(self, record_number: u64) -> u64 {
        4 * (record_number - 1) + 1 + self as u64
    }
}

/// Finds complete FASTQ records in `bytes` and passes each to `on_record`.
///
/// Returns how many bytes the complete records used. Anything after that is a partial record
/// to carry over to the next chunk.
///
/// Searching makes some optimisations assuming that valid fastq data is going to be the most common case.
#[inline(always)]
fn search_for_records<'bytes, B>(
    bytes: &'bytes [u8],
    count: &mut u64,
    on_record: &mut impl FnMut(Record<'bytes>) -> ControlFlow<B>,
) -> ControlFlow<B, usize> {
    let mut start = 0;

    while let Some(header_end) = find_newline(bytes, start)
        && let Some(sequence_end) = find_newline(bytes, header_end + 1)
    {
        // a valid fastq usually only has the single + separator so we can check for that
        // instead of running memchr for the new line
        // if the header has been repeated on the separator the memchr path runs instead
        let separator_end = if bytes.get(sequence_end + 1..sequence_end + 3) == Some(b"+\n") {
            sequence_end + 2
        } else {
            let Some(end) = find_newline(bytes, sequence_end + 1) else {
                break;
            };
            end
        };

        // a valid fastq quality line should be same length as the sequence line
        // so we can check for the newline at the same position in quality as sequence
        // instead of using memchr
        let expected_end = separator_end + (sequence_end - header_end);
        let mut quality_end = if bytes.get(expected_end) == Some(&b'\n') {
            expected_end
        } else {
            let Some(end) = find_newline(bytes, separator_end + 1) else {
                break;
            };
            end
        };
        // if the record fails it is not safe to assume that the newline found at the end of a quality line
        // was the actual end of the quality line, for example if the quality line was shorter by exactly the
        // length of the next line. In this case we scan again without the shortcut to find real end.
        while let ControlFlow::Break(outcome) = on_record(Record {
            number: *count + 1,
            header: line(&bytes[start..header_end]),
            sequence: line(&bytes[header_end + 1..sequence_end]),
            separator: line(&bytes[sequence_end + 1..separator_end]),
            quality: line(&bytes[separator_end + 1..quality_end]),
        }) {
            let Some(scanned_end) =
                find_newline(bytes, separator_end + 1).filter(|&end| end < quality_end)
            else {
                return ControlFlow::Break(outcome);
            };
            quality_end = scanned_end;
        }
        *count += 1;
        start = quality_end + 1;
    }
    ControlFlow::Continue(start)
}

/// Returns the offset of the first newline in `bytes` at or after `from`.
fn find_newline(bytes: &[u8], from: usize) -> Option<usize> {
    memchr::memchr(b'\n', &bytes[from..]).map(|offset| from + offset)
}

/// Returns a line stripped of a windows `\r` line ending.
fn line(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}
