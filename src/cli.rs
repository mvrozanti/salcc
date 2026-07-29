//! CLI surface. `salcc <cmd...>` wraps a child; subcommands are reserved
//! words that cannot collide with wrapping (record/replay/stats/doctor).

use clap::{Parser, Subcommand};

use crate::proxy::io_loop;

#[derive(Parser, Debug)]
#[command(name = "salcc", about = "Semantic attention layer for terminal AI agents", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Disable all styling (bit-perfect passthrough)
    #[arg(long, global = true)]
    pub no_style: bool,

    /// Child command to wrap, e.g. `salcc claude`
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub child: Vec<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Record a raw session to a fixture file
    Record {
        #[arg(short, long)]
        output: std::path::PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        child: Vec<String>,
    },
    /// Replay a fixture through the styling pipeline
    Replay {
        fixture: std::path::PathBuf,
    },
    /// Show scoring cost summary; --serve starts the local dashboard
    Stats {
        #[arg(long)]
        serve: bool,
        #[arg(long, default_value_t = 7877)]
        port: u16,
    },
    /// Diagnose terminal capabilities and configuration
    Doctor {
        #[arg(long)]
        verify: bool,
    },
}

pub fn run(args: Cli) -> anyhow::Result<()> {
    match args.command {
        Some(Command::Record { .. }) => anyhow::bail!("record: not yet implemented"),
        Some(Command::Replay { .. }) => anyhow::bail!("replay: not yet implemented"),
        Some(Command::Stats { .. }) => anyhow::bail!("stats: not yet implemented"),
        Some(Command::Doctor { .. }) => anyhow::bail!("doctor: not yet implemented"),
        None => {
            if args.child.is_empty() {
                anyhow::bail!("usage: salcc <command...>  (e.g. `salcc claude`)");
            }
            let code = io_loop::run(&args.child, &mut io_loop::IdentityTransform)?;
            std::process::exit(code)
        }
    }
}
