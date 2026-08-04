use clap::Parser;

use salcc::{cli, shim};

fn main() -> anyhow::Result<()> {
    if let Some(agent) = shim::shim_agent_name(std::env::args_os().next().as_deref()) {
        return shim::run_shim(&agent);
    }
    let args = cli::Cli::parse();
    cli::run(args)
}
