//! tf2demos — organize, review, and play Team Fortress 2 demos recorded by the built-in `ds_*`
//! demo support.

mod archive;
mod config;
mod demo;
mod index;
mod review;
mod tf2;
mod ui;
mod watch;

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "tf2demos", version, about)]
struct Cli {
    /// Path to config.toml (default: ~/.config/tf2demos/config.toml); theme.toml is read from
    /// the same directory.
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
    /// Watch for TF2 to exit and offer to review new marks (systemd user service).
    Watch,
    /// Label unlabelled marks one card at a time.
    Review,
    /// Play a demo at a tick: launch TF2, or copy the console command when TF2 is running.
    Play {
        /// Index id (`2026-09-21_19-51-20`, `Tight_scout_m`) or archived file name.
        id: String,
        /// Tick to jump to (default: the demo's first mark).
        #[arg(long)]
        tick: Option<i64>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::Config::load(cli.config.as_deref())?;
    match cli.command {
        Command::Organize { dry_run } => archive::organize(&cfg, dry_run),
        Command::Watch => watch::run(&cfg, cli.config.as_deref()),
        Command::Review => {
            let theme_path = config::Config::sibling_path(cli.config.as_deref(), "theme.toml");
            let theme = ui::theme::Theme::load(&theme_path);
            ui::run_review(&cfg, &theme)
        }
        Command::Play { id, tick } => play(&cfg, &id, tick),
    }
}

/// Resolve `id` against the index plus the hot demos on disk, then hand off to `tf2::play`.
fn play(cfg: &config::Config, id: &str, tick: Option<i64>) -> Result<()> {
    let scan = review::scan(cfg)?;
    let wanted = id.trim_end_matches(".dem");
    let entry = scan.index.demos.iter().find(|d| {
        d.id == wanted || d.original_name == wanted || d.file_stem() == wanted || d.file == id
    });
    let Some(entry) = entry else {
        bail!("no demo matches {id:?} (ids: {})", ids(&scan.index));
    };
    let tick = tick
        .or_else(|| entry.events.first().map(|e| e.tick))
        .unwrap_or(0);
    let msg = tf2::play(&entry.file, tick)?;
    println!("{msg}");
    Ok(())
}

fn ids(index: &index::Index) -> String {
    let v: Vec<&str> = index.demos.iter().map(|d| d.id.as_str()).collect();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}
