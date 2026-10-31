use std::mem;
use std::ops::{ControlFlow, Index};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, Scope};

use color_eyre::eyre::WrapErr;
use fearless_simd::{Level, Simd};
use sirocco::inflate;

use crate::cli::InputType;
use crate::input::{self, Chunks, Source};
use crate::output::{Failure, InputLabel, RecordAt};
use crate::records::{Leftover, Record, Records};
use crate::rules::{self, RecordRules, Rule};

/// How many batches of read ids R1's validator can get ahead of R2's (mpsc bound).
const MAX_ALLOWED_READ_AHEAD_BATCHES: usize = 4;

/// How many R1 read ids the validator collects before sending them to R2's validator.
const READ_IDS_PER_BATCH: usize = 1024;

type Outcome = color_eyre::Result<Option<Failure>>;

pub fn run(layout: &InputType, rules: &RecordRules) -> Outcome {
    // each input gets its own reader threads depending on format: 4 (PLAIN_READERS) for a
    // plain file, one for gzip since it can only be decompressed in order
    // single and interleaved files are validated on the main thread, R1 of a paired file
    // gets its own validator thread which sends its read ids to R2's validator on the
    // main thread for the mate check
    thread::scope(|scope| match layout {
        InputType::Single(source) => {
            let chunks = spawn_reader(scope, source)?;
            validate(chunks, rules, InputLabel::R1, source, MatePairing::None)
        }
        InputType::Interleaved(source) => {
            let chunks = spawn_reader(scope, source)?;
            let mates = MatePairing::Interleaved {
                previous: Vec::new(),
            };
            validate(chunks, rules, InputLabel::Interleaved, source, mates)
        }
        InputType::Paired { r1, r2 } => {
            let r1_chunks = spawn_reader(scope, r1)?;
            let r2_chunks = spawn_reader(scope, r2)?;
            let (sender, receiver) = mpsc::sync_channel(MAX_ALLOWED_READ_AHEAD_BATCHES);
            scope.spawn(move || {
                let mates = MatePairing::Sender {
                    messages: sender,
                    batch: Identifiers::default(),
                };
                validate(r1_chunks, rules, InputLabel::R1, r1, mates)
            });
            let mates = MatePairing::Receiver(FromR1::new(receiver));
            validate(r2_chunks, rules, InputLabel::R2, r2, mates)
        }
    })
}

/// How the validator handles each records mate
///
/// Single end files have no mates but still need to be  included as None to share
/// the validation loop
enum MatePairing {
    /// Single records have no mates.
    None,
    /// Interleaved - odd/even (records are 1 based - odd waits for even record)
    Interleaved { previous: Vec<u8> },
    /// R1 of paired file - spawned on own thread sending to main (R2)
    Sender {
        messages: SyncSender<Message>,
        batch: Identifiers,
    },
    /// R2 of paired file - recieves R1 read ids
    Receiver(FromR1),
}

impl MatePairing {
    /// Runs after a record passes all other records rules
    #[inline(always)]
    fn run_pair_rules(&mut self, input: InputLabel, record: &Record<'_>) -> ControlFlow<Outcome> {
        match self {
            Self::None => {}
            Self::Interleaved { previous } => {
                let identifier = rules::read_identifier(record.header);
                // odd = R1, even = R2
                if !record.number.is_multiple_of(2) {
                    previous.clear();
                    previous.extend_from_slice(identifier);
                } else if let Some(reason) = rules::mate_mismatch(previous, identifier) {
                    return ControlFlow::Break(Ok(Some(Failure::in_mates(
                        Rule::MateMismatch { reason },
                        &[
                            (RecordAt::new(input, record.number - 1), previous),
                            (RecordAt::new(input, record.number), identifier),
                        ],
                    ))));
                }
            }
            Self::Sender { messages, batch } => {
                batch.push(rules::read_identifier(record.header));
                if batch.len() == READ_IDS_PER_BATCH
                    && messages
                        .send(Message::Identifiers(mem::take(batch)))
                        .is_err()
                {
                    return ControlFlow::Break(Ok(None));
                }
            }
            Self::Receiver(from_r1) => {
                let r2_identifier = rules::read_identifier(record.header);
                let Some(r1_identifier) = from_r1.take()? else {
                    return ControlFlow::Break(Ok(Some(Failure::in_mates(
                        Rule::Singleton {
                            missing_mate_input: InputLabel::R1,
                        },
                        &[(RecordAt::new(input, record.number), r2_identifier)],
                    ))));
                };
                if let Some(reason) = rules::mate_mismatch(r1_identifier, r2_identifier) {
                    return ControlFlow::Break(Ok(Some(Failure::in_mates(
                        Rule::MateMismatch { reason },
                        &[
                            (RecordAt::new(InputLabel::R1, record.number), r1_identifier),
                            (RecordAt::new(input, record.number), r2_identifier),
                        ],
                    ))));
                }
            }
        }
        ControlFlow::Continue(())
    }

    fn finish(self, input: InputLabel, record_number: u64, outcome: Outcome) -> Outcome {
        match self {
            Self::None => outcome,
            Self::Interleaved { previous } => {
                // record_number is the next record so if its even we read an odd number of records,
                // meaning the last R1 never got its R2
                if matches!(outcome, Ok(None)) && record_number.is_multiple_of(2) {
                    return Ok(Some(Failure::in_mates(
                        Rule::Singleton {
                            missing_mate_input: InputLabel::Interleaved,
                        },
                        &[(RecordAt::new(input, record_number - 1), &previous)],
                    )));
                }
                outcome
            }
            Self::Sender { messages, batch } => {
                let _ = messages.send(Message::Identifiers(batch));
                let _ = messages.send(Message::End(outcome));
                Ok(None)
            }
            Self::Receiver(from_r1) => from_r1.finish(input, record_number, outcome),
        }
    }
}

/// Validation of R1 reads
struct FromR1 {
    messages: Receiver<Message>,
    batch: Identifiers,
    /// Index of the next read id in the batch.
    next: usize,
    /// How many R1 records validator has asked for.
    taken: u64,
}

impl FromR1 {
    fn new(messages: Receiver<Message>) -> Self {
        Self {
            messages,
            batch: Identifiers::default(),
            next: 0,
            taken: 0,
        }
    }

    fn take(&mut self) -> ControlFlow<Outcome, Option<&[u8]>> {
        self.taken += 1;
        while self.next == self.batch.len() {
            match self
                .messages
                .recv()
                .expect("R1's validator always sends its end")
            {
                Message::Identifiers(batch) => {
                    self.batch = batch;
                    self.next = 0;
                }
                Message::End(Ok(None)) => return ControlFlow::Continue(None),
                Message::End(outcome) => return ControlFlow::Break(outcome),
            }
        }
        self.next += 1;
        ControlFlow::Continue(Some(&self.batch[self.next - 1]))
    }

    fn finish(mut self, input: InputLabel, record_number: u64, outcome: Outcome) -> Outcome {
        // if R2 stopped before getting to the mate check we havent taken its R1 record yet
        // so take it here
        if self.taken < record_number {
            match self.take() {
                ControlFlow::Break(r1_outcome) => return r1_outcome,
                ControlFlow::Continue(Some(r1_identifier)) if matches!(outcome, Ok(None)) => {
                    return Ok(Some(Failure::in_mates(
                        Rule::Singleton {
                            missing_mate_input: input,
                        },
                        &[(RecordAt::new(InputLabel::R1, record_number), r1_identifier)],
                    )));
                }
                ControlFlow::Continue(_) => {}
            }
        }
        outcome
    }
}

/// A batch of R1 read ids sent to R2's validator.
///
/// The ids are stored in one buffer instead of a `Vec<Vec<u8>>`, which
/// is very slow.
#[derive(Default)]
struct Identifiers {
    /// Back to back buffer of reads ids
    bytes: Vec<u8>,
    /// Where each id ends in `bytes`
    ends: Vec<usize>,
}

impl Identifiers {
    fn push(&mut self, identifier: &[u8]) {
        self.bytes.extend_from_slice(identifier);
        self.ends.push(self.bytes.len());
    }

    fn len(&self) -> usize {
        self.ends.len()
    }
}

impl Index<usize> for Identifiers {
    type Output = [u8];

    fn index(&self, index: usize) -> &[u8] {
        let start = index
            .checked_sub(1)
            .map_or(0, |previous| self.ends[previous]);
        &self.bytes[start..self.ends[index]]
    }
}

/// R1 validator message sent to R2.
enum Message {
    /// Next set of read ids to mate check
    Identifiers(Identifiers),
    // R1 is done
    End(Outcome),
}

fn spawn_reader<'scope>(
    scope: &'scope Scope<'scope, '_>,
    source: &Source,
) -> color_eyre::Result<Chunks> {
    input::spawn_readers(scope, source).wrap_err_with(|| context(source))
}

/// Validates one input's records in order, stopping at the first failure.
fn validate(
    chunks: Chunks,
    rules: &RecordRules,
    input: InputLabel,
    source: &Source,
    mut mates: MatePairing,
) -> Outcome {
    // get the SIMD level once and dispatch per chunk instead of per record so the whole
    // record loop gets compiled for that level
    let level = Level::new();

    let failed = |failure: Failure| Ok(Some(failure));

    let mut records = Records::default();
    for chunk in chunks {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let record_number = records.next_record_number();
                let decode_error: Option<&inflate::Error> =
                    error.get_ref().and_then(|source| source.downcast_ref());
                let outcome = match decode_error {
                    Some(&error) => failed(Failure::in_record(
                        RecordAt::new(input, record_number),
                        Rule::InputReadFailed { error },
                    )),
                    None => Err(error).wrap_err_with(|| context(source)),
                };
                return mates.finish(input, record_number, outcome);
            }
        };
        let pushed = fearless_simd::dispatch!(level, simd => records.push(
            &chunk,
            #[inline(always)]
            |record| validate_record(simd, rules, input, &mut mates, &record),
        ));
        if let ControlFlow::Break(outcome) = pushed {
            return mates.finish(input, records.next_record_number(), outcome);
        }
    }

    let finished = fearless_simd::dispatch!(level, simd => records.finish(
        #[inline(always)]
        |record| validate_record(simd, rules, input, &mut mates, &record),
    ));
    let outcome = match finished {
        ControlFlow::Break(outcome) => outcome,
        ControlFlow::Continue(Leftover::Empty) => {
            failed(Failure::in_input(input, Rule::EmptyInput {}))
        }
        ControlFlow::Continue(Leftover::Clean) => Ok(None),
        ControlFlow::Continue(Leftover::Truncated { header, last_field }) => {
            let rule = rules::truncated_final_record(header, last_field);
            failed(Failure::in_record(
                RecordAt::new(input, records.next_record_number()),
                rule,
            ))
        }
    };
    mates.finish(input, records.next_record_number(), outcome)
}

#[inline(always)]
fn validate_record<S: Simd>(
    simd: S,
    rules: &RecordRules,
    input: InputLabel,
    mates: &mut MatePairing,
    record: &Record<'_>,
) -> ControlFlow<Outcome> {
    if let Err(rule) = rules.check(simd, record) {
        return ControlFlow::Break(Ok(Some(Failure::in_record(
            RecordAt::new(input, record.number),
            rule,
        ))));
    }
    mates.run_pair_rules(input, record)
}

fn context(source: &Source) -> String {
    format!("could not read {source}")
}
