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
    /// List every mark: demo, date, map, tick, time into the demo, wall-clock time, labels.
    Events {
        /// Only marks without a label.
        #[arg(long)]
        unlabelled: bool,
        /// Only this demo (id, original name, or archived file name).
        #[arg(long)]
        demo: Option<String>,
    },
    /// Edit one mark's label, class, rating, or streak (see `tf2demos events` for ids and ticks).
    Edit {
        /// Index id (`2026-09-21_19-51-20`, `Tight_scout_m`) or archived file name.
        id: String,
        /// Tick of the mark (any tick of a grouped press works). Optional when the demo has one mark.
        #[arg(long)]
        tick: Option<i64>,
        /// New label (free text; added to the label list).
        #[arg(long, conflicts_with = "clear_label")]
        label: Option<String>,
        /// Remove the label; the mark returns to the review queue.
        #[arg(long)]
        clear_label: bool,
        /// Your class (scout, soldier, ...).
        #[arg(long, conflicts_with = "clear_class")]
        class: Option<String>,
        #[arg(long)]
        clear_class: bool,
        /// Rating 1–5.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=5), conflicts_with = "clear_rating")]
        rating: Option<u8>,
        #[arg(long)]
        clear_rating: bool,
        /// Kill streak / combo length (≥ 1).
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        streak: Option<u32>,
        /// Put the demo's unlabelled marks back into the review wizard (undo a skip).
        #[arg(long, conflicts_with = "reviewed")]
        requeue: bool,
        /// Mark the demo as reviewed even with unlabelled marks.
        #[arg(long)]
        reviewed: bool,
    },
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
        Command::Events { unlabelled, demo } => events(&cfg, unlabelled, demo.as_deref()),
        Command::Edit {
            id,
            tick,
            label,
            clear_label,
            class,
            clear_class,
            rating,
            clear_rating,
            streak,
            requeue,
            reviewed,
        } => {
            let patch = review::EventPatch {
                label: if clear_label {
                    Some(None)
                } else {
                    label.map(Some)
                },
                class: if clear_class {
                    Some(None)
                } else {
                    class.map(Some)
                },
                rating: if clear_rating {
                    Some(None)
                } else {
                    rating.map(Some)
                },
                streak: streak.map(Some),
                reviewed: if requeue {
                    Some(false)
                } else if reviewed {
                    Some(true)
                } else {
                    None
                },
            };
            if patch == review::EventPatch::default() {
                bail!(
                    "nothing to change: pass --label/--class/--rating/--streak, --clear-*, --requeue, or --reviewed"
                );
            }
            let entry = review::edit_event(&cfg, &id, tick, &patch)?;
            print_events_header();
            for ev in &entry.events {
                if tick.is_none_or(|t| ev.tick == t || ev.raw_ticks.contains(&t)) {
                    print_event(&cfg, &entry, ev);
                }
            }
            Ok(())
        }
    }
}

/// `tf2demos events`: one line per mark, hot demos included, oldest first.
fn events(cfg: &config::Config, unlabelled: bool, demo: Option<&str>) -> Result<()> {
    let scan = review::scan(cfg)?;
    let only = match demo {
        Some(key) => Some(
            review::find_demo(&scan.index, key)
                .map(|d| d.id.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!("no demo matches {key:?} (ids: {})", ids(&scan.index))
                })?,
        ),
        None => None,
    };
    print_events_header();
    let mut n = 0;
    for d in &scan.index.demos {
        if only.as_deref().is_some_and(|id| id != d.id) {
            continue;
        }
        for ev in &d.events {
            if unlabelled && ev.label.is_some() {
                continue;
            }
            print_event(cfg, d, ev);
            n += 1;
        }
    }
    if n == 0 {
        println!("(no marks)");
    }
    Ok(())
}

/// Column layout shared by the header and the rows.
macro_rules! event_row {
    ($($arg:expr),*) => { println!("{:<21} {:<16} {:<19} {:>7} {:>6} {:<8} {:>2} {:<12} {:<9} {:>1} {:>3}  {}", $($arg),*) };
}

fn print_events_header() {
    let (demo, recorded, map, tick, into, at, n, label, class, r, stk, state) = (
        "demo", "recorded", "map", "tick", "into", "at", "n", "label", "class", "r", "stk", "state",
    );
    event_row!(
        demo, recorded, map, tick, into, at, n, label, class, r, stk, state
    );
}

fn print_event(cfg: &config::Config, d: &index::DemoEntry, ev: &index::Event) {
    let state = format!(
        "{}{}",
        if d.is_archived(&cfg.archive_dir) {
            "archived"
        } else {
            "hot"
        },
        if d.reviewed { ", reviewed" } else { "" }
    );
    event_row!(
        d.id,
        d.recorded_at.format("%Y-%m-%d %H:%M"),
        d.map,
        ev.tick,
        review::format_offset(ev.tick),
        review::mark_time(d.recorded_at, ev.tick).format("%H:%M:%S"),
        ev.presses,
        ev.label.as_deref().unwrap_or("-"),
        ev.class.as_deref().unwrap_or("-"),
        ev.rating.map_or("-".to_string(), |r| r.to_string()),
        ev.streak.map_or("-".to_string(), |s| s.to_string()),
        state
    );
}

/// Resolve `id` against the index plus the hot demos on disk, then hand off to `tf2::play`.
fn play(cfg: &config::Config, id: &str, tick: Option<i64>) -> Result<()> {
    let scan = review::scan(cfg)?;
    let Some(entry) = review::find_demo(&scan.index, id) else {
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
