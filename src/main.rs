use std::io;
use std::process::ExitCode;

use amro::{Action, InputType, OutputMode, RecordRules};
use color_eyre::eyre::WrapErr;

fn main() -> ExitCode {
    color_eyre::install().expect("coloreyre is only installed once");

    let action = Action::from_args();
    let result = match &action {
        Action::Validate {
            layout,
            rules,
            output,
        } => run_amro(layout, rules, *output),
        Action::ListRules { json } => run_rules(*json),
    };
    match result {
        // in amro exit code 1 means fastq was not ok
        // 2 means any other user/operational error
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("{error:?}");
            ExitCode::from(2)
        }
    }
}

fn run_amro(
    input_type: &InputType,
    rules: &RecordRules,
    output: OutputMode,
) -> color_eyre::Result<ExitCode> {
    // some if read error, none if fastq valid
    let amro_result = amro::run(input_type, rules)?;

    amro::write_output(
        &mut io::stdout().lock(),
        output,
        input_type,
        amro_result.as_ref(),
    )
    .wrap_err("could not write output")?;

    Ok(if amro_result.is_some() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn run_rules(json: bool) -> color_eyre::Result<ExitCode> {
    amro::write_rules(&mut io::stdout().lock(), json).wrap_err("could not write output")?;
    Ok(ExitCode::SUCCESS)
}
