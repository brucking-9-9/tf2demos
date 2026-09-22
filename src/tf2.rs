//! TF2 process detection and play-at-tick.
//!
//! `organize` cares whether TF2 is running for the `_events.txt` rewrite (ds appends to that file
//! mid-game); the watcher polls it; `play` picks its branch by it. Detection scans `/proc`
//! directly: `pgrep -f` matched its own wrapper shell on this box, so we never shell out.
//!
//! Test override: set `TF2DEMOS_TF2_RUNNING=1`/`true` to force "running" or `0`/`false` to force
//! "not running", e.g. when exercising `organize` on a scratch copy while the real game is up.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// Process names of the TF2 game binary (64-bit and the legacy 32-bit launcher).
const TF2_PROCESS_NAMES: &[&str] = &["tf_linux64", "hl2_linux"];

/// Length the kernel truncates `/proc/<pid>/comm` to.
const COMM_MAX: usize = 15;

/// Name of the environment variable that overrides detection.
pub const OVERRIDE_ENV: &str = "TF2DEMOS_TF2_RUNNING";

/// True when TF2 is running, unless overridden by [`OVERRIDE_ENV`].
pub fn is_running() -> bool {
    let env = std::env::var(OVERRIDE_ENV).ok();
    is_running_with(env.as_deref())
}

/// [`is_running`] with the override value passed in: `1`/`true` → running, `0`/`false` → not
/// running (case-insensitive, whitespace trimmed). Anything else falls through to the scan.
pub fn is_running_with(override_env: Option<&str>) -> bool {
    match parse_override(override_env) {
        Some(forced) => forced,
        None => scan_proc(Path::new("/proc")),
    }
}

fn parse_override(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// True if either the kernel's `comm` (truncated to 15 bytes) or argv[0]'s basename names a
/// TF2 binary. Only argv[0] is considered, so a `pgrep -f tf_linux64` or an editor with the
/// name in its arguments never matches.
pub fn proc_matches(comm: &str, cmdline_argv0: &str) -> bool {
    let argv0 = Path::new(cmdline_argv0)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    TF2_PROCESS_NAMES.iter().any(|name| {
        let truncated = &name[..name.len().min(COMM_MAX)];
        argv0 == *name || comm == truncated
    })
}

/// Scan every numeric entry of `proc_root` except our own pid. Unreadable entries (other
/// users' processes, races with exiting processes) are skipped.
fn scan_proc(proc_root: &Path) -> bool {
    let me = std::process::id().to_string();
    let Ok(entries) = fs::read_dir(proc_root) else {
        return false;
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name != me && !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
        })
        .any(|e| {
            let dir = e.path();
            let comm = fs::read_to_string(dir.join("comm")).unwrap_or_default();
            let cmdline = fs::read(dir.join("cmdline")).unwrap_or_default();
            let argv0 = cmdline
                .split(|&b| b == 0)
                .next()
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .unwrap_or_default();
            proc_matches(comm.trim(), &argv0)
        })
}

// ---------------------------------------------------------------------------------------------
// Play at tick (HANDOFF §4 "Playback helper")

/// Steam app id of Team Fortress 2.
pub const STEAM_APPID: &str = "440";

/// What `play` will do, decided from the TF2 state so it can be unit-tested without a desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayPlan {
    /// TF2 is running: the console command goes to the clipboard for the user to paste.
    Clipboard { command: String },
    /// TF2 is closed: launch through Steam with `+playdemo` / `+demo_gototick`; the gototick
    /// line also goes to the clipboard in case the queued `+demo_gototick` is ignored at launch.
    Launch {
        steam_args: Vec<String>,
        clipboard: String,
    },
}

/// The in-game console form: `playdemo demos/x.dem; demo_gototick 6964`.
pub fn console_command(rel: &str, tick: i64) -> String {
    format!("playdemo {rel}; demo_gototick {tick}")
}

/// `rel` is the demo path relative to `tf/` (`demos/archive/2026/09/21/x.dem` or `demos/x.dem`).
pub fn play_plan(rel: &str, tick: i64, running: bool) -> PlayPlan {
    if running {
        PlayPlan::Clipboard {
            command: console_command(rel, tick),
        }
    } else {
        PlayPlan::Launch {
            steam_args: vec![
                "-applaunch".into(),
                STEAM_APPID.into(),
                "-novid".into(),
                "+playdemo".into(),
                rel.into(),
                "+demo_gototick".into(),
                tick.to_string(),
            ],
            clipboard: format!("demo_gototick {tick}"),
        }
    }
}

/// Human-readable outcome of a plan, for the wizard's status line and `play`'s stdout.
pub fn describe(plan: &PlayPlan) -> String {
    match plan {
        PlayPlan::Clipboard { command } => {
            format!("TF2 is running: copied to clipboard, paste in console: {command}")
        }
        PlayPlan::Launch { steam_args, .. } => {
            format!("launching TF2: steam {}", steam_args.join(" "))
        }
    }
}

/// Play `rel` at `tick`: clipboard when TF2 runs, Steam launch otherwise. Returns [`describe`].
pub fn play(rel: &str, tick: i64) -> Result<String> {
    let plan = play_plan(rel, tick, is_running());
    execute(&plan)?;
    Ok(describe(&plan))
}

fn execute(plan: &PlayPlan) -> Result<()> {
    match plan {
        PlayPlan::Clipboard { command } => {
            wl_copy(command)?;
            notify("tf2demos", "Copied to clipboard — paste in the TF2 console");
        }
        PlayPlan::Launch {
            steam_args,
            clipboard,
        } => {
            // Best effort: the launch is the point, the clipboard only the fallback.
            if let Err(err) = wl_copy(clipboard) {
                eprintln!("tf2demos: {err:#}");
            }
            // Detached (own process group, no pipes) so closing the wizard never takes TF2 down.
            Command::new("steam")
                .args(steam_args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .context("launching `steam` (is it on PATH?)")?;
        }
    }
    Ok(())
}

fn wl_copy(text: &str) -> Result<()> {
    let status = Command::new("wl-copy")
        .arg("--")
        .arg(text)
        .stdin(Stdio::null())
        .status()
        .context("running `wl-copy` (wl-clipboard on PATH, WAYLAND_DISPLAY set?)")?;
    anyhow::ensure!(status.success(), "wl-copy exited with {status}");
    Ok(())
}

/// Fire-and-forget desktop notification; failures are logged, never fatal.
pub fn notify(summary: &str, body: &str) {
    let r = Command::new("notify-send")
        .args(["-a", "tf2demos", "--", summary, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status();
    if let Err(err) = r {
        eprintln!("tf2demos: notify-send failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_plan_running_goes_to_clipboard() {
        let p = play_plan("demos/archive/2026/09/21/x_pl_badwater.dem", 6964, true);
        assert_eq!(
            p,
            PlayPlan::Clipboard {
                command: "playdemo demos/archive/2026/09/21/x_pl_badwater.dem; demo_gototick 6964"
                    .into()
            }
        );
        assert!(describe(&p).contains("paste in console"));
    }

    #[test]
    fn play_plan_closed_launches_steam_with_fallback() {
        let p = play_plan("demos/Tight_scout_m.dem", 2291, false);
        assert_eq!(
            p,
            PlayPlan::Launch {
                steam_args: vec![
                    "-applaunch".into(),
                    "440".into(),
                    "-novid".into(),
                    "+playdemo".into(),
                    "demos/Tight_scout_m.dem".into(),
                    "+demo_gototick".into(),
                    "2291".into(),
                ],
                clipboard: "demo_gototick 2291".into(),
            }
        );
        assert_eq!(
            describe(&p),
            "launching TF2: steam -applaunch 440 -novid +playdemo demos/Tight_scout_m.dem +demo_gototick 2291"
        );
    }

    #[test]
    fn override_forces_true() {
        assert!(is_running_with(Some("1")));
        assert!(is_running_with(Some("true")));
        assert!(is_running_with(Some(" TRUE ")));
        assert_eq!(parse_override(Some("1")), Some(true));
    }

    #[test]
    fn override_forces_false() {
        assert!(!is_running_with(Some("0")));
        assert!(!is_running_with(Some("false")));
        assert!(!is_running_with(Some("False\n")));
        assert_eq!(parse_override(Some("0")), Some(false));
    }

    #[test]
    fn unrecognised_override_falls_through_to_scan() {
        assert_eq!(parse_override(None), None);
        assert_eq!(parse_override(Some("")), None);
        assert_eq!(parse_override(Some("maybe")), None);
        // Scanning must never panic, whatever this machine is running.
        let _ = is_running_with(Some("maybe"));
        let _ = is_running_with(None);
    }

    #[test]
    fn proc_matches_tf2_binaries() {
        assert!(proc_matches(
            "tf_linux64",
            "/home/brucking/.local/share/Steam/steamapps/common/Team Fortress 2/tf_linux64"
        ));
        assert!(proc_matches("tf_linux64", ""));
        assert!(proc_matches("hl2_linux", "hl2_linux"));
        assert!(proc_matches("hl2_linux", "./hl2_linux"));
        // comm truncated by the kernel would still match argv[0].
        assert!(proc_matches("", "/some/where/tf_linux64"));
    }

    #[test]
    fn proc_rejects_other_processes() {
        assert!(!proc_matches("zsh", "/run/current-system/sw/bin/zsh"));
        assert!(!proc_matches("pgrep", "pgrep"));
        assert!(!proc_matches("tf_linux64x", "tf_linux64x"));
        assert!(!proc_matches(
            "steam",
            "/home/brucking/.steam/steam/ubuntu12_32/steam"
        ));
        // The name only appears deeper in the path or later in argv: not a match.
        assert!(!proc_matches("hx", "/nix/store/x/tf_linux64/bin/hx"));
    }

    #[test]
    fn scan_skips_own_pid_and_tolerates_garbage() {
        // A directory with no numeric entries is simply "not running".
        assert!(!scan_proc(Path::new("/nonexistent-proc-root")));
        assert!(!scan_proc(&std::env::temp_dir()));
    }
}
