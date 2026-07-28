use clap::Parser;

use salcc::cli;

fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse();
    cli::run(args)
}
