use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "dotmerge",
    version,
    about = "Conservative jj-backed dotfile sync"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect the current sync state without changing anything.
    Status(StatusArgs),
    /// Run the sync workflow.
    Sync(SyncArgs),
    /// Admit one or more local paths into sync.
    Add(AddArgs),
}

#[derive(Debug, Args, Clone)]
pub struct RepoTargetArgs {
    /// Path to the jj repo.
    #[arg(long)]
    pub repo: PathBuf,

    /// Revision expression to synchronize against.
    #[arg(long)]
    pub target: String,
}

#[derive(Debug, Args, Clone)]
pub struct StatusArgs {
    #[command(flatten)]
    pub common: RepoTargetArgs,
}

#[derive(Debug, Args, Clone)]
pub struct SyncArgs {
    #[command(flatten)]
    pub common: RepoTargetArgs,

    /// Stop after the repo-side phases and skip exporting to $HOME.
    #[arg(long)]
    pub no_export: bool,
}

#[derive(Debug, Args, Clone)]
pub struct AddArgs {
    /// Path to the jj repo.
    #[arg(long)]
    pub repo: PathBuf,

    /// One or more absolute or home-relative paths to admit into sync.
    #[arg(value_name = "PATH", required = true)]
    pub paths: Vec<PathBuf>,
}
