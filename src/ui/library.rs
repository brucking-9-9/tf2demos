//! The manager's read model: every mark of every demo (hot ones included) as flat rows, plus
//! filtering and sorting. Pure functions here are unit-tested; the GUI only draws them.

use std::path::Path;

use anyhow::Result;
use chrono::NaiveDateTime;
use eframe::egui::Color32;

use super::theme::Palette;
use crate::config::Config;
use crate::index::{DemoEntry, Index};
use crate::review;

/// One mark, denormalised with what the table shows about its demo.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub demo_id: String,
    pub tick: i64,
    pub presses: u32,
    pub map: String,
    pub recorded_at: NaiveDateTime,
    pub labels: Vec<String>,
    pub class: Option<String>,
    pub rating: Option<u8>,
    pub streak: Option<u32>,
    pub archived: bool,
    pub reviewed: bool,
    /// `tf/`-relative demo path, for play-at-tick.
    pub file: String,
}

impl EventRow {
    pub fn key(&self) -> (String, i64) {
        (self.demo_id.clone(), self.tick)
    }

    pub fn labels_text(&self) -> String {
        if self.labels.is_empty() {
            "-".into()
        } else {
            self.labels.join(", ")
        }
    }

    pub fn state_text(&self) -> String {
        format!(
            "{}{}",
            if self.archived { "archived" } else { "hot" },
            if self.reviewed { "" } else { " · to review" }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Date,
    Map,
    Tick,
    Labels,
    Class,
    Rating,
    Streak,
}

impl SortKey {
    pub const ALL: [SortKey; 7] = [
        SortKey::Date,
        SortKey::Map,
        SortKey::Tick,
        SortKey::Labels,
        SortKey::Class,
        SortKey::Rating,
        SortKey::Streak,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SortKey::Date => "date",
            SortKey::Map => "map",
            SortKey::Tick => "tick",
            SortKey::Labels => "labels",
            SortKey::Class => "class",
            SortKey::Rating => "rating",
            SortKey::Streak => "streak",
        }
    }
}

/// What the filter bar holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Space-separated terms; every term must match some field (case-insensitive).
    pub text: String,
    pub unlabelled_only: bool,
}

/// Flatten the index into rows, oldest demo first, ticks ascending.
pub fn rows(index: &Index, archive_dir: &Path) -> Vec<EventRow> {
    index
        .demos
        .iter()
        .flat_map(|d| {
            d.events.iter().map(move |e| EventRow {
                demo_id: d.id.clone(),
                tick: e.tick,
                presses: e.presses,
                map: d.map.clone(),
                recorded_at: d.recorded_at,
                labels: e.labels.clone(),
                class: e.class.clone(),
                rating: e.rating,
                streak: e.streak,
                archived: d.is_archived(archive_dir),
                reviewed: d.reviewed,
                file: d.file.clone(),
            })
        })
        .collect()
}

/// True when every whitespace-separated term of `text` occurs in some field of `row`.
pub fn matches(row: &EventRow, text: &str) -> bool {
    let hay = format!(
        "{} {} {} {} {} {} {}",
        row.demo_id,
        row.recorded_at.format("%Y-%m-%d %H:%M"),
        row.map,
        row.labels.join(" "),
        row.class.as_deref().unwrap_or(""),
        row.rating.map_or(String::new(), |r| format!("r{r}")),
        row.state_text(),
    )
    .to_lowercase();
    text.split_whitespace()
        .all(|term| hay.contains(&term.to_lowercase()))
}

pub fn filter(rows: &[EventRow], f: &Filter) -> Vec<EventRow> {
    rows.iter()
        .filter(|r| !f.unlabelled_only || r.labels.is_empty())
        .filter(|r| matches(r, &f.text))
        .cloned()
        .collect()
}

/// Stable sort by `key`; ties keep date/tick order. `Option` fields sort unset last when
/// ascending.
pub fn sort(rows: &mut [EventRow], key: SortKey, descending: bool) {
    rows.sort_by(|a, b| {
        let ord = match key {
            SortKey::Date => (a.recorded_at, a.tick).cmp(&(b.recorded_at, b.tick)),
            SortKey::Map => a.map.cmp(&b.map),
            SortKey::Tick => a.tick.cmp(&b.tick),
            SortKey::Labels => opt_last(
                (!a.labels.is_empty()).then(|| a.labels.join(",")),
                (!b.labels.is_empty()).then(|| b.labels.join(",")),
            ),
            SortKey::Class => opt_last(a.class.clone(), b.class.clone()),
            SortKey::Rating => opt_last(a.rating, b.rating),
            SortKey::Streak => opt_last(a.streak, b.streak),
        };
        let ord = ord.then_with(|| (a.recorded_at, a.tick).cmp(&(b.recorded_at, b.tick)));
        if descending { ord.reverse() } else { ord }
    });
}

fn opt_last<T: Ord>(a: Option<T>, b: Option<T>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// A stable accent colour per label (FNV-1a over the bytes, six accents).
pub fn label_color(label: &str, p: &Palette) -> Color32 {
    let mut h: u32 = 0x811c9dc5;
    for b in label.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x01000193);
    }
    [p.cyan, p.pink, p.purple, p.yellow, p.green, p.blue][(h % 6) as usize]
}

/// The index as the manager sees it: reloaded from disk (plus hot demos) after every edit.
pub struct Library {
    pub cfg: Config,
    pub index: Index,
}

impl Library {
    pub fn load(cfg: &Config) -> Result<Library> {
        let scan = review::scan(cfg)?;
        Ok(Library {
            cfg: cfg.clone(),
            index: scan.index,
        })
    }

    pub fn reload(&mut self) -> Result<()> {
        self.index = review::scan(&self.cfg)?.index;
        Ok(())
    }

    pub fn rows(&self) -> Vec<EventRow> {
        rows(&self.index, &self.cfg.archive_dir)
    }

    pub fn demo(&self, id: &str) -> Option<&DemoEntry> {
        self.index.by_id(id)
    }

    pub fn labels(&self) -> &[String] {
        &self.index.labels
    }

    pub fn classes(&self) -> &[String] {
        &self.cfg.classes
    }

    /// Marks still waiting for a label (the Review tab's badge).
    pub fn pending(&self) -> usize {
        review::queue(&self.index).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Event, State};
    use chrono::NaiveDate;

    fn at(d: u32, h: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, 0, 0)
            .unwrap()
    }

    fn ev(tick: i64, labels: &[&str], class: Option<&str>, rating: Option<u8>) -> Event {
        Event {
            tick,
            presses: 1,
            raw_ticks: vec![tick],
            labels: labels.iter().map(|s| s.to_string()).collect(),
            class: class.map(str::to_string),
            rating,
            streak: rating.map(|_| 1),
        }
    }

    fn index() -> Index {
        let mut ix = Index::new(&[]);
        ix.upsert(DemoEntry {
            id: "b".into(),
            file: "demos/b.dem".into(),
            original_name: "b".into(),
            map: "pl_upward".into(),
            server: "s".into(),
            recorded_at: at(21, 20),
            seconds: 10.0,
            ticks: 666,
            state: State::Hot,
            frozen_in: None,
            reviewed: false,
            events: vec![
                ev(300, &[], None, None),
                ev(100, &["matador", "c-tap"], Some("spy"), Some(4)),
            ],
        });
        ix.upsert(DemoEntry {
            id: "a".into(),
            file: "demos/archive/2026/09/20/a_pl_badwater.dem".into(),
            original_name: "a".into(),
            map: "pl_badwater".into(),
            server: "s".into(),
            recorded_at: at(20, 12),
            seconds: 10.0,
            ticks: 666,
            state: State::Hot,
            frozen_in: None,
            reviewed: true,
            events: vec![ev(50, &["surf stab"], Some("scout"), Some(2))],
        });
        ix
    }

    #[test]
    fn rows_flatten_in_index_order() {
        let r = rows(&index(), Path::new("demos/archive"));
        let keys: Vec<(&str, i64)> = r.iter().map(|x| (x.demo_id.as_str(), x.tick)).collect();
        assert_eq!(keys, [("a", 50), ("b", 300), ("b", 100)]);
        assert!(r[0].archived && r[0].reviewed);
        assert!(!r[1].archived && !r[1].reviewed);
        assert_eq!(r[2].labels_text(), "matador, c-tap");
        assert_eq!(r[1].labels_text(), "-");
        assert_eq!(r[1].state_text(), "hot · to review");
        assert_eq!(r[0].state_text(), "archived");
    }

    #[test]
    fn filter_terms_and_unlabelled() {
        let r = rows(&index(), Path::new("demos/archive"));
        let f = |text: &str, unl: bool| {
            filter(
                &r,
                &Filter {
                    text: text.into(),
                    unlabelled_only: unl,
                },
            )
            .iter()
            .map(|x| x.tick)
            .collect::<Vec<_>>()
        };
        assert_eq!(f("", false), [50, 300, 100]);
        assert_eq!(f("MATADOR", false), [100]);
        assert_eq!(f("upward spy", false), [100]);
        assert_eq!(f("upward review", false), [300, 100]);
        assert_eq!(f("2026-09-20", false), [50]);
        assert_eq!(f("r4", false), [100]);
        assert_eq!(f("", true), [300]);
        assert!(f("nothing here", false).is_empty());
    }

    #[test]
    fn sort_keys_and_direction() {
        let r = rows(&index(), Path::new("demos/archive"));
        let ticks = |key, desc| {
            let mut v = r.clone();
            sort(&mut v, key, desc);
            v.iter().map(|x| x.tick).collect::<Vec<_>>()
        };
        assert_eq!(ticks(SortKey::Date, false), [50, 100, 300]);
        assert_eq!(ticks(SortKey::Date, true), [300, 100, 50]);
        assert_eq!(ticks(SortKey::Map, false), [50, 100, 300]);
        assert_eq!(ticks(SortKey::Tick, true), [300, 100, 50]);
        // Unset values sort last ascending, first descending.
        assert_eq!(ticks(SortKey::Rating, false), [50, 100, 300]);
        assert_eq!(ticks(SortKey::Rating, true), [300, 100, 50]);
        assert_eq!(ticks(SortKey::Labels, false), [100, 50, 300]);
        assert_eq!(ticks(SortKey::Class, false), [50, 100, 300]);
    }

    #[test]
    fn label_colors_are_stable_accents() {
        let p = super::super::theme::Theme::default().palette();
        assert_eq!(label_color("matador", &p), label_color("matador", &p));
        let accents = [p.cyan, p.pink, p.purple, p.yellow, p.green, p.blue];
        for l in ["matador", "surf stab", "c-tap", "x", ""] {
            assert!(accents.contains(&label_color(l, &p)));
        }
    }
}
