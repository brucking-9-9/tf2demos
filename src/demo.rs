//! `.dem` header parsing, `.json` sidecar parsing, and bookmark grouping.
//!
//! Layout and formats: `HANDOFF.md` §2 "File formats". Nothing here touches the
//! packet stream; the fixed 1072-byte header is all the organizer needs.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use chrono::NaiveDateTime;
use serde::{Deserialize, Deserializer, Serialize};

/// TF2 ticks per second (`tick / 66.6667 = seconds`).
pub const TICKS_PER_SEC: f64 = 66.6667;
/// Size of the fixed `.dem` header.
pub const HEADER_LEN: usize = 1072;

const MAGIC: &[u8; 8] = b"HL2DEMO\0";
const STR_LEN: usize = 260;

const OFF_DEMO_PROTOCOL: usize = 8;
const OFF_NETWORK_PROTOCOL: usize = 12;
const OFF_SERVER: usize = 16;
const OFF_CLIENT: usize = 276;
const OFF_MAP: usize = 536;
const OFF_GAME_DIR: usize = 796;
const OFF_SECONDS: usize = 1056;
const OFF_TICKS: usize = 1060;
const OFF_FRAMES: usize = 1064;
const OFF_SIGNON_LEN: usize = 1068;

/// The fixed 1072-byte `.dem` header.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub demo_protocol: i32,
    pub network_protocol: i32,
    pub server: String,
    pub client: String,
    pub map: String,
    pub game_dir: String,
    pub seconds: f32,
    pub ticks: i32,
    pub frames: i32,
    pub signon_len: i32,
}

impl Header {
    /// Parse a header from at least [`HEADER_LEN`] bytes. Longer input (a whole
    /// demo) is fine; only the first 1072 bytes are looked at.
    pub fn parse(bytes: &[u8]) -> Result<Header> {
        ensure!(
            bytes.len() >= HEADER_LEN,
            "demo header too short: {} bytes, need {HEADER_LEN}",
            bytes.len()
        );
        let magic = &bytes[..MAGIC.len()];
        ensure!(
            magic == MAGIC,
            "bad demo magic: expected {:?}, found {:?}",
            String::from_utf8_lossy(MAGIC),
            String::from_utf8_lossy(magic)
        );
        Ok(Header {
            demo_protocol: le_i32(bytes, OFF_DEMO_PROTOCOL),
            network_protocol: le_i32(bytes, OFF_NETWORK_PROTOCOL),
            server: c_string(bytes, OFF_SERVER),
            client: c_string(bytes, OFF_CLIENT),
            map: c_string(bytes, OFF_MAP),
            game_dir: c_string(bytes, OFF_GAME_DIR),
            seconds: le_f32(bytes, OFF_SECONDS),
            ticks: le_i32(bytes, OFF_TICKS),
            frames: le_i32(bytes, OFF_FRAMES),
            signon_len: le_i32(bytes, OFF_SIGNON_LEN),
        })
    }

    /// Read and parse the header of the demo at `path`. Only the first
    /// [`HEADER_LEN`] bytes are read; demos are tens of megabytes.
    pub fn read(path: &Path) -> Result<Header> {
        let mut buf = Vec::with_capacity(HEADER_LEN);
        File::open(path)
            .and_then(|f| f.take(HEADER_LEN as u64).read_to_end(&mut buf))
            .with_context(|| format!("reading demo header of {}", path.display()))?;
        Header::parse(&buf).with_context(|| format!("parsing demo header of {}", path.display()))
    }

    /// `ticks == 0`: still recording, or never finalized (crash).
    /// Reported by the GUI (later session); `organize` decides by age and sidecar only.
    #[allow(dead_code)]
    pub fn is_in_progress(&self) -> bool {
        self.ticks == 0
    }
}

fn le_i32(bytes: &[u8], off: usize) -> i32 {
    let raw: [u8; 4] = bytes[off..off + 4]
        .try_into()
        .expect("caller checked header length");
    i32::from_le_bytes(raw)
}

fn le_f32(bytes: &[u8], off: usize) -> f32 {
    let raw: [u8; 4] = bytes[off..off + 4]
        .try_into()
        .expect("caller checked header length");
    f32::from_le_bytes(raw)
}

/// A NUL-padded `char[260]` field: bytes up to the first NUL, decoded lossily.
fn c_string(bytes: &[u8], off: usize) -> String {
    let field = &bytes[off..off + STR_LEN];
    let end = field.iter().position(|&b| b == 0).unwrap_or(STR_LEN);
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// One entry of the `.json` sidecar written by ds when recording stops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub name: String,
    #[serde(deserialize_with = "string_or_number")]
    pub value: String,
    pub tick: i64,
}

/// Accept `"value": "General"` as well as `"value": 3`, both as a `String`.
fn string_or_number<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Str(String),
        Num(serde_json::Number),
    }
    Ok(match Raw::deserialize(d)? {
        Raw::Str(s) => s,
        Raw::Num(n) => n.to_string(),
    })
}

/// The `.json` sidecar: `{"events": [...]}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Sidecar {
    #[serde(default)]
    pub events: Vec<Mark>,
}

impl Sidecar {
    /// Parse sidecar JSON. Tabs and a blank line inside the object are normal.
    pub fn parse(text: &str) -> Result<Sidecar> {
        serde_json::from_str(text).context("parsing demo sidecar JSON")
    }

    /// Read and parse the sidecar at `path`.
    pub fn read(path: &Path) -> Result<Sidecar> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading demo sidecar {}", path.display()))?;
        Sidecar::parse(&text).with_context(|| format!("parsing demo sidecar {}", path.display()))
    }
}

/// Marks pressed in quick succession, merged into one event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupedEvent {
    /// Tick of the first (lowest) mark in the group.
    pub tick: i64,
    /// Number of marks in the group.
    pub presses: u32,
    /// Every mark tick in the group, ascending.
    pub raw_ticks: Vec<i64>,
}

/// Sort marks by tick and merge runs: a mark within `group_secs` (converted with
/// [`TICKS_PER_SEC`]) of the *previous* mark joins the current group.
pub fn group_marks(marks: &[Mark], group_secs: f64) -> Vec<GroupedEvent> {
    let mut ticks: Vec<i64> = marks.iter().map(|m| m.tick).collect();
    ticks.sort_unstable();
    let window = group_secs * TICKS_PER_SEC;

    let mut groups: Vec<GroupedEvent> = Vec::new();
    for tick in ticks {
        match groups.last_mut() {
            Some(g) if (tick - g.raw_ticks[g.raw_ticks.len() - 1]) as f64 <= window => {
                g.raw_ticks.push(tick);
                g.presses += 1;
            }
            _ => groups.push(GroupedEvent {
                tick,
                presses: 1,
                raw_ticks: vec![tick],
            }),
        }
    }
    groups
}

/// Parse a ds-style stem `YYYY-MM-DD_HH-MM-SS` (no extension). Anything else,
/// including hand-renamed demos such as `Tight_scout_m`, yields `None`.
pub fn parse_ds_name(stem: &str) -> Option<NaiveDateTime> {
    if stem.len() != 19 {
        return None;
    }
    NaiveDateTime::parse_from_str(stem, "%Y-%m-%d_%H-%M-%S").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    const PIER_HDR: &[u8] = include_bytes!("../tests/fixtures/2026-08-16_23-04-42.hdr");
    const PIER_JSON: &str = include_str!("../tests/fixtures/2026-08-16_23-04-42.json");
    const BORNEO_HDR: &[u8] = include_bytes!("../tests/fixtures/2026-09-09_22-09-51.hdr");
    const BORNEO_JSON: &str = include_str!("../tests/fixtures/2026-09-09_22-09-51.json");
    const PHOENIX_HDR: &[u8] = include_bytes!("../tests/fixtures/2026-09-21_00-00-37.hdr");
    const PHOENIX_JSON: &str = include_str!("../tests/fixtures/2026-09-21_00-00-37.json");
    const BADWATER_HDR: &[u8] = include_bytes!("../tests/fixtures/2026-09-21_19-51-20.hdr");
    const BADWATER_JSON: &str = include_str!("../tests/fixtures/2026-09-21_19-51-20.json");
    const TIGHT_HDR: &[u8] = include_bytes!("../tests/fixtures/Tight_scout_m.hdr");
    const TIGHT_JSON: &str = include_str!("../tests/fixtures/Tight_scout_m.json");
    const THUNDER_HDR: &[u8] = include_bytes!("../tests/fixtures/2026-09-21_20-42-43.hdr");

    fn mark(tick: i64) -> Mark {
        Mark {
            name: "Bookmark".into(),
            value: "General".into(),
            tick,
        }
    }

    /// Assert every field of a header against the verified table.
    fn check_header(
        bytes: &[u8],
        server: &str,
        map: &str,
        seconds: f32,
        ticks: i32,
        frames: i32,
        signon_len: i32,
    ) -> Header {
        let h = Header::parse(bytes).expect("fixture header parses");
        assert_eq!(h.demo_protocol, 3);
        assert_eq!(h.network_protocol, 24);
        assert_eq!(h.client, "brucking");
        assert_eq!(h.game_dir, "tf");
        assert_eq!(h.server, server);
        assert_eq!(h.map, map);
        assert!(
            (h.seconds - seconds).abs() < 0.01,
            "seconds: got {}, want {seconds}",
            h.seconds
        );
        assert_eq!(h.ticks, ticks);
        assert_eq!(h.frames, frames);
        assert_eq!(h.signon_len, signon_len);
        h
    }

    fn sidecar_ticks(text: &str) -> Vec<i64> {
        let sc = Sidecar::parse(text).expect("fixture sidecar parses");
        for m in &sc.events {
            assert_eq!(m.name, "Bookmark");
            assert_eq!(m.value, "General");
        }
        sc.events.iter().map(|m| m.tick).collect()
    }

    #[test]
    fn header_fixtures_are_exactly_1072_bytes() {
        for hdr in [
            PIER_HDR,
            BORNEO_HDR,
            PHOENIX_HDR,
            BADWATER_HDR,
            TIGHT_HDR,
            THUNDER_HDR,
        ] {
            assert_eq!(hdr.len(), HEADER_LEN);
        }
    }

    #[test]
    fn pier() {
        let h = check_header(
            PIER_HDR,
            "169.254.68.81:38104",
            "pl_pier",
            488.43,
            32562,
            31772,
            337200,
        );
        assert!(!h.is_in_progress());
        assert_eq!(sidecar_ticks(PIER_JSON), [24405]);
        let sc = Sidecar::parse(PIER_JSON).unwrap();
        let g = group_marks(&sc.events, 2.0);
        assert_eq!(
            g,
            [GroupedEvent {
                tick: 24405,
                presses: 1,
                raw_ticks: vec![24405],
            }]
        );
    }

    #[test]
    fn borneo() {
        let h = check_header(
            BORNEO_HDR,
            "169.254.176.234:55824",
            "pl_borneo",
            1616.27,
            107751,
            106071,
            332086,
        );
        assert!(!h.is_in_progress());
        assert_eq!(sidecar_ticks(BORNEO_JSON), [48085, 48131, 48156, 48181]);
        let sc = Sidecar::parse(BORNEO_JSON).unwrap();
        let g = group_marks(&sc.events, 2.0);
        assert_eq!(
            g,
            [GroupedEvent {
                tick: 48085,
                presses: 4,
                raw_ticks: vec![48085, 48131, 48156, 48181],
            }]
        );
    }

    #[test]
    fn phoenix() {
        let h = check_header(
            PHOENIX_HDR,
            "169.254.28.0:42360",
            "pl_phoenix",
            358.18,
            23879,
            23410,
            358222,
        );
        assert!(!h.is_in_progress());
        assert_eq!(sidecar_ticks(PHOENIX_JSON), [18564, 18578, 18590]);
        let sc = Sidecar::parse(PHOENIX_JSON).unwrap();
        let g = group_marks(&sc.events, 2.0);
        assert_eq!(
            g,
            [GroupedEvent {
                tick: 18564,
                presses: 3,
                raw_ticks: vec![18564, 18578, 18590],
            }]
        );
    }

    #[test]
    fn badwater() {
        let h = check_header(
            BADWATER_HDR,
            "169.254.240.159:13144",
            "pl_badwater",
            104.67,
            6978,
            6858,
            348686,
        );
        assert!(!h.is_in_progress());
        assert_eq!(sidecar_ticks(BADWATER_JSON), [6964]);
        let sc = Sidecar::parse(BADWATER_JSON).unwrap();
        let g = group_marks(&sc.events, 2.0);
        assert_eq!(
            g,
            [GroupedEvent {
                tick: 6964,
                presses: 1,
                raw_ticks: vec![6964],
            }]
        );
    }

    #[test]
    fn tight_scout_m() {
        let h = check_header(
            TIGHT_HDR,
            "169.254.240.159:13144",
            "pl_badwater",
            36.79,
            2453,
            2404,
            348686,
        );
        assert!(!h.is_in_progress());
        assert_eq!(sidecar_ticks(TIGHT_JSON), [2291]);
        let sc = Sidecar::parse(TIGHT_JSON).unwrap();
        let g = group_marks(&sc.events, 2.0);
        assert_eq!(
            g,
            [GroupedEvent {
                tick: 2291,
                presses: 1,
                raw_ticks: vec![2291],
            }]
        );
    }

    #[test]
    fn thundermountain_in_progress() {
        let h = check_header(
            THUNDER_HDR,
            "169.254.240.159:13144",
            "pl_thundermountain",
            0.0,
            0,
            0,
            0,
        );
        assert!(h.is_in_progress());
    }

    #[test]
    fn parse_accepts_longer_input() {
        let mut bytes = PIER_HDR.to_vec();
        bytes.extend_from_slice(&[0xAB; 500]);
        let h = Header::parse(&bytes).unwrap();
        assert_eq!(h.map, "pl_pier");
        assert_eq!(h.ticks, 32562);
    }

    #[test]
    fn parse_rejects_short_input() {
        let err = Header::parse(&PIER_HDR[..100]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("too short"), "{msg}");
        assert!(msg.contains("100"), "{msg}");
        assert!(Header::parse(&[]).is_err());
        assert!(Header::parse(&PIER_HDR[..HEADER_LEN - 1]).is_err());
    }

    #[test]
    fn parse_rejects_bad_magic() {
        let mut bytes = PIER_HDR.to_vec();
        bytes[..8].copy_from_slice(b"HL2DEMOX");
        let err = Header::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("magic"), "{err}");
        let zeros = vec![0u8; HEADER_LEN];
        assert!(Header::parse(&zeros).is_err());
    }

    #[test]
    fn read_only_takes_the_header() {
        let dir = std::env::temp_dir().join(format!("tf2demos-demo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long.dem");
        let mut bytes = BORNEO_HDR.to_vec();
        bytes.extend_from_slice(&[0x5A; 4096]);
        std::fs::write(&path, &bytes).unwrap();
        let h = Header::read(&path).unwrap();
        assert_eq!(h.map, "pl_borneo");
        assert_eq!(h.ticks, 107751);

        let short = dir.join("short.dem");
        std::fs::write(&short, &BORNEO_HDR[..50]).unwrap();
        let err = Header::read(&short).unwrap_err();
        assert!(format!("{err:#}").contains("short.dem"), "{err:#}");

        let missing = dir.join("missing.dem");
        assert!(Header::read(&missing).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sidecar_unknown_names_pass_through() {
        let text = r#"{"events":[{"name":"Killstreak","value":"5","tick":10},{"name":"Bookmark","value":"General","tick":20}]}"#;
        let sc = Sidecar::parse(text).unwrap();
        assert_eq!(sc.events.len(), 2);
        assert_eq!(sc.events[0].name, "Killstreak");
        assert_eq!(sc.events[0].value, "5");
        assert_eq!(sc.events[0].tick, 10);
        assert_eq!(sc.events[1], mark(20));
    }

    #[test]
    fn sidecar_numeric_value_becomes_string() {
        let text = r#"{"events":[{"name":"Killstreak","value":7,"tick":10}]}"#;
        let sc = Sidecar::parse(text).unwrap();
        assert_eq!(sc.events[0].value, "7");
    }

    #[test]
    fn sidecar_empty_and_missing_events() {
        assert_eq!(
            Sidecar::parse(r#"{"events":[]}"#).unwrap(),
            Sidecar::default()
        );
        assert_eq!(Sidecar::parse("{}").unwrap(), Sidecar::default());
        assert!(Sidecar::parse("not json").is_err());
        assert!(Sidecar::parse(r#"{"events":[{"name":"Bookmark"}]}"#).is_err());
    }

    #[test]
    fn sidecar_read_from_disk() {
        let dir =
            std::env::temp_dir().join(format!("tf2demos-sidecar-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.json");
        std::fs::write(&path, TIGHT_JSON).unwrap();
        let sc = Sidecar::read(&path).unwrap();
        assert_eq!(sc.events, [mark(2291)]);
        assert!(Sidecar::read(&dir.join("nope.json")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sidecar_roundtrips_through_serde() {
        let sc = Sidecar {
            events: vec![mark(1), mark(2)],
        };
        let json = serde_json::to_string(&sc).unwrap();
        assert_eq!(Sidecar::parse(&json).unwrap(), sc);
    }

    #[test]
    fn group_marks_splits_at_window() {
        // 233 - 100 = 133 ticks <= 133.33 (2.0 s): joins. 1000 - 234 does not.
        let marks = [mark(100), mark(233), mark(234), mark(1000)];
        let g = group_marks(&marks, 2.0);
        assert_eq!(
            g,
            [
                GroupedEvent {
                    tick: 100,
                    presses: 3,
                    raw_ticks: vec![100, 233, 234],
                },
                GroupedEvent {
                    tick: 1000,
                    presses: 1,
                    raw_ticks: vec![1000],
                },
            ]
        );
    }

    #[test]
    fn group_marks_window_is_relative_to_previous_mark() {
        // Each gap is 100 ticks (< 133.33) but the span is 300: one chain.
        let marks = [mark(0), mark(100), mark(200), mark(300)];
        let g = group_marks(&marks, 2.0);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].presses, 4);
        assert_eq!(g[0].raw_ticks, [0, 100, 200, 300]);
        // 134 ticks is just over 2.0 s: new group.
        let marks = [mark(0), mark(134)];
        assert_eq!(group_marks(&marks, 2.0).len(), 2);
    }

    #[test]
    fn group_marks_sorts_unsorted_input() {
        let marks = [mark(1000), mark(234), mark(100), mark(233)];
        let g = group_marks(&marks, 2.0);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].tick, 100);
        assert_eq!(g[0].raw_ticks, [100, 233, 234]);
        assert_eq!(g[1].tick, 1000);
    }

    #[test]
    fn group_marks_empty() {
        assert!(group_marks(&[], 2.0).is_empty());
    }

    #[test]
    fn group_marks_zero_window_keeps_duplicates_together() {
        let marks = [mark(5), mark(5), mark(6)];
        let g = group_marks(&marks, 0.0);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].raw_ticks, [5, 5]);
        assert_eq!(g[1].raw_ticks, [6]);
    }

    #[test]
    fn parse_ds_name_positive() {
        let want = NaiveDate::from_ymd_opt(2026, 9, 21)
            .unwrap()
            .and_hms_opt(19, 54, 0)
            .unwrap();
        assert_eq!(parse_ds_name("2026-09-21_19-54-00"), Some(want));
        let want = NaiveDate::from_ymd_opt(2026, 8, 16)
            .unwrap()
            .and_hms_opt(23, 4, 42)
            .unwrap();
        assert_eq!(parse_ds_name("2026-08-16_23-04-42"), Some(want));
    }

    #[test]
    fn parse_ds_name_negative() {
        assert_eq!(parse_ds_name("Tight_scout_m"), None);
        assert_eq!(parse_ds_name(""), None);
        assert_eq!(parse_ds_name("2026-09-21_19-54-00.dem"), None);
        assert_eq!(parse_ds_name("2026-09-21"), None);
        assert_eq!(parse_ds_name("2026-09-21 19-54-00"), None);
        assert_eq!(parse_ds_name("2026-13-21_19-54-00"), None);
        assert_eq!(parse_ds_name("2026-9-21_19-54-00"), None);
    }
}
