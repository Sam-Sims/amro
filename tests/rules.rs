use std::fs;
use std::path::PathBuf;

use amro::{InputType, OutputMode, QualityEncoding, RecordRules, SequenceBytes, Source};
use rstest::rstest;
use serde_json::{Value, json};

const BROKEN_GZIP: &[u8] = b"\x1f\x8bjunk";
const LONG_SEQUENCE: &str =
    "ACGTNACGTNACGTNACGTNACGTNACGTNACGTNACGTNACGTNACGTNACGTNACGTNACGTNACGTN";
const LONG_QUALITY: &str = "IJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJIJ";
const LONG_HEADER: &str = "@M00123:57:000000000-ABCDE:1:1101:1000:1000 1:N:0:ACGTACGT+TGCATGCA";

#[rstest]
#[case::single(single, &["@r\nACGT\n+\nIIJJ\n"], default_rules())]
#[case::paired(paired, &["@p/1\nACGT\n+\nIIJJ\n", "@p/2\nACGT\n+\nIIJJ\n"], default_rules())]
#[case::interleaved(
    interleaved,
    &["@p/1\nACGT\n+\nIIJJ\n@p/2\nACGT\n+\nIIJJ\n"],
    default_rules()
)]
#[case::crlf_line_endings(single, &["@r\r\nACGT\r\n+\r\nIIJJ\r\n"], default_rules())]
#[case::no_final_newline(single, &["@r\nACGT\n+\nIIJJ"], default_rules())]
#[case::header_description(single, &["@r 1:N:0:ACGT\nACGT\n+\nIIJJ\n"], default_rules())]
#[case::tab_in_header(single, &["@r\tRG:Z:x\nACGT\n+\nIIJJ\n"], default_rules())]
#[case::separator_repeats_title(single, &["@r d\nACGT\n+r d\nIIJJ\n"], default_rules())]
#[case::separator_title_with_trailing_whitespace(
    single,
    &["@r\nACGT\n+r \t\nIIJJ\n"],
    default_rules()
)]
#[case::header_title_with_trailing_whitespace(
    single,
    &["@r \nACGT\n+r\nIIJJ\n"],
    default_rules()
)]
#[case::separator_with_only_whitespace(single, &["@r\nACGT\n+ \t\nIIJJ\n"], default_rules())]
#[case::separator_repeats_title_with_crlf(
    single,
    &["@r\r\nACGT\r\n+r\r\nIIJJ\r\n"],
    default_rules()
)]
#[case::allow_lowercase_sequence(
    single,
    &["@r\nACgT\n+\nIIJJ\n"],
    RecordRules { lowercase_sequence: false, ..default_rules() }
)]
#[case::min_length_zero_allows_empty_read(
    single,
    &["@r\n\n+\n\n"],
    RecordRules { min_length: 0, ..default_rules() }
)]
#[case::allow_sequence_all_same_byte(
    single,
    &["@r\nAAAA\n+\nIIJJ\n"],
    RecordRules { sequence_all_same_byte: false, ..default_rules() }
)]
#[case::allow_quality_all_same_byte(
    single,
    &["@r\nACGT\n+\nIIII\n"],
    RecordRules { quality_all_same_byte: false, ..default_rules() }
)]
#[case::iupac_alphabet(
    single,
    &["@r\nACRT\n+\nIIJJ\n"],
    RecordRules { alphabet: SequenceBytes::Iupac, ..default_rules() }
)]
#[case::phred64_encoding(
    single,
    &["@r\nACGT\n+\n@@AB\n"],
    RecordRules { encoding: QualityEncoding::Phred64, ..default_rules() }
)]
fn run_works(
    #[case] layout: fn(&[PathBuf]) -> InputType,
    #[case] files: &[&str],
    #[case] rules: RecordRules,
) {
    assert_eq!(failure_code(layout, &bytes(files), rules), None);
}

#[rstest]
#[case::empty_input(single, &[""], "empty-input")]
#[case::invalid_header_marker(single, &["r\nACGT\n+\nIIJJ\n"], "invalid-header-marker")]
#[case::truncated_final_record(single, &["@r\nACGT\n+\n"], "truncated-final-record")]
#[case::invalid_header_character(
    single,
    &["@r\x01\nACGT\n+\nIIJJ\n"],
    "invalid-header-character"
)]
#[case::carriage_return_in_header(
    single,
    &["@r\rx\nACGT\n+\nIIJJ\n"],
    "invalid-header-character"
)]
#[case::doubled_carriage_return(
    single,
    &["@r\r\r\nACGT\n+\nIIJJ\n"],
    "invalid-header-character"
)]
#[case::delete_in_header(single, &["@r\x7f\nACGT\n+\nIIJJ\n"], "invalid-header-character")]
#[case::non_ascii_description(
    single,
    &["@r caf\u{e9}\nACGT\n+\nIIJJ\n"],
    "invalid-header-character"
)]
#[case::invalid_header_character_before_empty_read_identifier(
    single,
    &["@ \x01\nACGT\n+\nIIJJ\n"],
    "invalid-header-character"
)]
#[case::empty_read_identifier(single, &["@\nACGT\n+\nIIJJ\n"], "empty-read-identifier")]
#[case::invalid_separator_marker(
    single,
    &["@r\nACGT\n-\nIIJJ\n"],
    "invalid-separator-marker"
)]
#[case::separator_header_mismatch(
    single,
    &["@r\nACGT\n+s\nIIJJ\n"],
    "separator-header-mismatch"
)]
#[case::separator_title_is_prefix_of_header(
    single,
    &["@rr\nACGT\n+r\nIIJJ\n"],
    "separator-header-mismatch"
)]
#[case::separator_title_with_leading_space(
    single,
    &["@r\nACGT\n+ r\nIIJJ\n"],
    "separator-header-mismatch"
)]
#[case::separator_header_mismatch_before_invalid_sequence_character(
    single,
    &["@r\nACXT\n+s\nIIJJ\n"],
    "separator-header-mismatch"
)]
#[case::invalid_sequence_character(
    single,
    &["@r\nACXT\n+\nIIJJ\n"],
    "invalid-sequence-character"
)]
#[case::invalid_quality_byte(single, &["@r\nACGT\n+\nII J\n"], "invalid-quality-byte")]
#[case::sequence_quality_length_mismatch(
    single,
    &["@r\nACGT\n+\nIIJ\n"],
    "sequence-quality-length-mismatch"
)]
#[case::lowercase_sequence(single, &["@r\nACgT\n+\nIIJJ\n"], "lowercase-sequence")]
#[case::short_read(single, &["@r\n\n+\n\n"], "short-read")]
#[case::sequence_all_same_byte(single, &["@r\nAAAA\n+\nIIJJ\n"], "sequence-all-same-byte")]
#[case::quality_all_same_byte(single, &["@r\nACGT\n+\nIIII\n"], "quality-all-same-byte")]
#[case::mate_mismatch_paired(
    paired,
    &["@a/1\nACGT\n+\nIIJJ\n", "@b/2\nACGT\n+\nIIJJ\n"],
    "mate-mismatch"
)]
#[case::mate_mismatch_interleaved(
    interleaved,
    &["@a/1\nACGT\n+\nIIJJ\n@b/2\nACGT\n+\nIIJJ\n"],
    "mate-mismatch"
)]
#[case::singleton(
    paired,
    &["@p/1\nACGT\n+\nIIJJ\n@q/1\nACGT\n+\nIIJJ\n", "@p/2\nACGT\n+\nIIJJ\n"],
    "singleton"
)]
fn run_rejects(
    #[case] layout: fn(&[PathBuf]) -> InputType,
    #[case] files: &[&str],
    #[case] code: &str,
) {
    assert_eq!(
        failure_code(layout, &bytes(files), default_rules()).as_deref(),
        Some(code)
    );
}

#[rstest]
#[case::valid(LONG_SEQUENCE.to_owned(), LONG_QUALITY.to_owned(), None)]
#[case::invalid_sequence_character_in_first_block(
    replace_byte(LONG_SEQUENCE, 0, 'X'),
    LONG_QUALITY.to_owned(),
    Some("invalid-sequence-character")
)]
#[case::invalid_sequence_character_in_upper_half(
    replace_byte(LONG_SEQUENCE, 20, 'X'),
    LONG_QUALITY.to_owned(),
    Some("invalid-sequence-character")
)]
#[case::invalid_sequence_character_in_tail(
    replace_byte(LONG_SEQUENCE, 69, 'X'),
    LONG_QUALITY.to_owned(),
    Some("invalid-sequence-character")
)]
#[case::non_ascii_sequence_byte(
    replace_byte(LONG_SEQUENCE, 69, 'é'),
    LONG_QUALITY.to_owned(),
    Some("invalid-sequence-character")
)]
#[case::lowercase_sequence_in_tail(
    replace_byte(LONG_SEQUENCE, 69, 'g'),
    LONG_QUALITY.to_owned(),
    Some("lowercase-sequence")
)]
#[case::invalid_quality_byte_in_tail(
    LONG_SEQUENCE.to_owned(),
    replace_byte(LONG_QUALITY, 69, ' '),
    Some("invalid-quality-byte")
)]
fn run_checks_long_reads(
    #[case] sequence: String,
    #[case] quality: String,
    #[case] code: Option<&str>,
) {
    let record = format!("@r\n{sequence}\n+\n{quality}\n");
    assert_eq!(
        failure_code(single, &[record.as_bytes()], default_rules()).as_deref(),
        code
    );
}

#[rstest]
#[case::valid(LONG_HEADER.to_owned(), None)]
#[case::tab(replace_byte(LONG_HEADER, 43, '\t'), None)]
#[case::invalid_header_character_in_first_block(
    replace_byte(LONG_HEADER, 1, '\0'),
    Some("invalid-header-character")
)]
#[case::invalid_header_character_in_tail(
    replace_byte(LONG_HEADER, 66, '\u{1b}'),
    Some("invalid-header-character")
)]
#[case::non_ascii_header_byte_in_tail(
    replace_byte(LONG_HEADER, 66, '\u{e9}'),
    Some("invalid-header-character")
)]
#[case::max_length(header_of_length(255), None)]
#[case::header_too_long(header_of_length(256), Some("header-too-long"))]
#[case::invalid_header_character_before_header_too_long(
    replace_byte(&header_of_length(256), 1, '\x01'),
    Some("invalid-header-character")
)]
fn run_checks_long_headers(#[case] header: String, #[case] code: Option<&str>) {
    let record = format!("{header}\nACGT\n+\nIIJJ\n");
    assert_eq!(
        failure_code(single, &[record.as_bytes()], default_rules()).as_deref(),
        code
    );
}

#[test]
fn run_allows_header_too_long() {
    let record = format!("{}\nACGT\n+\nIIJJ\n", header_of_length(256));
    let rules = RecordRules {
        header_too_long: false,
        ..default_rules()
    };
    assert_eq!(failure_code(single, &[record.as_bytes()], rules), None);
}

#[test]
fn run_rejects_undecodable_gzip() {
    assert_eq!(
        failure_code(single, &[BROKEN_GZIP], default_rules()).as_deref(),
        Some("input-read-failed")
    );
}

#[test]
fn run_errors_missing_input() {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let layout = InputType::Single(Source::Local(directory.path().join("missing.fq")));
    assert!(amro::run(&layout, &default_rules()).is_err());
}

#[rstest]
#[case::whole_input(single, &[""], json!([{ "input": "r1" }]))]
#[case::record_field_and_column(
    single,
    &["@r\nACRT\n+\nIIJJ\n"],
    json!([{ "input": "r1", "record": 1, "line": 2, "column": 3, "field": "sequence" }])
)]
#[case::mates(
    paired,
    &["@a/1\nACGT\n+\nIIJJ\n", "@b/2\nACGT\n+\nIIJJ\n"],
    json!([
        { "input": "r1", "record": 1, "line": 1, "field": "header", "read-id": "a/1" },
        { "input": "r2", "record": 1, "line": 1, "field": "header", "read-id": "b/2" }
    ])
)]
fn run_reports_locations(
    #[case] layout: fn(&[PathBuf]) -> InputType,
    #[case] files: &[&str],
    #[case] locations: Value,
) {
    assert_eq!(
        report(layout, &bytes(files), default_rules())["failures"][0]["locations"],
        locations
    );
}

#[test]
fn run_reports_record_location_without_field() {
    assert_eq!(
        report(single, &[BROKEN_GZIP], default_rules())["failures"][0]["locations"],
        json!([{ "input": "r1", "record": 1, "line": 1 }])
    );
}

fn default_rules() -> RecordRules {
    RecordRules {
        header_too_long: true,
        alphabet: SequenceBytes::Standard,
        lowercase_sequence: true,
        encoding: QualityEncoding::Phred33,
        min_length: 1,
        sequence_all_same_byte: true,
        quality_all_same_byte: true,
    }
}

fn failure_code(
    layout: fn(&[PathBuf]) -> InputType,
    files: &[&[u8]],
    rules: RecordRules,
) -> Option<String> {
    report(layout, files, rules)["failures"][0]["code"]
        .as_str()
        .map(str::to_owned)
}

fn report(layout: fn(&[PathBuf]) -> InputType, files: &[&[u8]], rules: RecordRules) -> Value {
    let directory = tempfile::tempdir().expect("temporary directory is created");
    let paths: Vec<PathBuf> = files
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            let path = directory.path().join(format!("{index}.fq"));
            fs::write(&path, bytes).expect("input is written");
            path
        })
        .collect();
    let layout = layout(&paths);
    let failure = amro::run(&layout, &rules).expect("inputs are readable");
    let mut report = Vec::new();
    amro::write_output(&mut report, OutputMode::Json, &layout, failure.as_ref())
        .expect("report is written");
    serde_json::from_slice(&report).expect("report is JSON")
}

fn replace_byte(text: &str, offset: usize, replacement: char) -> String {
    format!("{}{replacement}{}", &text[..offset], &text[offset + 1..])
}

fn header_of_length(length: usize) -> String {
    format!("@{}", "r".repeat(length - 1))
}

fn bytes<'text>(files: &[&'text str]) -> Vec<&'text [u8]> {
    files.iter().map(|file| file.as_bytes()).collect()
}

fn single(paths: &[PathBuf]) -> InputType {
    InputType::Single(Source::Local(paths[0].clone()))
}

fn paired(paths: &[PathBuf]) -> InputType {
    InputType::Paired {
        r1: Source::Local(paths[0].clone()),
        r2: Source::Local(paths[1].clone()),
    }
}

fn interleaved(paths: &[PathBuf]) -> InputType {
    InputType::Interleaved(Source::Local(paths[0].clone()))
}
