mod cli;

fn main() {
    let cli = cli::Cli::parse_args();
    println!("{}", cli.input.display());
}
