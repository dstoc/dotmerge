#![warn(unused)]

mod add;
mod cli;
mod config;
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
use std::path::Path;

fn main() {
    if let Err(err) = run() {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = cli::Cli::parse();
    let config = cli.config;

    match cli.command {
        Command::Status(args) => handle_status(config.as_deref(), args),
        Command::Sync(args) => handle_sync(config.as_deref(), args),
        Command::Add(args) => handle_add(config.as_deref(), args),
    }
}

fn handle_status(config_flag: Option<&Path>, args: cli::StatusArgs) -> Result<()> {
    status::run(config_flag, args)
}

fn handle_sync(config_flag: Option<&Path>, args: cli::SyncArgs) -> Result<()> {
    sync::run(config_flag, args)
}

fn handle_add(config_flag: Option<&Path>, args: cli::AddArgs) -> Result<()> {
    add::run(config_flag, args)
}
