use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use owo_colors::{Stream, Style};
use serde::Serialize;
use serde_json::json;
use tabled::Table;
use tabled::settings::Style as TableStyle;

use crate::cli::{InputType, OutputMode};
use crate::input;
use crate::records::Field;
use crate::rules::{Rule, RuleCode, RuleInfo, Scope};

pub fn write_output(
    writer: &mut impl Write,
    mode: OutputMode,
    layout: &InputType,
    failure: Option<&Failure>,
) -> io::Result<()> {
    match (mode, failure) {
        (OutputMode::Quiet, _) => Ok(()),
        (OutputMode::Human, None) => writeln!(writer, "Yes, your reads are ok."),
        (OutputMode::Human, Some(failure)) => write_human(writer, layout, failure),
        (OutputMode::Json, failure) => {
            write_json(writer, &json!({ "failures": failure.as_slice() }))
        }
    }
}

pub fn write_rules(writer: &mut impl Write, json: bool) -> io::Result<()> {
    use strum::VariantArray;

    let rules: Vec<RuleInfo> = RuleCode::VARIANTS.iter().map(|code| code.info()).collect();
    if json {
        return write_json(writer, &json!({ "rules": rules }));
    }
    let mut table = Table::new(&rules);
    table.with(TableStyle::rounded());
    writeln!(writer, "{table}")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "Turn off optional rules with --allow <RULE>, and short-read with --min-length 0."
    )
}

#[derive(Debug, Serialize)]
pub struct Failure {
    code: RuleCode,
    message: &'static str,
    scope: Scope,
    locations: Vec<Location>,
    details: Rule,
}

impl Failure {
    /// A failure covering the whole input.
    pub fn in_input(input: InputLabel, rule: Rule) -> Self {
        Self::new(rule, vec![Location { input, place: None }])
    }

    /// A failure in one record
    pub fn in_record(at: RecordAt, rule: Rule) -> Self {
        let locations = match rule.fields() {
            [] => vec![Location::at(at, None, None)],
            fields => fields
                .iter()
                .map(|&field| Location::at(at, Some(field), rule.column()))
                .collect(),
        };
        Self::new(rule, locations)
    }

    /// A failure in mate pairing
    pub fn in_mates(rule: Rule, mates: &[(RecordAt, &[u8])]) -> Self {
        let locations = mates
            .iter()
            .map(|&(at, read_id)| Location {
                input: at.input,
                place: Some(RecordPlace {
                    read_id: Some(String::from_utf8_lossy(read_id).into_owned()),
                    ..RecordPlace::new(at.number, Some(Field::Header), None)
                }),
            })
            .collect();
        Self::new(rule, locations)
    }

    fn records_for_sed<'layout>(&self, layout: &'layout InputType) -> Vec<SedDisplay<'layout>> {
        if !self.details.records_are_readable() {
            return Vec::new();
        }
        let mut views: Vec<SedDisplay<'_>> = Vec::new();
        for location in &self.locations {
            // only give the sed hint for regular files
            let Some(path) = layout
                .source(location.input)
                .local_path()
                .filter(|path| fs::metadata(path).is_ok_and(|metadata| metadata.is_file()))
            else {
                continue;
            };
            let Some(record) = location.place.as_ref().map(|place| place.record) else {
                continue;
            };
            match views.last_mut() {
                Some(view) if view.path == path => {
                    view.first = view.first.min(record);
                    view.last = view.last.max(record);
                }
                _ => {
                    let Ok(gzip) = File::open(path).and_then(|file| input::is_gzip(&file)) else {
                        continue;
                    };
                    views.push(SedDisplay {
                        path,
                        gzip,
                        first: record,
                        last: record,
                    });
                }
            }
        }
        views
    }

    fn new(rule: Rule, locations: Vec<Location>) -> Self {
        let info = rule.code().info();
        Self {
            code: info.code,
            message: info.message,
            scope: info.scope,
            locations,
            details: rule,
        }
    }
}

struct SedDisplay<'layout> {
    path: &'layout Path,
    gzip: bool,
    first: u64,
    last: u64,
}

impl fmt::Display for SedDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.to_string_lossy();
        let path = shlex::try_quote(&path).expect("paths have no NUL bytes");
        let start = Field::Header.line(self.first);
        let end = Field::Quality.line(self.last);
        let lines = format!("sed -n '{start},{end}p;{end}q'");
        if self.gzip {
            write!(formatter, "gzip -cd {path} | {lines}")
        } else {
            write!(formatter, "{lines} {path}")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordAt {
    input: InputLabel,
    number: u64,
}

impl RecordAt {
    pub fn new(input: InputLabel, number: u64) -> Self {
        Self { input, number }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, strum::Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum InputLabel {
    R1,
    R2,
    Interleaved,
}

#[derive(Debug, Serialize)]
struct Location {
    input: InputLabel,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    place: Option<RecordPlace>,
}

impl Location {
    fn at(at: RecordAt, field: Option<Field>, column: Option<usize>) -> Self {
        Self {
            input: at.input,
            place: Some(RecordPlace::new(at.number, field, column)),
        }
    }

    fn write_human(&self, writer: &mut impl Write, layout: &InputType) -> io::Result<()> {
        write_field(writer, "file:", layout.source(self.input))?;
        let Some(place) = &self.place else {
            return Ok(());
        };
        write_field(writer, "record:", place.record)?;
        write_field(writer, "line:", place.line)?;
        if let Some(column) = place.column {
            write_field(writer, "column:", column)?;
        }
        if let Some(read_id) = &place.read_id {
            write_field(writer, "read id:", format_args!("{read_id:?}"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct RecordPlace {
    record: u64,
    line: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<Field>,
    #[serde(skip_serializing_if = "Option::is_none")]
    read_id: Option<String>,
}

impl RecordPlace {
    fn new(record: u64, field: Option<Field>, column: Option<usize>) -> Self {
        Self {
            record,
            line: field.unwrap_or(Field::Header).line(record),
            column,
            field,
            read_id: None,
        }
    }
}

fn write_human(writer: &mut impl Write, layout: &InputType, failure: &Failure) -> io::Result<()> {
    use owo_colors::OwoColorize;

    let red = Style::new().red();

    writeln!(writer, "No, your reads are not ok.")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "{}: {}",
        failure
            .code
            .if_supports_color(Stream::Stdout, |text| text.style(red)),
        failure.details
    )?;
    writeln!(writer)?;

    for (index, location) in failure.locations.iter().enumerate() {
        if index > 0 {
            writeln!(writer)?;
        }
        location.write_human(writer, layout)?;
    }
    if let Some(hint) = failure.details.hint() {
        writeln!(writer)?;
        writeln!(writer, "Hint:")?;
        writeln!(writer, "   {hint}")?;
    }

    let views = failure.records_for_sed(layout);
    if views.is_empty() {
        return Ok(());
    }
    let records: u64 = views.iter().map(|view| view.last - view.first + 1).sum();
    writeln!(writer)?;
    match records {
        1 => writeln!(writer, "To view this record:")?,
        _ => writeln!(writer, "To view these records:")?,
    }
    for view in views {
        writeln!(writer, "   {view}")?;
    }
    Ok(())
}

fn write_field(writer: &mut impl Write, label: &str, value: impl fmt::Display) -> io::Result<()> {
    use owo_colors::OwoColorize;

    let purple = Style::new().purple();
    writeln!(
        writer,
        "   {label:<10}{}",
        value.if_supports_color(Stream::Stdout, |text| text.style(purple))
    )
}

fn write_json(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *writer, value)?;
    writeln!(writer)
}
