//! `tf2demos organize [--dry-run]` — the daily organizing pass.
//!
//! One pass over `<tf_dir>/demos` (non-recursive, `archive/` excluded):
//! 1. every `*.dem` older than `age_hours` with a `.json` sidecar is moved to
//!    `<archive>/<YYYY>/<MM>/<DD>/<stem>_<map>.dem`, indexed, and its grouped marks appended to
//!    the day's `events.txt`;
//! 2. every old `*.dem` **without** a sidecar is deleted (`ds_autodelete` leftovers, crashes);
//! 3. `_events.txt` is folded: lines of archived demos go to their day's `events.txt`, lines of
//!    unknown demos to `<archive>/events-orphans.txt`, the rest is rewritten in place — unless
//!    TF2 is running, because ds appends to that file mid-game;
//! 4. `<archive>/by-label/` is deleted and regenerated from the index as relative symlinks;
//! 5. the index is saved atomically.
//!
//! The review wizard (`review.rs`) indexes demos while they are still hot, so the index may
//! already hold an entry for a demo being archived: [`Index::merge_archived`] keeps its labels,
//! [`DemoEntry::is_archived`] keeps the fold from touching lines of hot demos, and entries whose
//! hot file vanished (hand-deleted) are pruned unless they carry labels.
//!
//! `--dry-run` does all of it in memory and prints the same lines prefixed `[dry-run] `.

use std::collections::HashSet;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDateTime, Timelike};

use crate::config::Config;
use crate::demo::{self, Header, Sidecar};
use crate::index::{DemoEntry, Event, Index, State};
use crate::tf2;

/// Case (c) of the fold: a ds-named line matches a hand-renamed demo whose `recorded_at`
/// (`mtime − seconds`) is within this many seconds of the timestamp in the ds name.
const FOLD_MATCH_SLACK_SECS: i64 = 5;

/// Counters printed on the final `DONE` line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats {
    pub moved: usize,
    pub deleted: usize,
    pub folded: usize,
    pub orphans: usize,
    pub links: usize,
    pub skipped: usize,
    pub pruned: usize,
}

/// What a run printed, for tests and callers that want more than the exit code.
#[derive(Debug, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Report {
    pub lines: Vec<String>,
    pub stats: Stats,
}

/// Prints one line per action to stdout (`[dry-run] ` prefixed when dry) and keeps a copy.
struct Reporter {
    dry_run: bool,
    echo: bool,
    lines: Vec<String>,
}

impl Reporter {
    fn line(&mut self, text: impl AsRef<str>) {
        let text = if self.dry_run {
            format!("[dry-run] {}", text.as_ref())
        } else {
            text.as_ref().to_string()
        };
        if self.echo {
            println!("{text}");
        }
        self.lines.push(text);
    }
}

/// Entry point of the `organize` subcommand.
pub fn organize(cfg: &Config, dry_run: bool) -> Result<()> {
    organize_with(cfg, dry_run, tf2::is_running(), true).map(|_| ())
}

/// [`organize`] with the TF2 check and stdout echo passed in, so tests stay hermetic.
pub fn organize_with(cfg: &Config, dry_run: bool, tf2_running: bool, echo: bool) -> Result<Report> {
    let demos_dir = cfg.demos_dir();
    if !demos_dir.is_dir() && !dry_run {
        bail!(
            "demo directory {} does not exist (wrong tf_dir?); refusing to run without --dry-run",
            demos_dir.display()
        );
    }
    let archive_path = cfg.archive_path();
    let index_path = cfg.index_path();
    let loaded = Index::load_or_new(&index_path, &cfg.seed_labels)?;
    let mut index = loaded.clone();
    let mut rep = Reporter {
        dry_run,
        echo,
        lines: Vec::new(),
    };
    let mut stats = Stats::default();
    let write = !dry_run;

    // Steps 1–3: age, move or delete.
    let now = SystemTime::now();
    let stale_ids = stale_hot_ids(cfg, &index);
    let mut loop_result = Ok(());
    for path in list_demos(&demos_dir)? {
        let r = process_demo(
            cfg,
            &archive_path,
            &path,
            now,
            &mut index,
            &stale_ids,
            &mut rep,
            &mut stats,
            write,
        );
        if r.is_err() {
            loop_result = r;
            break;
        }
    }
    // §5: the index must never lag behind a completed move, even when a later move failed.
    if write && stats.moved > 0 {
        index.save(&index_path)?;
    }
    loop_result?;

    // Hot entries whose file is gone (hand-deleted, or hand-renamed and not matched by header).
    prune_stale_hot(cfg, &mut index, &mut rep, &mut stats);

    // Step 4: fold `_events.txt`.
    if tf2_running {
        rep.line("SKIP fold, TF2 running");
        stats.skipped += 1;
    } else {
        fold_master(
            cfg,
            &archive_path,
            &demos_dir,
            &index,
            &mut rep,
            &mut stats,
            write,
        )?;
    }

    // Step 5: by-label symlinks.
    regenerate_by_label(cfg, &archive_path, &index, &mut rep, &mut stats, write)?;

    // Step 6: index.
    if write && (index != loaded || !index_path.exists()) {
        index.save(&index_path)?;
    }

    rep.line(format!(
        "DONE moved={} deleted={} folded={} orphans={} links={} skipped={} pruned={}",
        stats.moved,
        stats.deleted,
        stats.folded,
        stats.orphans,
        stats.links,
        stats.skipped,
        stats.pruned
    ));
    Ok(Report {
        lines: rep.lines,
        stats,
    })
}

// ---------------------------------------------------------------------------------------------
// Steps 1–3: candidates, moves, deletes

/// Every regular `*.dem` directly inside `demos_dir`, sorted by file name. A missing directory
/// yields an empty list (dry-run against a wrong `tf_dir`).
pub(crate) fn list_demos(demos_dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(demos_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("listing {}", demos_dir.display())),
    };
    let mut demos = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("dem") {
            demos.push(path);
        }
    }
    demos.sort();
    Ok(demos)
}

/// Ids of not-yet-archived entries whose `file` no longer exists: a hot demo the user renamed
/// or deleted after the wizard indexed it.
fn stale_hot_ids(cfg: &Config, index: &Index) -> Vec<String> {
    index
        .demos
        .iter()
        .filter(|d| !d.is_archived(&cfg.archive_dir) && !cfg.tf_dir.join(&d.file).is_file())
        .map(|d| d.id.clone())
        .collect()
}

/// Drop stale hot entries that carry no labels; keep (and report) labelled ones, since a label
/// is never thrown away by this tool.
fn prune_stale_hot(cfg: &Config, index: &mut Index, rep: &mut Reporter, stats: &mut Stats) {
    let stale = stale_hot_ids(cfg, index);
    for id in stale {
        let labelled = index.by_id(&id).is_some_and(Index::has_labels);
        if labelled {
            rep.line(format!(
                "SKIP missing {id} (labelled entry kept, file gone)"
            ));
            stats.skipped += 1;
        } else {
            rep.line(format!("PRUNE {id} (hot entry, file gone)"));
            index.demos.retain(|d| d.id != id);
            stats.pruned += 1;
        }
    }
}

/// Hours between `now` and `mtime`; negative when `mtime` lies in the future.
fn age_hours(now: SystemTime, mtime: SystemTime) -> f64 {
    match now.duration_since(mtime) {
        Ok(d) => d.as_secs_f64() / 3600.0,
        Err(e) => -(e.duration().as_secs_f64() / 3600.0),
    }
}

/// Archived file name: `<stem>_<map>.dem` for ds-named and hand-renamed demos alike.
pub fn new_demo_name(stem: &str, map: &str) -> String {
    format!("{stem}_{map}.dem")
}

/// When the demo started: the timestamp in a ds name, else `mtime − header seconds` in local time.
pub fn recorded_at(stem: &str, mtime: SystemTime, seconds: f32) -> NaiveDateTime {
    if let Some(ts) = demo::parse_ds_name(stem) {
        return ts;
    }
    let length = Duration::from_secs_f64(f64::from(seconds.max(0.0)));
    let start = mtime.checked_sub(length).unwrap_or(mtime);
    let local: DateTime<Local> = start.into();
    local
        .naive_local()
        .with_nanosecond(0)
        .expect("0 is a valid nanosecond")
}

/// `<YYYY>/<MM>/<DD>` below the archive root.
fn day_dir(recorded_at: NaiveDateTime) -> PathBuf {
    PathBuf::from(recorded_at.format("%Y/%m/%d").to_string())
}

#[allow(clippy::too_many_arguments)]
fn process_demo(
    cfg: &Config,
    archive_path: &Path,
    path: &Path,
    now: SystemTime,
    index: &mut Index,
    stale_ids: &[String],
    rep: &mut Reporter,
    stats: &mut Stats,
    write: bool,
) -> Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let mtime = meta.modified()?;
    let age = age_hours(now, mtime);
    if age < cfg.age_hours {
        rep.line(format!("SKIP too new {name} (age {age:.2}h)"));
        stats.skipped += 1;
        return Ok(());
    }

    let sidecar_path = path.with_extension("json");
    if !sidecar_path.is_file() {
        rep.line(format!("DELETE {name} ({} bytes)", meta.len()));
        if write {
            fs::remove_file(path).with_context(|| format!("deleting {}", path.display()))?;
        }
        stats.deleted += 1;
        return Ok(());
    }

    let (header, sidecar) = match Header::read(path).and_then(|h| {
        let s = Sidecar::read(&sidecar_path)?;
        Ok((h, s))
    }) {
        Ok(v) => v,
        Err(err) => {
            rep.line(format!("SKIP unreadable {name}: {err:#}"));
            stats.skipped += 1;
            return Ok(());
        }
    };

    let recorded = recorded_at(&stem, mtime, header.seconds);
    let new_name = new_demo_name(&stem, &header.map);
    let day = day_dir(recorded);
    let dest_dir = archive_path.join(&day);
    let dest = dest_dir.join(&new_name);
    let rel_dest = cfg.archive_dir.join(&day).join(&new_name);
    let rel_dest_str = rel_dest.to_string_lossy().into_owned();

    if let Ok(existing) = fs::metadata(&dest) {
        rep.line(format!(
            "SKIP exists {rel_dest_str} ({} bytes there, {} bytes here)",
            existing.len(),
            meta.len()
        ));
        stats.skipped += 1;
        return Ok(());
    }

    rep.line(format!("MOVE {name} -> {rel_dest_str}"));
    if write {
        fs::create_dir_all(&dest_dir)
            .with_context(|| format!("creating {}", dest_dir.display()))?;
        move_verified(path, &dest)?;
        move_verified(
            &sidecar_path,
            &dest_dir.join(format!("{stem}_{}.json", header.map)),
        )?;
    }

    let events = events_from_sidecar(&sidecar, cfg.group_secs);
    index.merge_archived(
        DemoEntry {
            id: stem.clone(),
            file: rel_dest_str,
            original_name: stem.clone(),
            map: header.map,
            server: header.server,
            recorded_at: recorded,
            seconds: header.seconds,
            ticks: header.ticks,
            state: State::Hot,
            frozen_in: None,
            reviewed: false,
            events,
        },
        stale_ids,
    );
    // The day log shows the labels as they are now (the wizard may have set them while hot).
    let merged = index
        .by_id(&stem)
        .expect("entry was just merged under this id");
    let day_lines: Vec<String> = merged
        .events
        .iter()
        .map(|e| {
            format!(
                "{new_name}  tick={} presses={} label={} class={} rating={} streak={}",
                e.tick,
                e.presses,
                e.label.as_deref().unwrap_or("-"),
                e.class.as_deref().unwrap_or("-"),
                e.rating.map_or("-".to_string(), |r| r.to_string()),
                e.streak.map_or("-".to_string(), |s| s.to_string()),
            )
        })
        .collect();
    if write {
        append_lines(&dest_dir.join("events.txt"), &day_lines)?;
    }
    stats.moved += 1;
    Ok(())
}

/// Unlabelled index events from a sidecar's marks, grouped with `group_secs`.
pub fn events_from_sidecar(sidecar: &Sidecar, group_secs: f64) -> Vec<Event> {
    demo::group_marks(&sidecar.events, group_secs)
        .into_iter()
        .map(|g| Event {
            tick: g.tick,
            presses: g.presses,
            raw_ticks: g.raw_ticks,
            label: None,
            class: None,
            rating: None,
            streak: None,
        })
        .collect()
}

/// Rename `src` to `dest` (copy + remove across filesystems) and verify the destination exists
/// with the source's size before the source counts as gone.
fn move_verified(src: &Path, dest: &Path) -> Result<()> {
    let expected = fs::metadata(src)
        .with_context(|| format!("stat {}", src.display()))?
        .len();
    match fs::rename(src, dest) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc_exdev()) => {
            fs::copy(src, dest)
                .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
            verify_size(dest, expected)?;
            fs::remove_file(src).with_context(|| format!("removing {}", src.display()))?;
            return Ok(());
        }
        Err(e) => {
            return Err(e)
                .with_context(|| format!("renaming {} to {}", src.display(), dest.display()));
        }
    }
    verify_size(dest, expected)
}

/// `EXDEV` on Linux.
const fn libc_exdev() -> i32 {
    18
}

fn verify_size(path: &Path, expected: u64) -> Result<()> {
    let actual = fs::metadata(path)
        .with_context(|| format!("verifying {}", path.display()))?
        .len();
    if actual != expected {
        bail!(
            "size mismatch after move: {} is {actual} bytes, expected {expected}",
            path.display()
        );
    }
    Ok(())
}

/// Append `lines` (each newline-terminated) to `path`, creating it if needed.
fn append_lines(path: &Path, lines: &[String]) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {} for append", path.display()))?;
    for line in lines {
        writeln!(f, "{line}")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Step 4: fold `_events.txt`

/// One event line of `_events.txt`:
/// `[2026/09/21 19:54] Bookmark General ("2026-09-21_19-54-00" at 2291)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventsLine {
    pub demo: String,
    pub tick: i64,
}

/// Parse the quoted demo name and tick out of an event line. `None` for anything else.
pub fn parse_events_line(line: &str) -> Option<EventsLine> {
    let start = line.rfind("(\"")? + 2;
    let rest = &line[start..];
    let name_end = rest.find("\" at ")?;
    let demo = &rest[..name_end];
    let tail = rest[name_end + 5..].trim_end();
    let tick_str = tail.strip_suffix(')')?;
    if demo.is_empty() || !line.trim_start().starts_with('[') {
        return None;
    }
    Some(EventsLine {
        demo: demo.to_string(),
        tick: tick_str.trim().parse().ok()?,
    })
}

/// What happens to one event line of the master file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fate {
    /// Folded into the archived demo at this position of `index.demos`.
    Fold(usize),
    /// The demo is still hot; the line stays in the master.
    Keep,
    /// The demo is gone: append to `events-orphans.txt`.
    Orphan,
}

/// Classify an event line against the in-memory index, the `.dem` stems currently on disk in
/// `tf/demos`, and the cutoff before which a demo must be archived (`now − age_hours`).
///
/// Only **archived** entries (`file` below `archive_dir`) fold: the wizard indexes hot demos
/// under the same ids, and their lines must stay in the master until they are moved.
pub fn classify_fold(
    line: &EventsLine,
    index: &Index,
    on_disk: &HashSet<String>,
    hot_after: NaiveDateTime,
    archive_dir: &Path,
) -> Fate {
    if let Some(pos) = index
        .demos
        .iter()
        .position(|d| d.original_name == line.demo)
    {
        return if index.demos[pos].is_archived(archive_dir) {
            Fate::Fold(pos)
        } else {
            Fate::Keep
        };
    }
    if on_disk.contains(&line.demo) {
        return Fate::Keep;
    }
    let Some(ts) = demo::parse_ds_name(&line.demo) else {
        return Fate::Orphan;
    };
    // Hand-renamed demo: the ds name is gone but the recording is in the index by its
    // `mtime − seconds` start time and the tick is one of its marks.
    let hit = index.demos.iter().position(|d| {
        (d.recorded_at - ts).num_seconds().abs() <= FOLD_MATCH_SLACK_SECS
            && d.events.iter().any(|e| e.raw_ticks.contains(&line.tick))
    });
    if let Some(pos) = hit {
        return if index.demos[pos].is_archived(archive_dir) {
            Fate::Fold(pos)
        } else {
            Fate::Keep
        };
    }
    // Younger than `age_hours`: cannot be archived yet, so a hand-renamed copy may still be
    // waiting in `tf/demos`. Keep the line until it can be matched.
    if ts > hot_after {
        return Fate::Keep;
    }
    Fate::Orphan
}

/// The plan for one fold pass, computed without touching the disk.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FoldPlan {
    /// `(index position, raw line)` in master order.
    pub folds: Vec<(usize, String)>,
    /// Raw lines for `events-orphans.txt`, in master order.
    pub orphans: Vec<String>,
    /// Non-separator lines that did not parse (kept in place).
    pub unparsed: Vec<String>,
    /// The rewritten master text.
    pub kept: String,
}

/// Plan the fold of `master` text. `>` lines separate blocks; a separator survives only when its
/// block keeps at least one line.
pub fn plan_fold(
    master: &str,
    index: &Index,
    on_disk: &HashSet<String>,
    hot_after: NaiveDateTime,
    archive_dir: &Path,
) -> FoldPlan {
    let mut plan = FoldPlan::default();
    let mut blocks: Vec<(bool, Vec<&str>)> = vec![(false, Vec::new())];
    for line in master.lines() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.trim() == ">" {
            blocks.push((true, Vec::new()));
            continue;
        }
        if trimmed.trim().is_empty() {
            continue;
        }
        match parse_events_line(trimmed) {
            Some(ev) => match classify_fold(&ev, index, on_disk, hot_after, archive_dir) {
                Fate::Fold(pos) => plan.folds.push((pos, trimmed.to_string())),
                Fate::Orphan => plan.orphans.push(trimmed.to_string()),
                Fate::Keep => blocks.last_mut().expect("seeded").1.push(trimmed),
            },
            None => {
                plan.unparsed.push(trimmed.to_string());
                blocks.last_mut().expect("seeded").1.push(trimmed);
            }
        }
    }
    let mut kept = String::new();
    for (has_sep, lines) in blocks {
        if lines.is_empty() {
            continue;
        }
        if has_sep {
            kept.push_str(">\n");
        }
        for l in lines {
            kept.push_str(l);
            kept.push('\n');
        }
    }
    plan.kept = kept;
    plan
}

/// Stems of every `*.dem` directly inside `demos_dir` (after the moves of this run).
fn on_disk_stems(demos_dir: &Path) -> Result<HashSet<String>> {
    Ok(list_demos(demos_dir)?
        .iter()
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn fold_master(
    cfg: &Config,
    archive_path: &Path,
    demos_dir: &Path,
    index: &Index,
    rep: &mut Reporter,
    stats: &mut Stats,
    write: bool,
) -> Result<()> {
    let master_path = cfg.events_master_path();
    let master = match fs::read_to_string(&master_path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", master_path.display())),
    };
    let on_disk = on_disk_stems(demos_dir)?;
    let hot_after = Local::now().naive_local()
        - chrono::Duration::milliseconds((cfg.age_hours * 3_600_000.0) as i64);
    let plan = plan_fold(&master, index, &on_disk, hot_after, &cfg.archive_dir);

    for (pos, raw) in &plan.folds {
        let entry = &index.demos[*pos];
        let ev = parse_events_line(raw).expect("planned lines parse");
        let archived = Path::new(&entry.file)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        rep.line(format!("FOLD {} at {} -> {archived}", ev.demo, ev.tick));
    }
    for raw in &plan.orphans {
        rep.line(format!("ORPHAN {raw}"));
    }
    for raw in &plan.unparsed {
        rep.line(format!("SKIP unparsed line: {raw}"));
    }
    stats.folded += plan.folds.len();
    stats.orphans += plan.orphans.len();
    stats.skipped += plan.unparsed.len();

    let changed = !plan.folds.is_empty() || !plan.orphans.is_empty();
    if !write || !changed {
        return Ok(());
    }
    // Day files in index order so each file is opened once.
    let mut by_entry: Vec<(usize, Vec<String>)> = Vec::new();
    for (pos, raw) in &plan.folds {
        match by_entry.iter_mut().find(|(p, _)| p == pos) {
            Some((_, lines)) => lines.push(format!("# ds: {raw}")),
            None => by_entry.push((*pos, vec![format!("# ds: {raw}")])),
        }
    }
    for (pos, lines) in by_entry {
        let entry = &index.demos[pos];
        let day_file = archive_path
            .join(day_dir(entry.recorded_at))
            .join("events.txt");
        append_lines(&day_file, &lines)?;
    }
    append_lines(&archive_path.join("events-orphans.txt"), &plan.orphans)?;
    write_atomic(&master_path, &plan.kept)?;
    Ok(())
}

/// Write `text` to `<path>.tmp` and rename it over `path`.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let result = (|| -> Result<()> {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
            .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

// ---------------------------------------------------------------------------------------------
// Step 5: by-label symlinks

/// `(label directory, link file name)` for one link of `entry`.
///
/// Unlabelled (`event == None`): `("unlabelled", "<archived stem>.dem")`. A labelled event:
/// `(<label>, "<YYYY-MM-DD>_<map>_t<tick>_r<rating or 0>.dem")`.
pub fn by_label_link_name(entry: &DemoEntry, event: Option<&Event>) -> (String, String) {
    match event.and_then(|e| e.label.as_deref().map(|l| (l, e))) {
        Some((label, e)) => (
            label.to_string(),
            format!(
                "{}_{}_t{}_r{}.dem",
                entry.recorded_at.format("%Y-%m-%d"),
                entry.map,
                e.tick,
                e.rating.unwrap_or(0)
            ),
        ),
        None => {
            let stem = Path::new(&entry.file)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            ("unlabelled".to_string(), format!("{stem}.dem"))
        }
    }
}

/// Every link `entry` gets: one per labelled event, or a single unlabelled one.
pub fn by_label_links(entry: &DemoEntry) -> Vec<(String, String)> {
    let labelled: Vec<(String, String)> = entry
        .events
        .iter()
        .filter(|e| e.label.is_some())
        .map(|e| by_label_link_name(entry, Some(e)))
        .collect();
    if labelled.is_empty() {
        vec![by_label_link_name(entry, None)]
    } else {
        labelled
    }
}

/// Symlink target from `<archive>/by-label/<label>/` to the demo: `../../2026/09/21/x.dem` for
/// an archived demo, `../../../<stem>.dem` for one still in `tf/demos`. `file` is the index path
/// (relative to `tf_dir`, or absolute); `archive_dir` the configured archive root. When one is
/// absolute and the other relative no relative form exists, so the absolute path is used.
pub fn relative_link_target(file: &str, archive_dir: &Path, tf_dir: &Path) -> PathBuf {
    let file = Path::new(file);
    let link_dir = archive_dir.join("by-label").join("label");
    if file.is_absolute() != link_dir.is_absolute() {
        return if file.is_absolute() {
            file.to_path_buf()
        } else {
            tf_dir.join(file)
        };
    }
    let from: Vec<_> = link_dir.components().collect();
    let to: Vec<_> = file.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for c in &to[common..] {
        out.push(c);
    }
    out
}

/// Rebuild `<archive>/by-label/` from `index` right now (the wizard calls this after every save
/// so a fresh label shows up without waiting for the nightly run). Returns the link count.
pub fn rebuild_by_label(cfg: &Config, index: &Index) -> Result<usize> {
    let mut rep = Reporter {
        dry_run: false,
        echo: false,
        lines: Vec::new(),
    };
    let mut stats = Stats::default();
    regenerate_by_label(cfg, &cfg.archive_path(), index, &mut rep, &mut stats, true)?;
    Ok(stats.links)
}

fn regenerate_by_label(
    cfg: &Config,
    archive_path: &Path,
    index: &Index,
    rep: &mut Reporter,
    stats: &mut Stats,
    write: bool,
) -> Result<()> {
    let root = archive_path.join("by-label");
    if write && root.exists() {
        fs::remove_dir_all(&root).with_context(|| format!("removing {}", root.display()))?;
    }
    for entry in &index.demos {
        // A link to a file that is gone would dangle; dry runs plan links for files not moved yet.
        if write && !cfg.tf_dir.join(&entry.file).is_file() {
            rep.line(format!("SKIP link, file missing {}", entry.file));
            stats.skipped += 1;
            continue;
        }
        let target = relative_link_target(&entry.file, &cfg.archive_dir, &cfg.tf_dir);
        for (label, name) in by_label_links(entry) {
            rep.line(format!(
                "LINK by-label/{label}/{name} -> {}",
                target.display()
            ));
            if write {
                let dir = root.join(&label);
                fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
                let link = dir.join(&name);
                std::os::unix::fs::symlink(&target, &link)
                    .with_context(|| format!("linking {}", link.display()))?;
            }
            stats.links += 1;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const BADWATER_HDR: &[u8] = include_bytes!("../tests/fixtures/2026-09-21_19-51-20.hdr");
    const BADWATER_JSON: &str = include_str!("../tests/fixtures/2026-09-21_19-51-20.json");
    const TIGHT_HDR: &[u8] = include_bytes!("../tests/fixtures/Tight_scout_m.hdr");
    const TIGHT_JSON: &str = include_str!("../tests/fixtures/Tight_scout_m.json");
    const THUNDER_HDR: &[u8] = include_bytes!("../tests/fixtures/2026-09-21_20-42-43.hdr");
    const PIER_HDR: &[u8] = include_bytes!("../tests/fixtures/2026-08-16_23-04-42.hdr");
    const PIER_JSON: &str = include_str!("../tests/fixtures/2026-08-16_23-04-42.json");

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tf2demos-archive-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn event(tick: i64, raw: &[i64], label: Option<&str>, rating: Option<u8>) -> Event {
        Event {
            tick,
            presses: raw.len() as u32,
            raw_ticks: raw.to_vec(),
            label: label.map(str::to_string),
            class: None,
            rating,
            streak: None,
        }
    }

    fn entry(stem: &str, map: &str, recorded: NaiveDateTime, events: Vec<Event>) -> DemoEntry {
        DemoEntry {
            id: stem.into(),
            file: format!(
                "demos/archive/{}/{}",
                recorded.format("%Y/%m/%d"),
                new_demo_name(stem, map)
            ),
            original_name: stem.into(),
            map: map.into(),
            server: "srv".into(),
            recorded_at: recorded,
            seconds: 1.0,
            ticks: 1,
            state: State::Hot,
            frozen_in: None,
            reviewed: false,
            events,
        }
    }

    fn test_index() -> Index {
        let mut ix = Index::new(&[]);
        ix.upsert(entry(
            "2026-09-21_19-51-20",
            "pl_badwater",
            at(2026, 9, 21, 19, 51, 20),
            vec![event(6964, &[6964], None, None)],
        ));
        ix.upsert(entry(
            "Tight_scout_m",
            "pl_badwater",
            at(2026, 9, 21, 19, 54, 1),
            vec![event(2291, &[2291], None, None)],
        ));
        ix
    }

    /// `hot_after` value meaning "no ds timestamp is younger than age_hours".
    const NOTHING_HOT: NaiveDateTime = NaiveDateTime::MAX;
    const ARCHIVE: &str = "demos/archive";

    #[test]
    fn parse_events_line_positive() {
        let l = parse_events_line(
            r#"[2026/09/21 19:54] Bookmark General ("2026-09-21_19-54-00" at 2291)"#,
        )
        .unwrap();
        assert_eq!(l.demo, "2026-09-21_19-54-00");
        assert_eq!(l.tick, 2291);
        let l =
            parse_events_line(r#"[2026/09/21 19:54] Killstreak 5 ("Tight_scout_m" at 7)"#).unwrap();
        assert_eq!(l.demo, "Tight_scout_m");
        assert_eq!(l.tick, 7);
        // Trailing whitespace / CR tolerated.
        assert!(parse_events_line("[x] B G (\"a\" at 1)\r").is_some());
    }

    #[test]
    fn parse_events_line_negative() {
        assert!(parse_events_line(">").is_none());
        assert!(parse_events_line("").is_none());
        assert!(parse_events_line("garbage").is_none());
        assert!(parse_events_line(r#"[x] Bookmark General ("a" at nope)"#).is_none());
        assert!(parse_events_line(r#"[x] Bookmark General ("a" at 1"#).is_none());
        assert!(parse_events_line(r#"[x] Bookmark General ("" at 1)"#).is_none());
        assert!(parse_events_line(r#"Bookmark General ("a" at 1)"#).is_none());
    }

    #[test]
    fn new_name_rule() {
        assert_eq!(
            new_demo_name("2026-09-21_19-51-20", "pl_badwater"),
            "2026-09-21_19-51-20_pl_badwater.dem"
        );
        assert_eq!(
            new_demo_name("Tight_scout_m", "pl_badwater"),
            "Tight_scout_m_pl_badwater.dem"
        );
    }

    #[test]
    fn recorded_at_rule() {
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(
            recorded_at("2026-09-21_19-51-20", mtime, 104.67),
            at(2026, 9, 21, 19, 51, 20)
        );
        let end: DateTime<Local> = mtime.into();
        // 36.79 s before the end, floored to the second.
        let want = (end - chrono::Duration::milliseconds(36_790))
            .naive_local()
            .with_nanosecond(0)
            .unwrap();
        assert_eq!(recorded_at("Tight_scout_m", mtime, 36.79), want);
        assert_eq!(recorded_at("x", mtime, 0.0).nanosecond(), 0);
    }

    #[test]
    fn age_is_negative_for_future_mtime() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(7200);
        assert!((age_hours(now, SystemTime::UNIX_EPOCH) - 2.0).abs() < 1e-9);
        let future = now + Duration::from_secs(3600);
        assert!(age_hours(now, future) < 0.0);
    }

    #[test]
    fn by_label_names() {
        let e = entry(
            "2026-09-21_19-51-20",
            "pl_badwater",
            at(2026, 9, 21, 19, 51, 20),
            vec![],
        );
        assert_eq!(
            by_label_link_name(&e, None),
            (
                "unlabelled".to_string(),
                "2026-09-21_19-51-20_pl_badwater.dem".to_string()
            )
        );
        let labelled = event(6964, &[6964], Some("matador"), Some(4));
        assert_eq!(
            by_label_link_name(&e, Some(&labelled)),
            (
                "matador".to_string(),
                "2026-09-21_pl_badwater_t6964_r4.dem".to_string()
            )
        );
        let unrated = event(10, &[10], Some("surf stab"), None);
        assert_eq!(
            by_label_link_name(&e, Some(&unrated)),
            (
                "surf stab".to_string(),
                "2026-09-21_pl_badwater_t10_r0.dem".to_string()
            )
        );
        // An event without a label counts as unlabelled.
        let plain = event(1, &[1], None, Some(3));
        assert_eq!(by_label_link_name(&e, Some(&plain)).0, "unlabelled");
    }

    #[test]
    fn by_label_links_per_entry() {
        let mut e = entry(
            "Tight_scout_m",
            "pl_badwater",
            at(2026, 9, 21, 19, 54, 1),
            vec![event(2291, &[2291], None, None)],
        );
        assert_eq!(
            by_label_links(&e),
            [(
                "unlabelled".to_string(),
                "Tight_scout_m_pl_badwater.dem".to_string()
            )]
        );
        e.events = vec![
            event(10, &[10], Some("c-tap"), Some(2)),
            event(20, &[20], None, None),
            event(30, &[30], Some("matador"), None),
        ];
        let links = by_label_links(&e);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].0, "c-tap");
        assert_eq!(links[0].1, "2026-09-21_pl_badwater_t10_r2.dem");
        assert_eq!(links[1].0, "matador");
        assert_eq!(links[1].1, "2026-09-21_pl_badwater_t30_r0.dem");
    }

    #[test]
    fn link_target_is_relative() {
        let t = relative_link_target(
            "demos/archive/2026/09/21/x.dem",
            Path::new("demos/archive"),
            Path::new("/tf"),
        );
        assert_eq!(t, PathBuf::from("../../2026/09/21/x.dem"));
        let t = relative_link_target(
            "/elsewhere/2026/09/21/x.dem",
            Path::new("/elsewhere"),
            Path::new("/tf"),
        );
        assert_eq!(t, PathBuf::from("../../2026/09/21/x.dem"));
        // Hot demo in tf/demos: three levels up from by-label/<label>/.
        let t = relative_link_target(
            "demos/2026-09-21_19-51-20.dem",
            Path::new("demos/archive"),
            Path::new("/tf"),
        );
        assert_eq!(t, PathBuf::from("../../../2026-09-21_19-51-20.dem"));
        let t = relative_link_target("other/x.dem", Path::new("demos/archive"), Path::new("/tf"));
        assert_eq!(t, PathBuf::from("../../../../other/x.dem"));
        // Mixed absolute/relative: no relative form, use the absolute path.
        let t = relative_link_target("demos/x.dem", Path::new("/abs/archive"), Path::new("/tf"));
        assert_eq!(t, PathBuf::from("/tf/demos/x.dem"));
        let t = relative_link_target("/abs/x.dem", Path::new("demos/archive"), Path::new("/tf"));
        assert_eq!(t, PathBuf::from("/abs/x.dem"));
    }

    #[test]
    fn classify_by_original_name_then_disk_then_tick_then_orphan() {
        let ix = test_index();
        let disk: HashSet<String> = ["2026-09-21_20-42-43".to_string()].into();
        let line = |demo: &str, tick| EventsLine {
            demo: demo.into(),
            tick,
        };
        assert_eq!(
            classify_fold(
                &line("2026-09-21_19-51-20", 6964),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Fold(0)
        );
        // Original name wins even with a foreign tick.
        assert_eq!(
            classify_fold(
                &line("2026-09-21_19-51-20", 1),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Fold(0)
        );
        assert_eq!(
            classify_fold(
                &line("2026-09-21_20-42-43", 5),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Keep
        );
        // Hand-renamed: ds timestamp within 5 s of recorded_at and tick in raw_ticks.
        assert_eq!(
            classify_fold(
                &line("2026-09-21_19-54-00", 2291),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Fold(1)
        );
        assert_eq!(
            classify_fold(
                &line("2026-09-21_19-54-05", 2291),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Fold(1)
        );
        assert_eq!(
            classify_fold(
                &line("2026-09-21_19-54-07", 2291),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Orphan
        );
        assert_eq!(
            classify_fold(
                &line("2026-09-21_19-54-00", 2292),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Orphan
        );
        assert_eq!(
            classify_fold(
                &line("2026-09-20_17-22-21", 601),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Orphan
        );
        assert_eq!(
            classify_fold(
                &line("Nope", 601),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Orphan
        );
        // A ds line younger than age_hours may belong to a still-hot hand-renamed demo: keep.
        assert_eq!(
            classify_fold(
                &line("2026-09-20_17-22-21", 601),
                &ix,
                &disk,
                at(2026, 9, 20, 0, 0, 0),
                Path::new(ARCHIVE)
            ),
            Fate::Keep
        );
    }

    #[test]
    fn classify_keeps_lines_of_hot_indexed_demos() {
        // The wizard indexed both demos while hot: same ids, files still in tf/demos.
        let mut ix = test_index();
        for d in &mut ix.demos {
            d.file = format!("demos/{}.dem", d.id);
        }
        let disk: HashSet<String> = [
            "2026-09-21_19-51-20".to_string(),
            "Tight_scout_m".to_string(),
        ]
        .into();
        let line = |demo: &str, tick| EventsLine {
            demo: demo.into(),
            tick,
        };
        assert_eq!(
            classify_fold(
                &line("2026-09-21_19-51-20", 6964),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Keep
        );
        // Hand-renamed hot demo matched by timestamp + tick: still keep.
        assert_eq!(
            classify_fold(
                &line("2026-09-21_19-54-00", 2291),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Keep
        );
        // Once archived (file below archive_dir) the same lines fold.
        for d in &mut ix.demos {
            d.file = format!("demos/archive/2026/09/21/{}_pl_badwater.dem", d.id);
        }
        assert_eq!(
            classify_fold(
                &line("2026-09-21_19-51-20", 6964),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Fold(0)
        );
        assert_eq!(
            classify_fold(
                &line("2026-09-21_19-54-00", 2291),
                &ix,
                &disk,
                NOTHING_HOT,
                Path::new(ARCHIVE)
            ),
            Fate::Fold(1)
        );
    }

    #[test]
    fn plan_fold_rewrites_master_blocks() {
        let ix = test_index();
        let disk: HashSet<String> = ["2026-09-21_20-42-43".to_string()].into();
        let master = "\
>
[2026/09/21 19:53] Bookmark General (\"2026-09-21_19-51-20\" at 6964)
>
[2026/09/20 17:22] Bookmark General (\"2026-09-20_17-22-21\" at 601)
[2026/09/20 17:22] Bookmark General (\"2026-09-20_17-22-21\" at 615)
>
[2026/09/21 20:50] Bookmark General (\"2026-09-21_20-42-43\" at 100)
what is this
>
[2026/09/21 19:54] Bookmark General (\"2026-09-21_19-54-00\" at 2291)
";
        let plan = plan_fold(master, &ix, &disk, NOTHING_HOT, Path::new(ARCHIVE));
        assert_eq!(plan.folds.len(), 2);
        assert_eq!(plan.folds[0].0, 0);
        assert_eq!(plan.folds[1].0, 1);
        assert!(plan.folds[1].1.contains("19-54-00"));
        assert_eq!(plan.orphans.len(), 2);
        assert_eq!(plan.unparsed, ["what is this"]);
        assert_eq!(
            plan.kept,
            ">\n[2026/09/21 20:50] Bookmark General (\"2026-09-21_20-42-43\" at 100)\nwhat is this\n"
        );

        // Everything folded: the master becomes empty.
        let plan = plan_fold(
            ">\n[2026/09/21 19:53] Bookmark General (\"2026-09-21_19-51-20\" at 6964)\n",
            &ix,
            &disk,
            NOTHING_HOT,
            Path::new(ARCHIVE),
        );
        assert_eq!(plan.kept, "");
        // Lines before the first separator keep no separator.
        let plan = plan_fold(
            "[2026/09/21 20:50] Bookmark General (\"2026-09-21_20-42-43\" at 100)\n>\n",
            &ix,
            &disk,
            NOTHING_HOT,
            Path::new(ARCHIVE),
        );
        assert_eq!(
            plan.kept,
            "[2026/09/21 20:50] Bookmark General (\"2026-09-21_20-42-43\" at 100)\n"
        );
        assert_eq!(
            plan_fold("", &ix, &disk, NOTHING_HOT, Path::new(ARCHIVE)),
            FoldPlan::default()
        );
    }

    // ---- end to end on a temp tree built from the fixtures ----------------------------------

    struct Tree {
        root: PathBuf,
        cfg: Config,
    }

    fn set_mtime(path: &Path, when: SystemTime) {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn local_time(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> SystemTime {
        use chrono::TimeZone;
        Local
            .from_local_datetime(&at(y, mo, d, h, mi, s))
            .single()
            .unwrap()
            .into()
    }

    fn build_tree(tag: &str) -> Tree {
        let root = temp_dir(tag);
        let demos = root.join("tf/demos");
        fs::create_dir_all(&demos).unwrap();
        let old = local_time(2026, 9, 21, 19, 53, 5);
        let put = |name: &str, bytes: &[u8], when: SystemTime| {
            let p = demos.join(name);
            fs::write(&p, bytes).unwrap();
            set_mtime(&p, when);
        };
        put(
            "2026-08-16_23-04-42.dem",
            PIER_HDR,
            local_time(2026, 8, 16, 23, 12, 51),
        );
        put("2026-08-16_23-04-42.json", PIER_JSON.as_bytes(), old);
        put("2026-09-21_19-51-20.dem", BADWATER_HDR, old);
        put("2026-09-21_19-51-20.json", BADWATER_JSON.as_bytes(), old);
        // Hand-renamed: mtime 19:54:38 − 36.79 s → recorded_at 19:54:01.
        put(
            "Tight_scout_m.dem",
            TIGHT_HDR,
            local_time(2026, 9, 21, 19, 54, 38),
        );
        put("Tight_scout_m.json", TIGHT_JSON.as_bytes(), old);
        // Sidecar-less leftover: deleted.
        let mut junk = THUNDER_HDR.to_vec();
        junk.extend_from_slice(&[0; 4096]);
        put(
            "2026-09-20_12-00-00.dem",
            &junk,
            local_time(2026, 9, 20, 12, 5, 0),
        );
        // Still recording (mtime in the future): too new.
        put(
            "2026-09-21_20-42-43.dem",
            THUNDER_HDR,
            SystemTime::now() + Duration::from_secs(3600),
        );
        // Not a demo: ignored.
        put("notes.txt", b"hi", old);
        fs::write(
            demos.join("_events.txt"),
            "\
>
[2026/08/16 23:10] Bookmark General (\"2026-08-16_23-04-42\" at 24405)
>
[2026/09/20 17:22] Bookmark General (\"2026-09-20_17-22-21\" at 601)
[2026/09/20 17:22] Bookmark General (\"2026-09-20_17-22-21\" at 615)
>
[2026/09/21 19:53] Bookmark General (\"2026-09-21_19-51-20\" at 6964)
>
[2026/09/21 19:54] Bookmark General (\"2026-09-21_19-54-00\" at 2291)
",
        )
        .unwrap();
        let cfg = Config::parse(&format!(
            "tf_dir = {:?}\nage_hours = 0\n",
            root.join("tf").to_string_lossy()
        ))
        .unwrap();
        Tree { root, cfg }
    }

    fn lines_starting(rep: &Report, prefix: &str) -> Vec<String> {
        rep.lines
            .iter()
            .map(|l| l.trim_start_matches("[dry-run] ").to_string())
            .filter(|l| l.starts_with(prefix))
            .collect()
    }

    fn snapshot(root: &Path) -> Vec<String> {
        walkdir::WalkDir::new(root)
            .sort_by_file_name()
            .into_iter()
            .map(|e| {
                let e = e.unwrap();
                let rel = e.path().strip_prefix(root).unwrap().display().to_string();
                let meta = e.metadata().unwrap();
                format!("{rel} {}", if meta.is_file() { meta.len() } else { 0 })
            })
            .collect()
    }

    #[test]
    fn dry_run_plans_everything_and_writes_nothing() {
        let t = build_tree("dry");
        let before = snapshot(&t.root);
        let rep = organize_with(&t.cfg, true, false, false).unwrap();
        assert_eq!(snapshot(&t.root), before, "dry-run wrote something");
        assert!(rep.lines.iter().all(|l| l.starts_with("[dry-run] ")));
        assert_eq!(
            lines_starting(&rep, "MOVE"),
            [
                "MOVE 2026-08-16_23-04-42.dem -> demos/archive/2026/08/16/2026-08-16_23-04-42_pl_pier.dem",
                "MOVE 2026-09-21_19-51-20.dem -> demos/archive/2026/09/21/2026-09-21_19-51-20_pl_badwater.dem",
                "MOVE Tight_scout_m.dem -> demos/archive/2026/09/21/Tight_scout_m_pl_badwater.dem",
            ]
        );
        assert_eq!(
            lines_starting(&rep, "DELETE"),
            ["DELETE 2026-09-20_12-00-00.dem (5168 bytes)"]
        );
        let skips = lines_starting(&rep, "SKIP");
        assert_eq!(skips.len(), 1);
        assert!(skips[0].starts_with("SKIP too new 2026-09-21_20-42-43.dem"));
        assert_eq!(
            lines_starting(&rep, "FOLD"),
            [
                "FOLD 2026-08-16_23-04-42 at 24405 -> 2026-08-16_23-04-42_pl_pier.dem",
                "FOLD 2026-09-21_19-51-20 at 6964 -> 2026-09-21_19-51-20_pl_badwater.dem",
                "FOLD 2026-09-21_19-54-00 at 2291 -> Tight_scout_m_pl_badwater.dem",
            ]
        );
        assert_eq!(lines_starting(&rep, "ORPHAN").len(), 2);
        assert_eq!(
            lines_starting(&rep, "LINK"),
            [
                "LINK by-label/unlabelled/2026-08-16_23-04-42_pl_pier.dem -> ../../2026/08/16/2026-08-16_23-04-42_pl_pier.dem",
                "LINK by-label/unlabelled/2026-09-21_19-51-20_pl_badwater.dem -> ../../2026/09/21/2026-09-21_19-51-20_pl_badwater.dem",
                "LINK by-label/unlabelled/Tight_scout_m_pl_badwater.dem -> ../../2026/09/21/Tight_scout_m_pl_badwater.dem",
            ]
        );
        assert_eq!(
            rep.lines.last().unwrap(),
            "[dry-run] DONE moved=3 deleted=1 folded=3 orphans=2 links=3 skipped=1 pruned=0"
        );
        fs::remove_dir_all(&t.root).unwrap();
    }

    #[test]
    fn real_run_then_idempotent_rerun() {
        let t = build_tree("real");
        let rep = organize_with(&t.cfg, false, false, false).unwrap();
        assert_eq!(
            rep.stats,
            Stats {
                moved: 3,
                deleted: 1,
                folded: 3,
                orphans: 2,
                links: 3,
                skipped: 1,
                pruned: 0,
            }
        );
        let demos = t.root.join("tf/demos");
        let arch = demos.join("archive");
        assert!(!demos.join("2026-09-20_12-00-00.dem").exists());
        assert!(demos.join("2026-09-21_20-42-43.dem").exists());
        assert!(demos.join("notes.txt").exists());
        assert!(!demos.join("Tight_scout_m.dem").exists());
        assert!(!demos.join("Tight_scout_m.json").exists());
        assert_eq!(
            fs::metadata(arch.join("2026/09/21/Tight_scout_m_pl_badwater.dem"))
                .unwrap()
                .len(),
            TIGHT_HDR.len() as u64
        );
        assert!(
            arch.join("2026/09/21/Tight_scout_m_pl_badwater.json")
                .is_file()
        );
        assert!(
            arch.join("2026/08/16/2026-08-16_23-04-42_pl_pier.dem")
                .is_file()
        );
        assert_eq!(fs::read_to_string(demos.join("_events.txt")).unwrap(), "");
        assert!(!demos.join("_events.txt.tmp").exists());
        assert_eq!(
            fs::read_to_string(arch.join("2026/08/16/events.txt")).unwrap(),
            "2026-08-16_23-04-42_pl_pier.dem  tick=24405 presses=1 label=- class=- rating=- streak=-\n\
             # ds: [2026/08/16 23:10] Bookmark General (\"2026-08-16_23-04-42\" at 24405)\n"
        );
        assert_eq!(
            fs::read_to_string(arch.join("2026/09/21/events.txt")).unwrap(),
            "2026-09-21_19-51-20_pl_badwater.dem  tick=6964 presses=1 label=- class=- rating=- streak=-\n\
             Tight_scout_m_pl_badwater.dem  tick=2291 presses=1 label=- class=- rating=- streak=-\n\
             # ds: [2026/09/21 19:53] Bookmark General (\"2026-09-21_19-51-20\" at 6964)\n\
             # ds: [2026/09/21 19:54] Bookmark General (\"2026-09-21_19-54-00\" at 2291)\n"
        );
        assert_eq!(
            fs::read_to_string(arch.join("events-orphans.txt")).unwrap(),
            "[2026/09/20 17:22] Bookmark General (\"2026-09-20_17-22-21\" at 601)\n\
             [2026/09/20 17:22] Bookmark General (\"2026-09-20_17-22-21\" at 615)\n"
        );
        let ix = Index::load_or_new(&t.cfg.index_path(), &[]).unwrap();
        assert_eq!(ix.demos.len(), 3);
        let tight = ix.by_original_name("Tight_scout_m").unwrap();
        assert_eq!(tight.recorded_at, at(2026, 9, 21, 19, 54, 1));
        assert_eq!(
            tight.file,
            "demos/archive/2026/09/21/Tight_scout_m_pl_badwater.dem"
        );
        assert_eq!(tight.map, "pl_badwater");
        assert_eq!(tight.events.len(), 1);
        assert_eq!(tight.events[0].raw_ticks, [2291]);
        assert_eq!(ix.labels, t.cfg.seed_labels);
        for name in [
            "2026-08-16_23-04-42_pl_pier.dem",
            "2026-09-21_19-51-20_pl_badwater.dem",
            "Tight_scout_m_pl_badwater.dem",
        ] {
            let link = arch.join("by-label/unlabelled").join(name);
            let target = fs::read_link(&link).unwrap();
            assert!(target.starts_with("../.."), "{}", target.display());
            assert!(fs::metadata(&link).is_ok(), "{name} does not resolve");
        }

        // Second run: nothing changes.
        let index_before = fs::read(t.cfg.index_path()).unwrap();
        let before = snapshot(&t.root);
        let rep = organize_with(&t.cfg, false, false, false).unwrap();
        assert_eq!(
            rep.stats,
            Stats {
                links: 3,
                skipped: 1,
                ..Stats::default()
            }
        );
        assert_eq!(snapshot(&t.root), before);
        assert_eq!(fs::read(t.cfg.index_path()).unwrap(), index_before);
        assert!(rep.lines.iter().all(|l| {
            l.starts_with("SKIP too new") || l.starts_with("LINK") || l.starts_with("DONE")
        }));
        fs::remove_dir_all(&t.root).unwrap();
    }

    #[test]
    fn tf2_running_skips_the_fold_atomically() {
        let t = build_tree("tf2");
        let master_before = fs::read_to_string(t.root.join("tf/demos/_events.txt")).unwrap();
        let rep = organize_with(&t.cfg, false, true, false).unwrap();
        assert_eq!(rep.stats.moved, 3);
        assert_eq!(rep.stats.folded, 0);
        assert_eq!(rep.stats.orphans, 0);
        assert!(rep.lines.contains(&"SKIP fold, TF2 running".to_string()));
        let arch = t.root.join("tf/demos/archive");
        assert_eq!(
            fs::read_to_string(t.root.join("tf/demos/_events.txt")).unwrap(),
            master_before
        );
        assert!(!arch.join("events-orphans.txt").exists());
        assert!(
            !fs::read_to_string(arch.join("2026/09/21/events.txt"))
                .unwrap()
                .contains("# ds:")
        );

        let rep = organize_with(&t.cfg, false, false, false).unwrap();
        assert_eq!(rep.stats.moved, 0);
        assert_eq!(rep.stats.folded, 3);
        assert_eq!(rep.stats.orphans, 2);
        assert_eq!(
            fs::read_to_string(t.root.join("tf/demos/_events.txt")).unwrap(),
            ""
        );
        let day = fs::read_to_string(arch.join("2026/09/21/events.txt")).unwrap();
        assert_eq!(day.matches("# ds:").count(), 2);
        assert_eq!(
            fs::read_to_string(arch.join("events-orphans.txt"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        fs::remove_dir_all(&t.root).unwrap();
    }

    /// The session-2 invariant: `organize` after `review` loses nothing.
    #[test]
    fn labels_on_hot_entries_survive_organize() {
        let t = build_tree("labels");
        let demos = t.root.join("tf/demos");
        // The wizard indexed two hot demos and labelled one event each; a third hot entry's
        // file was hand-deleted (unlabelled → pruned); a fourth was labelled then deleted (kept).
        let mut ix = Index::new(&t.cfg.seed_labels);
        let mut bad = entry(
            "2026-09-21_19-51-20",
            "pl_badwater",
            at(2026, 9, 21, 19, 51, 20),
            vec![event(6964, &[6964], Some("matador"), Some(4))],
        );
        bad.file = "demos/2026-09-21_19-51-20.dem".into();
        bad.reviewed = true;
        bad.events[0].class = Some("spy".into());
        bad.events[0].streak = Some(2);
        ix.upsert(bad);
        let mut tight = entry(
            "Tight_scout_m",
            "pl_badwater",
            at(2026, 9, 21, 19, 54, 1),
            vec![event(2291, &[2291], Some("surf stab"), None)],
        );
        tight.file = "demos/Tight_scout_m.dem".into();
        ix.upsert(tight);
        let mut gone = entry(
            "gone",
            "pl_x",
            at(2026, 9, 1, 0, 0, 0),
            vec![event(1, &[1], None, None)],
        );
        gone.file = "demos/gone.dem".into();
        ix.upsert(gone);
        let mut gone_labelled = entry(
            "gone_labelled",
            "pl_x",
            at(2026, 9, 2, 0, 0, 0),
            vec![event(1, &[1], Some("c-tap"), None)],
        );
        gone_labelled.file = "demos/gone_labelled.dem".into();
        ix.upsert(gone_labelled);
        ix.add_label("free text");
        ix.last_class = Some("spy".into());
        ix.save(&t.cfg.index_path()).unwrap();
        // Hot by-label links exist before the run (as the wizard would have made them).
        rebuild_by_label(&t.cfg, &ix).unwrap();
        assert!(
            fs::metadata(
                t.root
                    .join("tf/demos/archive/by-label/matador/2026-09-21_pl_badwater_t6964_r4.dem")
            )
            .is_ok(),
            "hot labelled link resolves before organize"
        );

        let rep = organize_with(&t.cfg, false, false, false).unwrap();
        assert_eq!(rep.stats.moved, 3);
        assert_eq!(rep.stats.pruned, 1);
        assert!(
            rep.lines
                .iter()
                .any(|l| l == "PRUNE gone (hot entry, file gone)"),
            "{:?}",
            rep.lines
        );
        assert!(
            rep.lines
                .iter()
                .any(|l| l.starts_with("SKIP missing gone_labelled"))
        );
        assert_eq!(rep.stats.folded, 3, "archived lines fold as before");

        let ix = Index::load_or_new(&t.cfg.index_path(), &[]).unwrap();
        let ids: Vec<&str> = ix.demos.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "2026-08-16_23-04-42",
                "gone_labelled",
                "2026-09-21_19-51-20",
                "Tight_scout_m"
            ]
        );
        let bad = ix.by_id("2026-09-21_19-51-20").unwrap();
        assert_eq!(
            bad.file,
            "demos/archive/2026/09/21/2026-09-21_19-51-20_pl_badwater.dem"
        );
        assert!(bad.reviewed);
        assert_eq!(bad.events[0].label.as_deref(), Some("matador"));
        assert_eq!(bad.events[0].class.as_deref(), Some("spy"));
        assert_eq!(bad.events[0].rating, Some(4));
        assert_eq!(bad.events[0].streak, Some(2));
        let tight = ix.by_id("Tight_scout_m").unwrap();
        assert_eq!(
            tight.file,
            "demos/archive/2026/09/21/Tight_scout_m_pl_badwater.dem"
        );
        assert_eq!(tight.events[0].label.as_deref(), Some("surf stab"));
        assert!(!tight.reviewed);
        assert_eq!(ix.labels.last().map(String::as_str), Some("free text"));
        assert_eq!(ix.last_class.as_deref(), Some("spy"));
        // Day log carries the labels; links moved from the hot path to the archive.
        let day = fs::read_to_string(demos.join("archive/2026/09/21/events.txt")).unwrap();
        assert!(
            day.contains("tick=6964 presses=1 label=matador class=spy rating=4 streak=2"),
            "{day}"
        );
        let link = demos.join("archive/by-label/matador/2026-09-21_pl_badwater_t6964_r4.dem");
        assert_eq!(
            fs::read_link(&link).unwrap(),
            PathBuf::from("../../2026/09/21/2026-09-21_19-51-20_pl_badwater.dem")
        );
        assert!(fs::metadata(&link).is_ok(), "link resolves after the move");
        assert!(
            demos
                .join("archive/by-label/surf stab/2026-09-21_pl_badwater_t2291_r0.dem")
                .exists()
        );
        assert!(
            !demos
                .join("archive/by-label/unlabelled/Tight_scout_m_pl_badwater.dem")
                .exists()
        );
        // The labelled-but-missing entry gets no dangling link.
        assert!(!demos.join("archive/by-label/c-tap").exists());
        fs::remove_dir_all(&t.root).unwrap();
    }

    /// Lines of hot demos indexed by the wizard stay in `_events.txt` until the demo moves.
    #[test]
    fn fold_keeps_lines_of_hot_indexed_demos_end_to_end() {
        let t = build_tree("hotfold");
        let cfg = Config::parse(&format!(
            "tf_dir = {:?}\nage_hours = 1000000\n",
            t.root.join("tf").to_string_lossy()
        ))
        .unwrap();
        let mut ix = Index::new(&[]);
        let mut bad = entry(
            "2026-09-21_19-51-20",
            "pl_badwater",
            at(2026, 9, 21, 19, 51, 20),
            vec![event(6964, &[6964], Some("matador"), None)],
        );
        bad.file = "demos/2026-09-21_19-51-20.dem".into();
        ix.upsert(bad);
        let mut tight = entry(
            "Tight_scout_m",
            "pl_badwater",
            at(2026, 9, 21, 19, 54, 1),
            vec![event(2291, &[2291], None, None)],
        );
        tight.file = "demos/Tight_scout_m.dem".into();
        ix.upsert(tight);
        ix.save(&cfg.index_path()).unwrap();
        let master_before = fs::read_to_string(t.root.join("tf/demos/_events.txt")).unwrap();
        let rep = organize_with(&cfg, false, false, false).unwrap();
        assert_eq!(rep.stats.moved, 0);
        assert_eq!(rep.stats.folded, 0);
        assert!(lines_starting(&rep, "FOLD").is_empty());
        // Nothing aged, so nothing changed in the master except the (age-independent) orphans.
        let after = fs::read_to_string(t.root.join("tf/demos/_events.txt")).unwrap();
        assert!(after.contains("\"2026-09-21_19-51-20\" at 6964"), "{after}");
        assert!(after.contains("\"2026-09-21_19-54-00\" at 2291"), "{after}");
        assert!(master_before.len() >= after.len());
        // Hot links point three levels up and resolve.
        let link = t
            .root
            .join("tf/demos/archive/by-label/matador/2026-09-21_pl_badwater_t6964_r0.dem");
        assert_eq!(
            fs::read_link(&link).unwrap(),
            PathBuf::from("../../../2026-09-21_19-51-20.dem")
        );
        assert!(fs::metadata(&link).is_ok());
        fs::remove_dir_all(&t.root).unwrap();
    }

    #[test]
    fn missing_demos_dir_refuses_unless_dry_run() {
        let root = temp_dir("missing");
        let cfg = Config::parse(&format!(
            "tf_dir = {:?}\n",
            root.join("nope").to_string_lossy()
        ))
        .unwrap();
        let err = organize_with(&cfg, false, false, false).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        let rep = organize_with(&cfg, true, false, false).unwrap();
        assert_eq!(rep.stats, Stats::default());
        assert!(!root.join("nope").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn existing_destination_is_never_overwritten() {
        let t = build_tree("exists");
        let dest_dir = t.root.join("tf/demos/archive/2026/09/21");
        fs::create_dir_all(&dest_dir).unwrap();
        fs::write(dest_dir.join("Tight_scout_m_pl_badwater.dem"), b"other").unwrap();
        let rep = organize_with(&t.cfg, false, false, false).unwrap();
        assert!(lines_starting(&rep, "SKIP exists").len() == 1);
        assert!(t.root.join("tf/demos/Tight_scout_m.dem").exists());
        assert_eq!(
            fs::read(dest_dir.join("Tight_scout_m_pl_badwater.dem")).unwrap(),
            b"other"
        );
        assert_eq!(rep.stats.moved, 2);
        fs::remove_dir_all(&t.root).unwrap();
    }
}
