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
    pub label: Option<String>,
    pub class: Option<String>,
    pub rating: Option<u8>,
    /// Kill streak / combo length of the play. Default `1` when labelled.
    pub streak: Option<u32>,
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

    /// Lookup used by the labelling wizard (later session).
    #[allow(dead_code)]
    pub fn by_id(&self, id: &str) -> Option<&DemoEntry> {
        self.demos.iter().find(|d| d.id == id)
    }

    /// Lookup by the on-disk stem at archive time; `organize` matches via positions instead.
    #[allow(dead_code)]
    pub fn by_original_name(&self, name: &str) -> Option<&DemoEntry> {
        self.demos.iter().find(|d| d.original_name == name)
    }

    /// Replace the entry with the same `id`, or push. `demos` stays sorted by `(recorded_at, id)`.
    pub fn upsert(&mut self, entry: DemoEntry) {
        match self.demos.iter_mut().find(|d| d.id == entry.id) {
            Some(existing) => *existing = entry,
            None => self.demos.push(entry),
        }
        self.demos
            .sort_by(|a, b| (a.recorded_at, &a.id).cmp(&(b.recorded_at, &b.id)));
    }

    /// Add a label unless an identical (case-sensitive) one exists. Returns whether it was added.
    /// Used by the labelling wizard (later session).
    #[allow(dead_code)]
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
                label: Some("matador".into()),
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
      "label": "matador", "class": "spy", "rating": 4, "streak": 1
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
            label: None,
            class: None,
            rating: None,
            streak: None,
        };
        let v = serde_json::to_value(e).unwrap();
        for key in ["label", "class", "rating", "streak"] {
            assert!(v[key].is_null(), "{key} must be present as null");
        }
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
