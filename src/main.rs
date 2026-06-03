mod cli;
mod jj;
mod model;
mod util;

use anyhow::Result;
use clap::Parser;
use cli::Command;

fn main() {
    if let Err(err) = run() {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = cli::Cli::parse();

    match cli.command {
        Command::Status(args) => handle_status(args),
        Command::Sync(args) => handle_sync(args),
        Command::Add(args) => handle_add(args),
    }
}

fn handle_status(_args: cli::StatusArgs) -> Result<()> {
    Err(util::not_implemented("status"))
}

fn handle_sync(_args: cli::SyncArgs) -> Result<()> {
    Err(util::not_implemented("sync"))
}

fn handle_add(_args: cli::AddArgs) -> Result<()> {
    Err(util::not_implemented("add"))
}
