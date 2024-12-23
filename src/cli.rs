use std::path::PathBuf;

use clap::Parser;

/// Are your FASTQ files ok?
#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// FASTQ file to validate.
    pub input: PathBuf,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
