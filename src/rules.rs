use std::{fmt, slice};

use fearless_simd::Simd;
use serde::{Serialize, Serializer};
use sirocco::inflate;

use crate::output::InputLabel;
use crate::records::{Field, Record};
use crate::scan;
use crate::scan::{QualityEncoding, SequenceBytes};

/// Longest header the header-too-long rule accepts including the `@`.
const MAX_HEADER_LENGTH: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools)]
pub struct RecordRules {
    /// Turns `header-too-long` on or off.
    pub header_too_long: bool,
    /// Which bases are allowed for `invalid-sequence-character`.
    pub alphabet: SequenceBytes,
    /// Turns `lowercase-sequence` on or off.
    pub lowercase_sequence: bool,
    /// Which quality bytes are allowed for `invalid-quality-byte`.
    pub encoding: QualityEncoding,
    /// Min sequence length for `short-read` (0 turns it off).
    pub min_length: usize,
    /// Turns `sequence-all-same-byte` on or off.
    pub sequence_all_same_byte: bool,
    /// Turns `quality-all-same-byte` on or off.
    pub quality_all_same_byte: bool,
}

impl RecordRules {
    /// Checks a single record against all enabled rules.
    /// Returns the first rule that the record fails, if any.
    #[inline(always)]
    pub fn check<S: Simd>(&self, simd: S, record: &Record<'_>) -> Result<(), Rule> {
        let Record {
            header,
            sequence,
            separator,
            quality,
            ..
        } = *record;

        check_header_marker(header)?;

        if let Some(offset) = scan::find_invalid_header_byte(simd, header) {
            return Err(Rule::InvalidHeaderCharacter {
                offset,
                character: char::from(header[offset]),
            });
        }

        if Self::read_identifier_is_empty(header) {
            return Err(Rule::EmptyReadIdentifier {});
        }

        if !separator.starts_with(b"+") {
            return Err(Rule::InvalidSeparatorMarker {
                character: first_char(separator),
            });
        }

        check_separator_title(header, separator)?;

        let lowercase_offset = self.check_sequence(simd, sequence)?;

        if let Some(offset) = self.encoding.find_invalid(simd, quality) {
            return Err(Rule::InvalidQualityByte {
                encoding: self.encoding,
                offset,
                character: char::from(quality[offset]),
            });
        }

        if sequence.len() != quality.len() {
            return Err(Rule::SequenceQualityLengthMismatch {
                sequence_length: sequence.len(),
                quality_length: quality.len(),
            });
        }

        // optional rules below
        if self.header_too_long && header.len() > MAX_HEADER_LENGTH {
            return Err(Rule::HeaderTooLong {
                length: header.len(),
            });
        }

        if let Some(offset) = lowercase_offset {
            return Err(Rule::LowercaseSequence {
                offset,
                character: char::from(sequence[offset]),
            });
        }

        if sequence.len() < self.min_length {
            return Err(Rule::ShortRead {
                length: sequence.len(),
                min_length: self.min_length,
            });
        }

        if self.sequence_all_same_byte
            && let Some(byte) = Self::repeated_byte(sequence)
        {
            return Err(Rule::SequenceAllSameByte {
                character: char::from(byte),
                length: sequence.len(),
            });
        }

        if self.quality_all_same_byte
            && let Some(byte) = Self::repeated_byte(quality)
        {
            return Err(Rule::QualityAllSameByte {
                character: char::from(byte),
                length: quality.len(),
            });
        }

        Ok(())
    }

    /// Fails `invalid-sequence-character` or returns the offset of the first
    /// lowercase base
    #[inline(always)]
    fn check_sequence<S: Simd>(&self, simd: S, sequence: &[u8]) -> Result<Option<usize>, Rule> {
        let alphabet = self.alphabet;
        let first_attempt = if self.lowercase_sequence {
            alphabet.upper_case()
        } else {
            alphabet.any_case()
        };

        let Some(first_failure) = first_attempt.find_rejected(simd, sequence) else {
            return Ok(None);
        };

        // if failed then invalid reported over lowcase
        match alphabet.any_case().find_rejected(simd, sequence) {
            Some(offset) => Err(Rule::InvalidSequenceCharacter {
                alphabet,
                offset,
                character: char::from(sequence[offset]),
            }),
            None => Ok(Some(first_failure)),
        }
    }

    fn read_identifier_is_empty(header: &[u8]) -> bool {
        header
            .get(1)
            .is_none_or(|&byte| byte == b' ' || byte == b'\t')
    }

    fn repeated_byte(bytes: &[u8]) -> Option<u8> {
        // need at least 2 bytes for it to count as repeated - empty ones get caught by short-read anyway
        let [first, _, ..] = bytes else {
            return None;
        };
        // a normal sequence or quality line usually will have some sort of variation within
        // the first 8 bytes. If we check that those 8 bytes are the same all at once it is
        // very likely they wont be - which improves branch prediction over checking byte by byte
        // if we know those 8 bytes differ it passes the rule so can return
        if let Some(first_eight) = bytes.first_chunk::<8>()
            && *first_eight != [*first; 8]
        {
            return None;
        }
        // if they do happen to be all the same or its less than 8 bytes anyway then do the full check
        bytes.iter().all(|byte| byte == first).then_some(*first)
    }
}

/// Truncated records need their own check since `check` only works on full 4 line records.
pub fn truncated_final_record(header: &[u8], last_field: Field) -> Rule {
    // check does the truncation at least look like a record by looking for @
    // otherwise could be junk - in that situation truncated record might be misleading
    match check_header_marker(header) {
        Err(failure) => failure,
        Ok(()) => Rule::TruncatedFinalRecord { last_field },
    }
}

#[derive(Debug, Clone, Copy, Serialize, strum::Display, strum::EnumDiscriminants)]
#[serde(untagged, rename_all_fields = "kebab-case")]
#[strum_discriminants(
    name(RuleCode),
    derive(Serialize, strum::Display, strum::VariantArray),
    serde(rename_all = "kebab-case"),
    strum(serialize_all = "kebab-case")
)]
pub enum Rule {
    #[strum(to_string = "the input could not be decompressed: {error}.")]
    InputReadFailed {
        #[serde(serialize_with = "serialize_display")]
        error: inflate::Error,
    },
    #[strum(to_string = "the input contains no records.")]
    EmptyInput {},
    #[strum(to_string = "expected '@' at the start of the header, found {character:?}.")]
    InvalidHeaderMarker { character: char },
    #[strum(to_string = "the input ends after the {last_field} line of the last record.")]
    TruncatedFinalRecord { last_field: Field },
    #[strum(to_string = "{character:?} is not printable ASCII or a tab.")]
    InvalidHeaderCharacter {
        #[serde(skip)]
        offset: usize,
        character: char,
    },
    #[strum(to_string = "the header has no read identifier after '@'.")]
    EmptyReadIdentifier {},
    #[strum(to_string = "expected '+' at the start of the separator, found {character:?}.")]
    InvalidSeparatorMarker { character: char },
    #[strum(to_string = "the separator repeats a title that differs from the header.")]
    SeparatorHeaderMismatch {
        #[serde(skip)]
        offset: usize,
    },
    #[strum(to_string = "{character:?} is not in the {alphabet} alphabet.")]
    InvalidSequenceCharacter {
        alphabet: SequenceBytes,
        #[serde(skip)]
        offset: usize,
        character: char,
    },
    #[strum(to_string = "{character:?} is outside the {encoding}.")]
    InvalidQualityByte {
        encoding: QualityEncoding,
        #[serde(skip)]
        offset: usize,
        character: char,
    },
    #[strum(
        to_string = "the sequence has {sequence_length} bases but the quality has {quality_length} characters."
    )]
    SequenceQualityLengthMismatch {
        sequence_length: usize,
        quality_length: usize,
    },
    #[strum(to_string = "the header is {length} bytes, but the maximum is 255.")]
    HeaderTooLong { length: usize },
    #[strum(to_string = "{character:?} is lowercase.")]
    LowercaseSequence {
        #[serde(skip)]
        offset: usize,
        character: char,
    },
    #[strum(to_string = "the sequence has {length} bases, but the minimum is {min_length}.")]
    ShortRead { length: usize, min_length: usize },
    #[strum(to_string = "all {length} bases are {character:?}.")]
    SequenceAllSameByte { character: char, length: usize },
    #[strum(to_string = "all {length} qualities are {character:?}.")]
    QualityAllSameByte { character: char, length: usize },
    #[strum(to_string = "{reason}")]
    MateMismatch { reason: MateMismatchReason },
    #[strum(to_string = "this read has no mate in {missing_mate_input}.")]
    Singleton { missing_mate_input: InputLabel },
}

impl Rule {
    pub fn code(&self) -> RuleCode {
        RuleCode::from(self)
    }

    pub fn hint(&self) -> Option<String> {
        match *self {
            Self::InvalidSequenceCharacter {
                alphabet: SequenceBytes::Standard,
                character,
                ..
            } if u8::try_from(character)
                .is_ok_and(|byte| SequenceBytes::Iupac.any_case().contains(byte)) =>
            {
                Some("if the sequences use IUPAC codes, pass --alphabet iupac".to_owned())
            }
            Self::InvalidHeaderCharacter {
                // flags any rogue windows line endings remaining after being stripped
                character: '\r',
                ..
            } => {
                Some("a carriage return inside a line usually means broken line endings".to_owned())
            }
            Self::ShortRead { length: 0, .. } => {
                Some("if empty reads are expected, pass --min-length 0".to_owned())
            }
            Self::ShortRead { .. } => {
                Some("if shorter reads are expected, lower --min-length".to_owned())
            }
            _ => self
                .code()
                .info()
                .opt_out
                .map(|flag| format!("if this is expected, pass {flag}")),
        }
    }

    pub fn column(&self) -> Option<usize> {
        match *self {
            Self::InvalidHeaderCharacter { offset, .. }
            | Self::SeparatorHeaderMismatch { offset }
            | Self::InvalidSequenceCharacter { offset, .. }
            | Self::InvalidQualityByte { offset, .. }
            | Self::LowercaseSequence { offset, .. } => Some(offset + 1),
            _ => None,
        }
    }

    pub fn records_are_readable(&self) -> bool {
        !matches!(self, Self::InputReadFailed { .. })
    }

    pub fn fields(&self) -> &[Field] {
        match self {
            // truncated record only goes up to the last line that was there so point at that
            Self::TruncatedFinalRecord { last_field } => slice::from_ref(last_field),
            Self::InvalidHeaderMarker { .. }
            | Self::InvalidHeaderCharacter { .. }
            | Self::EmptyReadIdentifier {}
            | Self::HeaderTooLong { .. } => &[Field::Header],
            Self::InvalidSequenceCharacter { .. }
            | Self::LowercaseSequence { .. }
            | Self::ShortRead { .. }
            | Self::SequenceAllSameByte { .. } => &[Field::Sequence],
            Self::InvalidSeparatorMarker { .. } => &[Field::Separator],
            Self::SeparatorHeaderMismatch { .. } => &[Field::Header, Field::Separator],
            Self::InvalidQualityByte { .. } | Self::QualityAllSameByte { .. } => &[Field::Quality],
            Self::SequenceQualityLengthMismatch { .. } => &[Field::Sequence, Field::Quality],
            Self::InputReadFailed { .. }
            | Self::EmptyInput {}
            | Self::MateMismatch { .. }
            | Self::Singleton { .. } => &[],
        }
    }
}

impl RuleCode {
    pub fn info(self) -> RuleInfo {
        let (scope, message, opt_out) = match self {
            Self::InputReadFailed => (
                Scope::Input,
                "FASTQ could not be read to the end of the file.",
                None,
            ),
            Self::EmptyInput => (Scope::Input, "FASTQ contains no records.", None),
            Self::InvalidHeaderMarker => {
                (Scope::Record, "Header line does not start with '@'.", None)
            }
            Self::TruncatedFinalRecord => (
                Scope::Record,
                "FASTQ ends before a complete record can be formed.",
                None,
            ),
            Self::InvalidHeaderCharacter => (
                Scope::Record,
                "Header contains a byte that is not printable ASCII.",
                None,
            ),
            Self::EmptyReadIdentifier => (
                Scope::Record,
                "Header has no read identifier after '@'.",
                None,
            ),
            Self::InvalidSeparatorMarker => (
                Scope::Record,
                "Separator line does not start with '+'.",
                None,
            ),
            Self::SeparatorHeaderMismatch => (
                Scope::Record,
                "Separator contains a header that differs from the main header.",
                None,
            ),
            Self::InvalidSequenceCharacter => (
                Scope::Record,
                "Sequence contains a character outside the allowed alphabet.",
                None,
            ),
            Self::InvalidQualityByte => (
                Scope::Record,
                "Quality contains a character outside the encoding range.",
                None,
            ),
            Self::SequenceQualityLengthMismatch => {
                (Scope::Record, "Sequence and quality lengths differ.", None)
            }
            Self::HeaderTooLong => (
                Scope::Record,
                "Header is longer than 255 bytes.",
                Some("--allow header-too-long"),
            ),
            Self::LowercaseSequence => (
                Scope::Record,
                "Sequence contains lowercase bases.",
                Some("--allow lowercase-sequence"),
            ),
            Self::ShortRead => (
                Scope::Record,
                "Sequence is shorter than the minimum read length.",
                Some("--min-length 0"),
            ),
            Self::SequenceAllSameByte => (
                Scope::Record,
                "Sequence consists of one repeated base.",
                Some("--allow sequence-all-same-byte"),
            ),
            Self::QualityAllSameByte => (
                Scope::Record,
                "Quality string consists of one repeated character.",
                Some("--allow quality-all-same-byte"),
            ),
            Self::MateMismatch => (
                Scope::Pair,
                "Paired reads do not have the same read identifier.",
                None,
            ),
            Self::Singleton => (Scope::Pair, "Read has no mate in the other input.", None),
        };
        RuleInfo {
            code: self,
            scope,
            message,
            opt_out,
        }
    }
}

#[derive(Debug, Serialize, tabled::Tabled)]
#[serde(rename_all = "kebab-case")]
#[tabled(rename_all = "UPPERCASE")]
pub struct RuleInfo {
    #[tabled(rename = "RULE")]
    pub code: RuleCode,
    pub scope: Scope,
    #[tabled(skip)]
    pub message: &'static str,
    #[tabled(
        rename = "OPTIONAL",
        format("{}", if self.opt_out.is_some() { "yes" } else { "no" })
    )]
    opt_out: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, strum::Display)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum Scope {
    Input,
    Record,
    Pair,
}

/// Returns the read name without its leading `@` or description.
///
/// Only call this after `invalid-header-marker` has passed, otherwise the `@` might not be there.
pub fn read_identifier(header: &[u8]) -> &[u8] {
    let name = &header[1..];
    let end = memchr::memchr2(b' ', b'\t', name).unwrap_or(name.len());
    &name[..end]
}

/// Fails when the two mates read identifiers arent correct (i.e the /1 /2)
/// The read ids are compared first, so different ids are always an identifier
/// mismatch first.
pub fn mate_mismatch(r1: &[u8], r2: &[u8]) -> Option<MateMismatchReason> {
    let (r1_base, r1_mate) = split_mate(r1);
    let (r2_base, r2_mate) = split_mate(r2);
    if r1_base != r2_base {
        return Some(MateMismatchReason::IdentifierMismatch);
    }
    match (r1_mate, r2_mate) {
        (None, None) | (Some(b'1'), Some(b'2')) => None,
        (Some(_), Some(_)) => Some(MateMismatchReason::MateNumberMismatch),
        (Some(_), None) | (None, Some(_)) => Some(MateMismatchReason::MateNumberMissing),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, strum::Display)]
#[serde(rename_all = "kebab-case")]
pub enum MateMismatchReason {
    #[strum(to_string = "the mates' read identifiers differ.")]
    IdentifierMismatch,
    #[strum(to_string = "the mates' /1 and /2 suffixes don't match.")]
    MateNumberMismatch,
    #[strum(to_string = "only one mate has a /1 or /2 suffix.")]
    MateNumberMissing,
}

/// Fails when the header doesn't start with `@`.
fn check_header_marker(header: &[u8]) -> Result<(), Rule> {
    if header.starts_with(b"@") {
        Ok(())
    } else {
        Err(Rule::InvalidHeaderMarker {
            character: first_char(header),
        })
    }
}

/// Fails when the separator contains a header and it differs from the main header.
///
/// Trailing whitespace are ignored on both, full check doesnt run if no separator header.
#[inline(always)]
fn check_separator_title(header: &[u8], separator: &[u8]) -> Result<(), Rule> {
    // both markers have passed already at this point so we defo have the @ and + chars to drop
    let actual_header = &header[1..];
    let separator_header = &separator[1..];
    // most separators are a lone '+' - if it has the repeated header, it should be an exact copy when valid
    if separator_header.is_empty() || separator_header == actual_header {
        return Ok(());
    }
    // if it isnt check without trailing whitespace
    let title = trim_title(separator_header);
    let header_title = trim_title(actual_header);
    if title.is_empty() || title == header_title {
        return Ok(());
    }
    // otherwise they are wrong - find where for report
    let title_offset = title
        .iter()
        .zip(header_title)
        .position(|(title_byte, header_byte)| title_byte != header_byte)
        .unwrap_or_else(|| title.len().min(header_title.len()));
    Err(Rule::SeparatorHeaderMismatch {
        offset: title_offset + 1,
    })
}

fn trim_title(mut title: &[u8]) -> &[u8] {
    while let [rest @ .., b' ' | b'\t'] = title {
        title = rest;
    }
    title
}

fn first_char(line: &[u8]) -> char {
    char::from(line.first().copied().unwrap_or(b'\n'))
}

/// Splits a `/1` or `/2` from the read id.
fn split_mate(identifier: &[u8]) -> (&[u8], Option<u8>) {
    match identifier {
        [base @ .., b'/', mate @ (b'1' | b'2')] => (base, Some(*mate)),
        _ => (identifier, None),
    }
}

/// Serializes a field as its `Display` text.
fn serialize_display<S: Serializer>(
    value: &impl fmt::Display,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}
