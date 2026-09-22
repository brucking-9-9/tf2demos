//! The review queue and the display-free labelling session behind `tf2demos review`.
//!
//! Hot demos (still in `tf/demos`, under `age_hours` old) are indexed here with
//! `file = "demos/<stem>.dem"` and the same `id` `organize` will use later, so a label set now
//! survives the nightly move ([`Index::merge_archived`]). Every write goes through
//! [`Session::apply`], which **reloads `index.json` first** and patches one event: the nightly
//! `organize` may rewrite the file while the wizard is open, and nothing here may undo that.
//!
//! Queue = every event with `label == null` in demos with `reviewed == false`, oldest first.
//! *Skip* leaves the event unlabelled; a demo becomes `reviewed` once each of its events is
//! labelled or was skipped in this session, so skipped marks do not come back on the next run.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::NaiveDateTime;

use crate::archive;
use crate::config::Config;
use crate::demo::{self, Header, Sidecar};
use crate::index::{DemoEntry, Index, State};

/// One card of the wizard: an unlabelled event plus what the card shows about its demo.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub demo_id: String,
    pub tick: i64,
    pub presses: u32,
    pub map: String,
    pub recorded_at: NaiveDateTime,
    /// Demo length from the header.
    pub demo_seconds: f32,
}

impl Card {
    /// `mm:ss` into the demo (`tick / 66.6667`).
    pub fn offset(&self) -> String {
        format_offset(self.tick)
    }
}

/// `tick` as `mm:ss` (hours fold into the minutes; demos are short).
pub fn format_offset(tick: i64) -> String {
    let secs = (tick.max(0) as f64 / demo::TICKS_PER_SEC).round() as i64;
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// What one card's answer looks like.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub label: String,
    pub class: Option<String>,
    pub rating: Option<u8>,
    /// Blank in the wizard → 1.
    pub streak: u32,
}

/// Result of [`scan`]: the index as loaded plus every hot demo not yet in it.
#[derive(Debug)]
pub struct Scan {
    pub index: Index,
    /// Ids added by the scan (not yet saved).
    pub added: Vec<String>,
}

/// Load `index.json` and add every hot demo of `tf/demos` that is not in it yet: has a `.json`
/// sidecar, a readable header, and `ticks > 0` (a demo still recording is skipped). Nothing is
/// written.
pub fn scan(cfg: &Config) -> Result<Scan> {
    let index = Index::load_or_new(&cfg.index_path(), &cfg.seed_labels)?;
    scan_into(cfg, index)
}

fn scan_into(cfg: &Config, mut index: Index) -> Result<Scan> {
    let mut added = Vec::new();
    for path in archive::list_demos(&cfg.demos_dir())? {
        let Some(entry) = hot_entry(cfg, &path, &index) else {
            continue;
        };
        added.push(entry.id.clone());
        index.upsert(entry);
    }
    Ok(Scan { index, added })
}

/// Index entry for a hot demo at `path`, or `None` when it is already indexed, has no sidecar,
/// is still recording, or cannot be read.
fn hot_entry(cfg: &Config, path: &Path, index: &Index) -> Option<DemoEntry> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    if index.by_id(&stem).is_some() {
        return None;
    }
    let sidecar_path = path.with_extension("json");
    if !sidecar_path.is_file() {
        return None;
    }
    let header = match Header::read(path) {
        Ok(h) => h,
        Err(err) => {
            eprintln!("review: skipping {name}: {err:#}");
            return None;
        }
    };
    if header.is_in_progress() {
        return None;
    }
    let sidecar = match Sidecar::read(&sidecar_path) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("review: skipping {name}: {err:#}");
            return None;
        }
    };
    let mtime = fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let recorded_at = archive::recorded_at(&stem, mtime, header.seconds);
    Some(DemoEntry {
        id: stem.clone(),
        file: Path::new("demos")
            .join(&name)
            .to_string_lossy()
            .into_owned(),
        original_name: stem,
        map: header.map,
        server: header.server,
        recorded_at,
        seconds: header.seconds,
        ticks: header.ticks,
        state: State::Hot,
        frozen_in: None,
        reviewed: false,
        events: archive::events_from_sidecar(&sidecar, cfg.group_secs),
    })
}

/// Every unlabelled event of every unreviewed demo, oldest demo first, ticks ascending.
pub fn queue(index: &Index) -> Vec<Card> {
    let mut cards: Vec<Card> = index
        .demos
        .iter()
        .filter(|d| !d.reviewed)
        .flat_map(|d| {
            d.events
                .iter()
                .filter(|e| e.label.is_none())
                .map(move |e| Card {
                    demo_id: d.id.clone(),
                    tick: e.tick,
                    presses: e.presses,
                    map: d.map.clone(),
                    recorded_at: d.recorded_at,
                    demo_seconds: d.seconds,
                })
        })
        .collect();
    cards.sort_by(|a, b| {
        (a.recorded_at, &a.demo_id, a.tick).cmp(&(b.recorded_at, &b.demo_id, b.tick))
    });
    cards
}

/// `(demos, marks)` in a queue.
pub fn counts(cards: &[Card]) -> (usize, usize) {
    let demos: HashSet<&str> = cards.iter().map(|c| c.demo_id.as_str()).collect();
    (demos.len(), cards.len())
}

/// The wizard's state: the queue, the current position, and the in-memory index view.
pub struct Session {
    cfg: Config,
    index: Index,
    cards: Vec<Card>,
    pos: usize,
    skipped: HashSet<(String, i64)>,
}

impl Session {
    /// Scan, persist newly indexed hot demos (so `organize` sees them), and build the queue.
    pub fn open(cfg: &Config) -> Result<Session> {
        let scan = scan(cfg)?;
        let index = scan.index;
        if !scan.added.is_empty() {
            index.save(&cfg.index_path())?;
            archive::rebuild_by_label(cfg, &index)?;
        }
        let cards = queue(&index);
        Ok(Session {
            cfg: cfg.clone(),
            index,
            cards,
            pos: 0,
            skipped: HashSet::new(),
        })
    }

    pub fn current(&self) -> Option<&Card> {
        self.cards.get(self.pos)
    }

    /// `(1-based position, total)`; position is `total + 1` once done.
    pub fn progress(&self) -> (usize, usize) {
        (self.pos + 1, self.cards.len())
    }

    pub fn is_done(&self) -> bool {
        self.pos >= self.cards.len()
    }

    pub fn labels(&self) -> &[String] {
        &self.index.labels
    }

    pub fn classes(&self) -> &[String] {
        &self.cfg.classes
    }

    pub fn last_class(&self) -> Option<&str> {
        self.index.last_class.as_deref()
    }

    /// `tf/`-relative path of the current card's demo, as the index knows it *now* (the nightly
    /// run may have moved it since the card was built).
    pub fn current_file(&self) -> Option<String> {
        let card = self.current()?;
        self.index.by_id(&card.demo_id).map(|d| d.file.clone())
    }

    /// Label the current card and advance. Reloads the index from disk before patching.
    pub fn save(&mut self, answer: Answer) -> Result<()> {
        let Some(card) = self.current().cloned() else {
            bail!("review: nothing left to label");
        };
        let label = answer.label.trim().to_string();
        if label.is_empty() {
            bail!("review: label is empty");
        }
        let class = answer
            .class
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
        self.patch(&card, |ix, ev| {
            ev.label = Some(label.clone());
            ev.class = class.clone();
            ev.rating = answer.rating.map(|r| r.clamp(1, 5));
            ev.streak = Some(answer.streak.max(1));
            ix.add_label(&label);
            if class.is_some() {
                ix.last_class = class.clone();
            }
        })?;
        self.pos += 1;
        Ok(())
    }

    /// Skip the current card and advance. The demo flips to `reviewed` once every event is
    /// labelled or skipped, which is saved right away.
    pub fn skip(&mut self) -> Result<()> {
        let Some(card) = self.current().cloned() else {
            bail!("review: nothing left to skip");
        };
        self.skipped.insert((card.demo_id.clone(), card.tick));
        self.patch(&card, |_, _| {})?;
        self.pos += 1;
        Ok(())
    }

    /// Reload, find the card's event, apply `f`, recompute `reviewed`, save atomically, rebuild
    /// `by-label/`, and keep the reloaded index as the session's view.
    fn patch(
        &mut self,
        card: &Card,
        f: impl FnOnce(&mut Index, &mut crate::index::Event),
    ) -> Result<()> {
        let path = self.cfg.index_path();
        let mut ix = Index::load_or_new(&path, &self.cfg.seed_labels)?;
        if ix.by_id(&card.demo_id).is_none() {
            // Pruned or never saved: reinsert our copy so the label has somewhere to live.
            let copy = self
                .index
                .by_id(&card.demo_id)
                .cloned()
                .with_context(|| format!("demo {} vanished from the index", card.demo_id))?;
            ix.upsert(copy);
        }
        let skipped = &self.skipped;
        {
            let entry = ix.by_id_mut(&card.demo_id).expect("entry was just ensured");
            let ev = entry
                .events
                .iter_mut()
                .find(|e| e.tick == card.tick || e.raw_ticks.contains(&card.tick))
                .with_context(|| {
                    format!("event at tick {} vanished from {}", card.tick, card.demo_id)
                })?;
            let mut ev_out = ev.clone();
            f(&mut ix, &mut ev_out);
            let entry = ix.by_id_mut(&card.demo_id).expect("entry was just ensured");
            let ev = entry
                .events
                .iter_mut()
                .find(|e| e.tick == card.tick || e.raw_ticks.contains(&card.tick))
                .expect("found a moment ago");
            *ev = ev_out;
            entry.reviewed = entry
                .events
                .iter()
                .all(|e| e.label.is_some() || skipped.contains(&(entry.id.clone(), e.tick)));
        }
        ix.save(&path)?;
        archive::rebuild_by_label(&self.cfg, &ix)?;
        self.index = ix;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Event;
    use chrono::NaiveDate;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    const BADWATER_HDR: &[u8] = include_bytes!("../tests/fixtures/2026-09-21_19-51-20.hdr");
    const BADWATER_JSON: &str = include_str!("../tests/fixtures/2026-09-21_19-51-20.json");
    const PHOENIX_HDR: &[u8] = include_bytes!("../tests/fixtures/2026-09-21_00-00-37.hdr");
    const PHOENIX_JSON: &str = include_str!("../tests/fixtures/2026-09-21_00-00-37.json");
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
            "tf2demos-review-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ev(tick: i64, label: Option<&str>) -> Event {
        Event {
            tick,
            presses: 1,
            raw_ticks: vec![tick],
            label: label.map(str::to_string),
            class: None,
            rating: None,
            streak: None,
        }
    }

    fn entry(id: &str, recorded: NaiveDateTime, events: Vec<Event>) -> DemoEntry {
        DemoEntry {
            id: id.into(),
            file: format!(
                "demos/archive/{}/{id}_pl_x.dem",
                recorded.format("%Y/%m/%d")
            ),
            original_name: id.into(),
            map: "pl_x".into(),
            server: "srv".into(),
            recorded_at: recorded,
            seconds: 100.0,
            ticks: 6666,
            state: State::Hot,
            frozen_in: None,
            reviewed: false,
            events,
        }
    }

    /// A scratch `tf/` with one archived demo already indexed and three hot demos on disk:
    /// badwater (1 mark), phoenix (3 marks → 1 event), thundermountain (in progress, no json).
    struct Tree {
        root: PathBuf,
        cfg: Config,
    }

    fn build_tree(tag: &str) -> Tree {
        let root = temp_dir(tag);
        let demos = root.join("tf/demos");
        fs::create_dir_all(demos.join("archive/2026/08/16")).unwrap();
        let put = |name: &str, bytes: &[u8]| fs::write(demos.join(name), bytes).unwrap();
        put("2026-09-21_19-51-20.dem", BADWATER_HDR);
        put("2026-09-21_19-51-20.json", BADWATER_JSON.as_bytes());
        put("2026-09-21_00-00-37.dem", PHOENIX_HDR);
        put("2026-09-21_00-00-37.json", PHOENIX_JSON.as_bytes());
        put("2026-09-21_20-42-43.dem", THUNDER_HDR);
        put("2026-09-21_20-42-43.json", b"{\"events\": []}");
        put("nojson.dem", BADWATER_HDR);
        fs::write(
            demos.join("archive/2026/08/16/2026-08-16_23-04-42_pl_pier.dem"),
            PIER_HDR,
        )
        .unwrap();
        fs::write(
            demos.join("archive/2026/08/16/2026-08-16_23-04-42_pl_pier.json"),
            PIER_JSON,
        )
        .unwrap();
        let cfg = Config::parse(&format!(
            "tf_dir = {:?}\nage_hours = 24\n",
            root.join("tf").to_string_lossy()
        ))
        .unwrap();
        let mut ix = Index::new(&cfg.seed_labels);
        let mut pier = entry(
            "2026-08-16_23-04-42",
            at(2026, 8, 16, 23, 4, 42),
            vec![ev(24405, None)],
        );
        pier.file = "demos/archive/2026/08/16/2026-08-16_23-04-42_pl_pier.dem".into();
        pier.map = "pl_pier".into();
        ix.upsert(pier);
        ix.save(&cfg.index_path()).unwrap();
        Tree { root, cfg }
    }

    #[test]
    fn offset_formats_mm_ss() {
        assert_eq!(format_offset(0), "00:00");
        assert_eq!(format_offset(6964), "01:44");
        assert_eq!(format_offset(48085), "12:01");
        assert_eq!(format_offset(-5), "00:00");
    }

    #[test]
    fn queue_is_unlabelled_events_of_unreviewed_demos_oldest_first() {
        let mut ix = Index::new(&[]);
        ix.upsert(entry(
            "b",
            at(2026, 9, 21, 12, 0, 0),
            vec![ev(300, None), ev(100, Some("x")), ev(200, None)],
        ));
        ix.upsert(entry("a", at(2026, 9, 20, 12, 0, 0), vec![ev(5, None)]));
        let mut reviewed = entry("c", at(2026, 9, 1, 0, 0, 0), vec![ev(1, None)]);
        reviewed.reviewed = true;
        ix.upsert(reviewed);
        let q = queue(&ix);
        let keys: Vec<(&str, i64)> = q.iter().map(|c| (c.demo_id.as_str(), c.tick)).collect();
        assert_eq!(keys, [("a", 5), ("b", 200), ("b", 300)]);
        assert_eq!(counts(&q), (2, 3));
        assert_eq!(counts(&[]), (0, 0));
    }

    #[test]
    fn scan_adds_hot_demos_without_writing() {
        let t = build_tree("scan");
        let before = fs::read(t.cfg.index_path()).unwrap();
        let s = scan(&t.cfg).unwrap();
        assert_eq!(s.added, ["2026-09-21_00-00-37", "2026-09-21_19-51-20"]);
        assert_eq!(fs::read(t.cfg.index_path()).unwrap(), before, "scan wrote");
        let bad = s.index.by_id("2026-09-21_19-51-20").unwrap();
        assert_eq!(bad.file, "demos/2026-09-21_19-51-20.dem");
        assert_eq!(bad.map, "pl_badwater");
        assert_eq!(bad.recorded_at, at(2026, 9, 21, 19, 51, 20));
        assert_eq!(bad.ticks, 6978);
        assert!(!bad.reviewed);
        assert_eq!(bad.events.len(), 1);
        assert_eq!(bad.events[0].tick, 6964);
        let ph = s.index.by_id("2026-09-21_00-00-37").unwrap();
        assert_eq!(ph.events.len(), 1);
        assert_eq!(ph.events[0].presses, 3);
        assert!(
            s.index.by_id("2026-09-21_20-42-43").is_none(),
            "in progress"
        );
        assert!(s.index.by_id("nojson").is_none());
        // Idempotent: a second scan over the merged index adds nothing.
        let again = scan_into(&t.cfg, s.index.clone()).unwrap();
        assert!(again.added.is_empty());
        assert_eq!(again.index, s.index);
        let q = queue(&s.index);
        let ids: Vec<&str> = q.iter().map(|c| c.demo_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "2026-08-16_23-04-42",
                "2026-09-21_00-00-37",
                "2026-09-21_19-51-20"
            ]
        );
        assert_eq!(q[2].offset(), "01:44");
        assert!((q[2].demo_seconds - 104.67).abs() < 0.01);
        fs::remove_dir_all(&t.root).unwrap();
    }

    #[test]
    fn session_labels_and_skips_with_reload_between() {
        let t = build_tree("session");
        let mut s = Session::open(&t.cfg).unwrap();
        // Opening persisted the hot entries and their unlabelled links.
        let on_disk = Index::load_or_new(&t.cfg.index_path(), &[]).unwrap();
        assert_eq!(on_disk.demos.len(), 3);
        let hot_link = t
            .root
            .join("tf/demos/archive/by-label/unlabelled/2026-09-21_19-51-20.dem");
        assert!(
            fs::metadata(&hot_link).is_ok(),
            "hot unlabelled link resolves"
        );
        assert_eq!(s.progress(), (1, 3));
        assert_eq!(s.current().unwrap().demo_id, "2026-08-16_23-04-42");
        assert_eq!(s.labels(), t.cfg.seed_labels);
        assert_eq!(s.last_class(), None);
        assert_eq!(
            s.current_file().as_deref(),
            Some("demos/archive/2026/08/16/2026-08-16_23-04-42_pl_pier.dem")
        );

        // Someone else (the nightly run) writes the index while the wizard is open.
        let mut other = Index::load_or_new(&t.cfg.index_path(), &[]).unwrap();
        other.by_id_mut("2026-09-21_00-00-37").unwrap().map = "pl_phoenix_v2".into();
        other.save(&t.cfg.index_path()).unwrap();

        s.save(Answer {
            label: "  free text ".into(),
            class: Some("spy".into()),
            rating: Some(7),
            streak: 0,
        })
        .unwrap();
        let ix = Index::load_or_new(&t.cfg.index_path(), &[]).unwrap();
        let pier = ix.by_id("2026-08-16_23-04-42").unwrap();
        assert_eq!(pier.events[0].label.as_deref(), Some("free text"));
        assert_eq!(pier.events[0].class.as_deref(), Some("spy"));
        assert_eq!(pier.events[0].rating, Some(5), "clamped");
        assert_eq!(pier.events[0].streak, Some(1), "blank streak → 1");
        assert!(pier.reviewed);
        assert_eq!(ix.labels.last().map(String::as_str), Some("free text"));
        assert_eq!(ix.last_class.as_deref(), Some("spy"));
        assert_eq!(
            ix.by_id("2026-09-21_00-00-37").unwrap().map,
            "pl_phoenix_v2",
            "the other writer's change survived our save"
        );
        assert_eq!(s.last_class(), Some("spy"));
        assert!(s.labels().contains(&"free text".to_string()));
        let link = t
            .root
            .join("tf/demos/archive/by-label/free text/2026-08-16_pl_pier_t24405_r5.dem");
        assert!(fs::metadata(&link).is_ok(), "label link resolves");
        assert!(
            !t.root
                .join("tf/demos/archive/by-label/unlabelled/2026-08-16_23-04-42_pl_pier.dem")
                .exists()
        );

        // Skip phoenix: its only event is skipped → reviewed, label stays null.
        assert_eq!(s.current().unwrap().demo_id, "2026-09-21_00-00-37");
        s.skip().unwrap();
        let ix = Index::load_or_new(&t.cfg.index_path(), &[]).unwrap();
        let ph = ix.by_id("2026-09-21_00-00-37").unwrap();
        assert!(ph.reviewed);
        assert_eq!(ph.events[0].label, None);

        // Label the hot badwater demo; the link points into tf/demos.
        assert_eq!(s.current().unwrap().demo_id, "2026-09-21_19-51-20");
        assert_eq!(
            s.current_file().as_deref(),
            Some("demos/2026-09-21_19-51-20.dem")
        );
        s.save(Answer {
            label: "matador".into(),
            class: None,
            rating: Some(4),
            streak: 3,
        })
        .unwrap();
        assert!(s.is_done());
        assert_eq!(s.progress(), (4, 3));
        assert!(s.current().is_none());
        assert!(
            s.save(Answer {
                label: "x".into(),
                class: None,
                rating: None,
                streak: 1,
            })
            .is_err()
        );
        let ix = Index::load_or_new(&t.cfg.index_path(), &[]).unwrap();
        let bad = ix.by_id("2026-09-21_19-51-20").unwrap();
        assert_eq!(bad.events[0].label.as_deref(), Some("matador"));
        assert_eq!(bad.events[0].streak, Some(3));
        assert_eq!(
            ix.last_class.as_deref(),
            Some("spy"),
            "no class → last_class untouched"
        );
        assert_eq!(ix.labels.len(), 4, "existing label not duplicated");
        let link = t
            .root
            .join("tf/demos/archive/by-label/matador/2026-09-21_pl_badwater_t6964_r4.dem");
        assert_eq!(
            fs::read_link(&link).unwrap(),
            PathBuf::from("../../../2026-09-21_19-51-20.dem")
        );
        assert!(fs::metadata(&link).is_ok());
        assert!(!hot_link.exists());

        // Reopening finds nothing to do; the skipped event does not return.
        let s2 = Session::open(&t.cfg).unwrap();
        assert!(s2.is_done());
        assert_eq!(s2.progress(), (1, 0));

        // The invariant: organize after review keeps every label (age 0 moves everything).
        let cfg0 = Config::parse(&format!(
            "tf_dir = {:?}\nage_hours = 0\n",
            t.root.join("tf").to_string_lossy()
        ))
        .unwrap();
        let old = SystemTime::now() - Duration::from_secs(60);
        for name in ["2026-09-21_19-51-20", "2026-09-21_00-00-37"] {
            for ext in ["dem", "json"] {
                fs::OpenOptions::new()
                    .write(true)
                    .open(t.root.join(format!("tf/demos/{name}.{ext}")))
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
        }
        let rep = archive::organize_with(&cfg0, false, false, false).unwrap();
        assert_eq!(rep.stats.moved, 3, "{:?}", rep.lines);
        let ix = Index::load_or_new(&t.cfg.index_path(), &[]).unwrap();
        let bad = ix.by_id("2026-09-21_19-51-20").unwrap();
        assert_eq!(
            bad.file,
            "demos/archive/2026/09/21/2026-09-21_19-51-20_pl_badwater.dem"
        );
        assert_eq!(bad.events[0].label.as_deref(), Some("matador"));
        assert_eq!(bad.events[0].rating, Some(4));
        assert!(bad.reviewed);
        assert!(ix.by_id("2026-09-21_00-00-37").unwrap().reviewed);
        assert_eq!(
            fs::read_link(&link).unwrap(),
            PathBuf::from("../../2026/09/21/2026-09-21_19-51-20_pl_badwater.dem")
        );
        assert!(fs::metadata(&link).is_ok());
        fs::remove_dir_all(&t.root).unwrap();
    }
}
