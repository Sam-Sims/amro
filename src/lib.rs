mod cli;
mod input;
mod output;
mod records;
mod rules;
mod run;
mod scan;

pub use cli::{Action, InputType, OutputMode};
pub use input::Source;
pub use output::{write_output, write_rules};
pub use records::Record;
pub use rules::RecordRules;
pub use run::run;
pub use scan::{QualityEncoding, SequenceBytes};
