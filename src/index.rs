//! `<archive>/index.json` — the only mutable state the tool owns.
//!
//! Schema: HANDOFF §4 with the event field `length_s` replaced by `streak` (an integer kill
//! streak / combo length), plus top-level `labels` (seeded from the config's `seed_labels` on
//! first creation, growable afterwards) and `last_class`.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

pub const INDEX_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    pub demos: Vec<DemoEntry>,
    /// Live, growable label list; the config's `seed_labels` are only the seed.
    pub labels: Vec<String>,
    /// Class chosen in the most recent labelling, offered as the default next time.
    pub last_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DemoEntry {
    /// Equal to `original_name`: the stem as found on disk at archive time.
    pub id: String,
    /// Relative to `tf_dir`, e.g. `demos/archive/2026/09/21/2026-09-21_19-51-20_pl_badwater.dem`.
    pub file: String,
    /// Stem as found in `tf/demos`: `2026-09-21_19-51-20` (ds name) or `Tight_scout_m` (hand-renamed).
    pub original_name: String,
    pub map: String,
    pub server: String,
    /// Serialized as `2026-09-21T19:51:20`.
    pub recorded_at: NaiveDateTime,
    pub seconds: f32,
    pub ticks: i32,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_in: Option<String>,
    pub reviewed: bool,
    pub events: Vec<Event>,
}

impl DemoEntry {
    /// True once `file` lies below the configured archive root (relative to `tf_dir`, or
    /// absolute when `archive_dir` is). Hot demos indexed by the review wizard live in
    /// `demos/<stem>.dem` and are not archived.
    pub fn is_archived(&self, archive_dir: &Path) -> bool {
        Path::new(&self.file).starts_with(archive_dir)
    }

    /// Stem of the file as it is on disk now (`2026-09-21_19-51-20_pl_badwater` or, while hot,
    /// `2026-09-21_19-51-20`).
    pub fn file_stem(&self) -> String {
        Path::new(&self.file)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Hot,
    Frozen,
}

/// One grouped mark: `presses` key presses within `group_secs` of each other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub tick: i64,
    pub presses: u32,
    pub raw_ticks: Vec<i64>,
    /// Tags of the play (any number). Files written before tags existed carry a single
    /// `label` (string or null); both forms parse, and `labels` is what gets written.
    #[serde(default, alias = "label", deserialize_with = "one_or_many")]
    pub labels: Vec<String>,
    pub class: Option<String>,
    pub rating: Option<u8>,
    /// Kill streak / combo length of the play. Default `1` when labelled.
    pub streak: Option<u32>,
}

impl Event {
    pub fn is_labelled(&self) -> bool {
        !self.labels.is_empty()
    }

    /// `"-"` when unlabelled, else the tags joined with `sep`.
    pub fn labels_text(&self, sep: &str) -> String {
        if self.labels.is_empty() {
            "-".to_string()
        } else {
            self.labels.join(sep)
        }
    }

    /// Append `label` unless already present (case-sensitive). Returns whether it was added.
    pub fn add_label(&mut self, label: &str) -> bool {
        if self.labels.iter().any(|l| l == label) {
            return false;
        }
        self.labels.push(label.to_string());
        true
    }
}

/// `null` → `[]`, `"x"` → `["x"]`, `["x", "y"]` → as is.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        One(Option<String>),
        Many(Vec<String>),
    }
    Ok(match Raw::deserialize(d)? {
        Raw::One(None) => Vec::new(),
        Raw::One(Some(s)) => vec![s],
        Raw::Many(v) => v,
    })
}

/// Read just enough to reject foreign schemas before the full parse.
#[derive(Deserialize)]
struct VersionProbe {
    version: u32,
}

impl Index {
    pub fn new(seed_labels: &[String]) -> Index {
        Index {
            version: INDEX_VERSION,
            demos: Vec::new(),
            labels: seed_labels.to_vec(),
            last_class: None,
        }
    }

    /// Load `path` if it exists, else [`Index::new`]. A `version` other than 1 is an error.
    pub fn load_or_new(path: &Path, seed_labels: &[String]) -> Result<Index> {
        if !path.exists() {
            return Ok(Index::new(seed_labels));
        }
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Parse JSON text, checking `version` first so a foreign schema gets a clear error.
    pub fn parse(text: &str) -> Result<Index> {
        let probe: VersionProbe = serde_json::from_str(text).context("reading index version")?;
        if probe.version != INDEX_VERSION {
            bail!(
                "unsupported index version {} (this build understands version {})",
                probe.version,
                INDEX_VERSION
            );
        }
        Ok(serde_json::from_str(text)?)
    }

    /// Atomic save: create parent dirs, write `<path>.tmp` (pretty JSON + trailing newline),
    /// fsync, then rename over `path`.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = Path::new(&tmp);
        let result = (|| -> Result<()> {
            let mut text = serde_json::to_string_pretty(self)?;
            text.push('\n');
            let mut f =
                fs::File::create(tmp).with_context(|| format!("creating {}", tmp.display()))?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(tmp, path)
                .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }

    /// Lookup by `id`.
    pub fn by_id(&self, id: &str) -> Option<&DemoEntry> {
        self.demos.iter().find(|d| d.id == id)
    }

    /// Mutable lookup by `id`.
    pub fn by_id_mut(&mut self, id: &str) -> Option<&mut DemoEntry> {
        self.demos.iter_mut().find(|d| d.id == id)
    }

    /// Lookup by the on-disk stem at archive time; `organize` matches via positions instead.
    #[allow(dead_code)]
    pub fn by_original_name(&self, name: &str) -> Option<&DemoEntry> {
        self.demos.iter().find(|d| d.original_name == name)
    }

    /// Replace the entry with the same `id`, or push. `demos` stays sorted by `(recorded_at, id)`.
    ///
    /// Blind replacement: labels on the old entry are lost. `organize` uses
    /// [`Index::merge_archived`] instead; this stays for callers that build entries from scratch.
    pub fn upsert(&mut self, entry: DemoEntry) {
        match self.demos.iter_mut().find(|d| d.id == entry.id) {
            Some(existing) => *existing = entry,
            None => self.demos.push(entry),
        }
        self.sort();
    }

    fn sort(&mut self) {
        self.demos
            .sort_by(|a, b| (a.recorded_at, &a.id).cmp(&(b.recorded_at, &b.id)));
    }

    /// Record that `fresh` (built from the header and sidecar at archive time) has just been
    /// archived, **keeping every label** the review wizard may already have stored.
    ///
    /// The existing entry is found by `id`, or — when the user hand-renamed the demo after it
    /// was reviewed while hot — by an identical header (`server`, `map`, `ticks`, `seconds`) on a
    /// not-yet-archived entry whose `file` is gone (`stale_ids` lists those). File-derived fields
    /// (`file`, `map`, `server`, `recorded_at`, `seconds`, `ticks`, `id`, `original_name`) come
    /// from `fresh`; `label/class/rating/streak` are copied per event matched by `tick`, else by
    /// any shared `raw_ticks` entry; `reviewed` and `state`/`frozen_in` are kept.
    pub fn merge_archived(&mut self, fresh: DemoEntry, stale_ids: &[String]) {
        let pos = self
            .demos
            .iter()
            .position(|d| d.id == fresh.id)
            .or_else(|| {
                self.demos.iter().position(|d| {
                    stale_ids.contains(&d.id)
                        && d.server == fresh.server
                        && d.map == fresh.map
                        && d.ticks == fresh.ticks
                        && d.seconds == fresh.seconds
                })
            });
        match pos {
            Some(pos) => {
                let old = std::mem::replace(&mut self.demos[pos], fresh);
                let new = &mut self.demos[pos];
                new.reviewed = old.reviewed;
                new.state = old.state;
                new.frozen_in = old.frozen_in;
                let mut used = vec![false; old.events.len()];
                for ev in &mut new.events {
                    let hit = old
                        .events
                        .iter()
                        .position(|o| o.tick == ev.tick)
                        .or_else(|| {
                            old.events
                                .iter()
                                .position(|o| o.raw_ticks.iter().any(|t| ev.raw_ticks.contains(t)))
                        });
                    if let Some(i) = hit {
                        let o = &old.events[i];
                        ev.labels = o.labels.clone();
                        ev.class = o.class.clone();
                        ev.rating = o.rating;
                        ev.streak = o.streak;
                        used[i] = true;
                    }
                }
                // A labelled event that the fresh sidecar no longer lists is kept rather than lost.
                for (o, used) in old.events.into_iter().zip(used) {
                    if !used && o.is_labelled() {
                        new.events.push(o);
                    }
                }
                new.events.sort_by_key(|e| e.tick);
            }
            None => self.demos.push(fresh),
        }
        self.sort();
    }

    /// Positions of the entries that still hold labels somewhere.
    #[allow(dead_code)]
    pub fn has_labels(entry: &DemoEntry) -> bool {
        entry.events.iter().any(Event::is_labelled)
    }

    /// Add a label unless an identical (case-sensitive) one exists. Returns whether it was added.
    pub fn add_label(&mut self, label: &str) -> bool {
        if self.labels.iter().any(|l| l == label) {
            return false;
        }
        self.labels.push(label.to_string());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tf2demos-index-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    fn badwater() -> DemoEntry {
        DemoEntry {
            id: "2026-09-21_19-51-20".into(),
            file: "demos/archive/2026/09/21/2026-09-21_19-51-20_pl_badwater.dem".into(),
            original_name: "2026-09-21_19-51-20".into(),
            map: "pl_badwater".into(),
            server: "169.254.240.159:13144".into(),
            recorded_at: at(2026, 9, 21, 19, 51, 20),
            seconds: 104.7,
            ticks: 6978,
            state: State::Hot,
            frozen_in: None,
            reviewed: true,
            events: vec![Event {
                tick: 6964,
                presses: 1,
                raw_ticks: vec![6964],
                labels: vec!["matador".into()],
                class: Some("spy".into()),
                rating: Some(4),
                streak: Some(1),
            }],
        }
    }

    fn entry(id: &str, recorded_at: NaiveDateTime) -> DemoEntry {
        DemoEntry {
            id: id.into(),
            original_name: id.into(),
            recorded_at,
            reviewed: false,
            events: vec![],
            ..badwater()
        }
    }

    const HANDOFF_JSON: &str = r#"{
  "version": 1,
  "demos": [{
    "id": "2026-09-21_19-51-20",
    "file": "demos/archive/2026/09/21/2026-09-21_19-51-20_pl_badwater.dem",
    "original_name": "2026-09-21_19-51-20",
    "map": "pl_badwater", "server": "169.254.240.159:13144",
    "recorded_at": "2026-09-21T19:51:20", "seconds": 104.7, "ticks": 6978,
    "state": "hot",
    "reviewed": true,
    "events": [{
      "tick": 6964, "presses": 1, "raw_ticks": [6964],
      "labels": ["matador"], "class": "spy", "rating": 4, "streak": 1
    }]
  }],
  "labels": ["surf stab", "c-tap", "matador"],
  "last_class": "spy"
}"#;

    fn seeded() -> Index {
        let mut ix = Index::new(&["surf stab".into(), "c-tap".into(), "matador".into()]);
        ix.demos.push(badwater());
        ix.last_class = Some("spy".into());
        ix
    }

    #[test]
    fn round_trips_handoff_shape() {
        let parsed = Index::parse(HANDOFF_JSON).unwrap();
        assert_eq!(parsed, seeded());
        let out: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&seeded()).unwrap()).unwrap();
        let expected: serde_json::Value = serde_json::from_str(HANDOFF_JSON).unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn single_label_files_still_parse() {
        let old = HANDOFF_JSON.replace(r#""labels": ["matador"]"#, r#""label": "matador""#);
        assert_ne!(old, HANDOFF_JSON);
        assert_eq!(Index::parse(&old).unwrap(), seeded());
        let none = HANDOFF_JSON.replace(r#""labels": ["matador"]"#, r#""label": null"#);
        assert!(
            Index::parse(&none).unwrap().demos[0].events[0]
                .labels
                .is_empty()
        );
        let many = HANDOFF_JSON.replace(r#""labels": ["matador"]"#, r#""labels": ["a", "b"]"#);
        assert_eq!(
            Index::parse(&many).unwrap().demos[0].events[0].labels,
            ["a", "b"]
        );
        let missing = HANDOFF_JSON.replace(r#""labels": ["matador"], "#, "");
        assert!(
            Index::parse(&missing).unwrap().demos[0].events[0]
                .labels
                .is_empty()
        );
    }

    #[test]
    fn event_label_helpers() {
        let mut e = Event {
            tick: 1,
            presses: 1,
            raw_ticks: vec![1],
            labels: vec![],
            class: None,
            rating: None,
            streak: None,
        };
        assert!(!e.is_labelled());
        assert_eq!(e.labels_text(", "), "-");
        assert!(e.add_label("a"));
        assert!(!e.add_label("a"));
        assert!(e.add_label("b"));
        assert_eq!(e.labels_text(", "), "a, b");
        assert!(e.is_labelled());
    }

    #[test]
    fn recorded_at_and_state_render_plainly() {
        let v = serde_json::to_value(badwater()).unwrap();
        assert_eq!(v["recorded_at"], "2026-09-21T19:51:20");
        assert_eq!(v["state"], "hot");
        assert!(
            v.get("frozen_in").is_none(),
            "frozen_in is skipped when None"
        );
        let mut frozen = badwater();
        frozen.state = State::Frozen;
        frozen.frozen_in = Some("2026-09-21.zip".into());
        let v = serde_json::to_value(frozen).unwrap();
        assert_eq!(v["state"], "frozen");
        assert_eq!(v["frozen_in"], "2026-09-21.zip");
    }

    #[test]
    fn unlabelled_event_renders_nulls() {
        let e = Event {
            tick: 1,
            presses: 1,
            raw_ticks: vec![1],
            labels: vec![],
            class: None,
            rating: None,
            streak: None,
        };
        let v = serde_json::to_value(e).unwrap();
        for key in ["class", "rating", "streak"] {
            assert!(v[key].is_null(), "{key} must be present as null");
        }
        assert_eq!(v["labels"], serde_json::json!([]));
    }

    #[test]
    fn save_then_load_equals_and_is_atomic() {
        let dir = temp_dir("save");
        let path = dir.join("nested").join("index.json");
        let ix = seeded();
        ix.save(&path).unwrap();
        let names: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["index.json"], "no .tmp left behind");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("}\n"));
        assert!(text.contains("\n  \"version\": 1,"), "pretty-printed");
        let back = Index::load_or_new(&path, &[]).unwrap();
        assert_eq!(back, ix);
        // Saving again overwrites in place.
        ix.save(&path).unwrap();
        assert_eq!(Index::load_or_new(&path, &[]).unwrap(), ix);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_or_new_missing_path_seeds_labels() {
        let dir = temp_dir("missing");
        let seeds = vec!["surf stab".to_string(), "c-tap".to_string()];
        let ix = Index::load_or_new(&dir.join("index.json"), &seeds).unwrap();
        assert_eq!(ix.version, 1);
        assert!(ix.demos.is_empty());
        assert_eq!(ix.labels, seeds);
        assert_eq!(ix.last_class, None);
        assert!(!dir.join("index.json").exists(), "load never writes");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn version_2_errors() {
        let err = Index::parse(r#"{"version": 2, "demos": [], "labels": [], "last_class": null}"#)
            .unwrap_err();
        assert!(err.to_string().contains("version 2"), "{err}");
        let dir = temp_dir("v2");
        let path = dir.join("index.json");
        fs::write(&path, r#"{"version": 2}"#).unwrap();
        let err = Index::load_or_new(&path, &[]).unwrap_err();
        assert!(format!("{err:#}").contains("version 2"), "{err:#}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn upsert_replaces_by_id_and_keeps_order() {
        let mut ix = Index::new(&[]);
        ix.upsert(entry("c", at(2026, 9, 21, 12, 0, 0)));
        ix.upsert(entry("a", at(2026, 9, 20, 12, 0, 0)));
        ix.upsert(entry("b", at(2026, 9, 21, 12, 0, 0)));
        let ids: Vec<&str> = ix.demos.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);

        let mut replacement = entry("c", at(2026, 9, 19, 12, 0, 0));
        replacement.map = "pl_upward".into();
        ix.upsert(replacement);
        assert_eq!(ix.demos.len(), 3);
        let ids: Vec<&str> = ix.demos.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "b"]);
        assert_eq!(ix.by_id("c").unwrap().map, "pl_upward");
    }

    #[test]
    fn lookups() {
        let mut ix = Index::new(&[]);
        let mut e = badwater();
        e.id = "Tight_scout_m".into();
        e.original_name = "Tight_scout_m".into();
        ix.upsert(e);
        ix.upsert(badwater());
        assert_eq!(ix.by_id("Tight_scout_m").unwrap().map, "pl_badwater");
        assert_eq!(
            ix.by_original_name("2026-09-21_19-51-20").unwrap().id,
            "2026-09-21_19-51-20"
        );
        assert!(ix.by_id("nope").is_none());
        assert!(ix.by_original_name("nope").is_none());
    }

    fn labelled_hot() -> DemoEntry {
        let mut e = badwater();
        e.file = "demos/2026-09-21_19-51-20.dem".into();
        e.events.push(Event {
            tick: 9000,
            presses: 1,
            raw_ticks: vec![9000],
            labels: vec![],
            class: None,
            rating: None,
            streak: None,
        });
        e
    }

    fn fresh_archived() -> DemoEntry {
        let mut e = badwater();
        e.reviewed = false;
        e.events = vec![
            Event {
                tick: 6964,
                presses: 1,
                raw_ticks: vec![6964],
                labels: vec![],
                class: None,
                rating: None,
                streak: None,
            },
            Event {
                tick: 9000,
                presses: 1,
                raw_ticks: vec![9000],
                labels: vec![],
                class: None,
                rating: None,
                streak: None,
            },
        ];
        e
    }

    #[test]
    fn is_archived_by_file_prefix() {
        let hot = labelled_hot();
        assert!(!hot.is_archived(Path::new("demos/archive")));
        assert_eq!(hot.file_stem(), "2026-09-21_19-51-20");
        let arch = badwater();
        assert!(arch.is_archived(Path::new("demos/archive")));
        assert_eq!(arch.file_stem(), "2026-09-21_19-51-20_pl_badwater");
        let mut abs = badwater();
        abs.file = "/elsewhere/2026/09/21/x.dem".into();
        assert!(abs.is_archived(Path::new("/elsewhere")));
        assert!(!abs.is_archived(Path::new("demos/archive")));
    }

    #[test]
    fn merge_archived_keeps_labels_and_reviewed() {
        let mut ix = Index::new(&[]);
        ix.upsert(labelled_hot());
        ix.merge_archived(fresh_archived(), &[]);
        assert_eq!(ix.demos.len(), 1);
        let d = &ix.demos[0];
        assert!(
            d.is_archived(Path::new("demos/archive")),
            "file updated: {}",
            d.file
        );
        assert!(d.reviewed, "reviewed kept");
        assert_eq!(d.events.len(), 2);
        assert_eq!(d.events[0].labels, ["matador"]);
        assert_eq!(d.events[0].class.as_deref(), Some("spy"));
        assert_eq!(d.events[0].rating, Some(4));
        assert_eq!(d.events[0].streak, Some(1));
        assert!(d.events[1].labels.is_empty());
    }

    #[test]
    fn merge_archived_matches_by_raw_tick_and_keeps_orphaned_labels() {
        let mut ix = Index::new(&[]);
        let mut old = labelled_hot();
        // Labelled group starts at 6960 with 6964 inside; the fresh sidecar groups from 6964.
        old.events[0].tick = 6960;
        old.events[0].raw_ticks = vec![6960, 6964];
        old.events.push(Event {
            tick: 12000,
            presses: 1,
            raw_ticks: vec![12000],
            labels: vec!["c-tap".into()],
            class: None,
            rating: None,
            streak: None,
        });
        ix.upsert(old);
        ix.merge_archived(fresh_archived(), &[]);
        let d = &ix.demos[0];
        let ticks: Vec<i64> = d.events.iter().map(|e| e.tick).collect();
        assert_eq!(ticks, [6964, 9000, 12000]);
        assert_eq!(d.events[0].labels, ["matador"]);
        assert_eq!(d.events[2].labels, ["c-tap"]);
    }

    #[test]
    fn merge_archived_adopts_hand_renamed_stale_entry() {
        let mut ix = Index::new(&[]);
        ix.upsert(labelled_hot());
        let mut fresh = fresh_archived();
        fresh.id = "Tight_scout_m".into();
        fresh.original_name = "Tight_scout_m".into();
        fresh.file = "demos/archive/2026/09/21/Tight_scout_m_pl_badwater.dem".into();
        // Not stale: a second entry appears.
        let mut probe = ix.clone();
        probe.merge_archived(fresh.clone(), &[]);
        assert_eq!(probe.demos.len(), 2);
        // Stale (its file is gone) and the header matches: the entry is adopted with its labels.
        ix.merge_archived(fresh, &["2026-09-21_19-51-20".to_string()]);
        assert_eq!(ix.demos.len(), 1);
        assert_eq!(ix.demos[0].id, "Tight_scout_m");
        assert_eq!(ix.demos[0].events[0].labels, ["matador"]);
        assert!(ix.by_id("2026-09-21_19-51-20").is_none());
    }

    #[test]
    fn merge_archived_new_entry_is_pushed_sorted() {
        let mut ix = Index::new(&[]);
        ix.upsert(entry("b", at(2026, 9, 21, 12, 0, 0)));
        ix.merge_archived(entry("a", at(2026, 9, 20, 12, 0, 0)), &[]);
        let ids: Vec<&str> = ix.demos.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn add_label_dedups_case_sensitively() {
        let mut ix = Index::new(&["matador".into()]);
        assert!(!ix.add_label("matador"));
        assert!(ix.add_label("Matador"));
        assert!(ix.add_label("trickstab"));
        assert!(!ix.add_label("trickstab"));
        assert_eq!(ix.labels, ["matador", "Matador", "trickstab"]);
    }
}
