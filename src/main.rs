//! tf2demos — organize Team Fortress 2 demos recorded by the built-in `ds_*` demo support.

mod archive;
mod config;
mod demo;
mod index;
mod tf2;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "tf2demos", version, about)]
struct Cli {
    /// Path to config.toml (default: ~/.config/tf2demos/config.toml)
    #[arg(long, global = true, env = "TF2DEMOS_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Age, rename, and archive demos; fold _events.txt; regenerate by-label symlinks.
    Organize {
        /// Print every action without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::Config::load(cli.config.as_deref())?;
    match cli.command {
        Command::Organize { dry_run } => archive::organize(&cfg, dry_run),
    }
}
