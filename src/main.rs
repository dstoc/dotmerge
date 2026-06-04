#![warn(unused)]

mod add;
mod cli;
mod fs;
mod export;
mod import;
mod merge;
mod jj;
mod model;
mod status;
mod sync;
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

fn handle_status(args: cli::StatusArgs) -> Result<()> {
    status::run(args)
}

fn handle_sync(args: cli::SyncArgs) -> Result<()> {
    sync::run(args)
}

fn handle_add(args: cli::AddArgs) -> Result<()> {
    add::run(args)
}
