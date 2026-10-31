use clap::builder::{OsStringValueParser, TypedValueParser};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::input::Source;
use crate::output::InputLabel;
use crate::rules::RecordRules;
use crate::scan::{QualityEncoding, SequenceBytes};

#[derive(Debug)]
pub enum Action {
    Validate {
        layout: InputType,
        rules: RecordRules,
        output: OutputMode,
    },
    ListRules {
        json: bool,
    },
}

impl Action {
    pub fn from_args() -> Self {
        let cli = Cli::parse();
        let (layout, arguments) = match cli.command {
            Command::Single { input, arguments } => (InputType::Single(input), arguments),
            Command::Paired { r1, r2, arguments } => (InputType::Paired { r1, r2 }, arguments),
            Command::Interleaved { input, arguments } => (InputType::Interleaved(input), arguments),
            Command::Rules { json } => return Self::ListRules { json },
        };
        Self::Validate {
            layout,
            rules: arguments.rules(),
            output: arguments.output(),
        }
    }
}

#[derive(Debug)]
pub enum InputType {
    Single(Source),
    Paired { r1: Source, r2: Source },
    Interleaved(Source),
}

impl InputType {
    pub fn source(&self, label: InputLabel) -> &Source {
        match (self, label) {
            (Self::Single(source) | Self::Interleaved(source), _) => source,
            (Self::Paired { r1, .. }, InputLabel::R1) => r1,
            (Self::Paired { r2, .. }, _) => r2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Quiet,
    Json,
}

/// Are your FASTQ files ok?
#[derive(Parser)]
#[command(
    version,
    about,
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a single end FASTQ file.
    Single {
        #[arg(value_parser = OsStringValueParser::new().try_map(Source::parse))]
        input: Source,
        #[command(flatten)]
        arguments: ValidateArguments,
    },
    /// Validate a set of paired end FASTQ files.
    Paired {
        #[arg(value_parser = OsStringValueParser::new().try_map(Source::parse))]
        r1: Source,
        #[arg(value_parser = OsStringValueParser::new().try_map(Source::parse))]
        r2: Source,
        #[command(flatten)]
        arguments: ValidateArguments,
    },
    /// Validate an interleaved FASTQ file of alternating R1 and R2 mates.
    Interleaved {
        #[arg(value_parser = OsStringValueParser::new().try_map(Source::parse))]
        input: Source,
        #[command(flatten)]
        arguments: ValidateArguments,
    },
    /// List AMRO rules.
    Rules {
        /// Print the rules as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
struct ValidateArguments {
    /// Bases a sequence may contain.
    ///
    /// A sequence with any other byte fails invalid-sequence-character.
    #[arg(long, value_enum, default_value_t)]
    alphabet: SequenceBytes,

    /// Bytes a quality string may contain, depending on how qualities were encoded.
    ///
    /// A quality with any other byte fails invalid-quality-byte.
    #[arg(long, value_enum, default_value_t)]
    quality_encoding: QualityEncoding,

    /// Shortest sequence length allowed.
    ///
    /// A sequence shorter than this fails the short-read rule.
    /// The default rejects empty reads, use 0 to allow them.
    #[arg(long, value_name = "BASES", default_value_t = 1)]
    min_length: usize,

    /// Disable default rules.
    ///
    /// Codes can be comma-separated or given in repeated flags. To allow
    /// empty or short reads, see --min-length.
    #[arg(long, value_name = "CODE", value_enum, value_delimiter = ',')]
    allow: Vec<Allow>,

    /// Disable all output, the outcome is reported by exit code only.
    #[arg(long, conflicts_with = "json")]
    quiet: bool,

    /// Print the result as JSON.
    #[arg(long)]
    json: bool,
}

impl ValidateArguments {
    fn rules(&self) -> RecordRules {
        RecordRules {
            header_too_long: !self.allow.contains(&Allow::HeaderTooLong),
            alphabet: self.alphabet,
            lowercase_sequence: !self.allow.contains(&Allow::LowercaseSequence),
            encoding: self.quality_encoding,
            min_length: self.min_length,
            sequence_all_same_byte: !self.allow.contains(&Allow::SequenceAllSameByte),
            quality_all_same_byte: !self.allow.contains(&Allow::QualityAllSameByte),
        }
    }

    fn output(&self) -> OutputMode {
        if self.quiet {
            OutputMode::Quiet
        } else if self.json {
            OutputMode::Json
        } else {
            OutputMode::Human
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Allow {
    /// Allow headers longer than 255 bytes.
    HeaderTooLong,
    /// Allow lowercase bases in sequences.
    LowercaseSequence,
    /// Allow sequences made of one repeated base.
    SequenceAllSameByte,
    /// Allow quality strings made of one repeated byte.
    QualityAllSameByte,
}
