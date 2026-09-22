//! `tf2demos watch` — the TF2-exit watcher (systemd user service).
//!
//! Polls [`tf2::is_running`] every `poll_secs`. On a running→stopped transition it waits for ds
//! to flush the `.json` sidecars, builds the review queue ([`review::scan`] + [`review::queue`],
//! in memory only) and, if anything is unlabelled, shows a persistent mako notification with a
//! *Review* action. Clicking it spawns `tf2demos review`.
//!
//! The watcher never moves or deletes files, never writes `_events.txt` or `index.json`, and
//! never calls `organize` (the daily timer does that). The notification blocks in its own thread
//! so the poll loop keeps running while the prompt sits on screen.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::review;
use crate::tf2;

/// Seconds to wait after TF2 exits before reading sidecars (ds writes them on stop).
const FLUSH_SECS: u64 = 5;

/// Edge detector over the running flag. The first sample only sets the baseline.
#[derive(Debug, Default)]
pub struct Watcher {
    was_running: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Unchanged,
    Started,
    Stopped,
}

impl Watcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, running: bool) -> Transition {
        let t = match self.was_running {
            None => Transition::Unchanged,
            Some(prev) if prev == running => Transition::Unchanged,
            Some(_) if running => Transition::Started,
            Some(_) => Transition::Stopped,
        };
        self.was_running = Some(running);
        t
    }
}

/// `"N demos, M marks to review"`.
pub fn summary_text(demos: usize, marks: usize) -> String {
    format!(
        "{demos} demo{}, {marks} mark{} to review",
        if demos == 1 { "" } else { "s" },
        if marks == 1 { "" } else { "s" }
    )
}

/// Arguments for `notify-send`: app name, never expire (mako's `default-timeout` is 5 s),
/// one action keyed `default` so mako's left click (`invoke-default-action`) fires it.
pub fn notify_args(summary: &str) -> Vec<String> {
    [
        "-a",
        "tf2demos",
        "-t",
        "0",
        "-A",
        "default=Review",
        "--",
        "TF2 closed",
        summary,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `notify-send -A` prints the chosen action's key on stdout.
pub fn is_review_action(stdout: &str) -> bool {
    stdout.trim() == "default"
}

/// The blocking loop. `config_path` is forwarded to the spawned `review` so a scratch config
/// stays in effect.
pub fn run(cfg: &Config, config_path: Option<&Path>) -> Result<()> {
    let poll = Duration::from_secs(cfg.poll_secs.max(1));
    let mut w = Watcher::new();
    let config_path = config_path.map(Path::to_path_buf);
    println!(
        "tf2demos watch: polling every {}s, TF2 {}",
        poll.as_secs(),
        if tf2::is_running() {
            "running"
        } else {
            "not running"
        }
    );
    loop {
        if w.observe(tf2::is_running()) == Transition::Stopped {
            println!("tf2demos watch: TF2 exited, waiting {FLUSH_SECS}s for sidecars");
            thread::sleep(Duration::from_secs(FLUSH_SECS));
            match pending(cfg) {
                Ok((0, _)) => println!("tf2demos watch: nothing to review"),
                Ok((demos, marks)) => {
                    let summary = summary_text(demos, marks);
                    println!("tf2demos watch: {summary}");
                    let config_path = config_path.clone();
                    thread::spawn(move || prompt(&summary, config_path.as_deref()));
                }
                Err(err) => eprintln!("tf2demos watch: {err:#}"),
            }
        }
        thread::sleep(poll);
    }
}

/// `(demos, marks)` waiting for a label, computed without writing anything.
fn pending(cfg: &Config) -> Result<(usize, usize)> {
    let scan = review::scan(cfg)?;
    Ok(review::counts(&review::queue(&scan.index)))
}

/// Show the notification (blocks until clicked or dismissed) and spawn the wizard on click.
fn prompt(summary: &str, config_path: Option<&Path>) {
    let out = Command::new("notify-send")
        .args(notify_args(summary))
        .stdin(Stdio::null())
        .output();
    let out = match out {
        Ok(o) => o,
        Err(err) => {
            eprintln!("tf2demos watch: notify-send failed: {err}");
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !is_review_action(&stdout) {
        println!("tf2demos watch: notification dismissed");
        return;
    }
    println!("tf2demos watch: opening review");
    if let Err(err) = spawn_review(config_path) {
        eprintln!("tf2demos watch: {err:#}");
    }
}

fn spawn_review(config_path: Option<&Path>) -> Result<()> {
    let exe: PathBuf = std::env::current_exe().context("locating tf2demos binary")?;
    let mut cmd = Command::new(exe);
    if let Some(p) = config_path {
        cmd.arg("--config").arg(p);
    }
    cmd.arg("review")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = cmd.status().context("running tf2demos review")?;
    if !status.success() {
        eprintln!("tf2demos watch: review exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the detector with a scripted sequence of samples, as the loop would.
    fn transitions(samples: &[bool]) -> Vec<Transition> {
        let mut w = Watcher::new();
        samples.iter().map(|&r| w.observe(r)).collect()
    }

    #[test]
    fn running_then_stopped_fires_once() {
        use Transition::*;
        assert_eq!(
            transitions(&[true, true, false, false, false]),
            [Unchanged, Unchanged, Stopped, Unchanged, Unchanged]
        );
    }

    #[test]
    fn stopped_to_stopped_never_fires() {
        assert!(
            transitions(&[false, false, false])
                .iter()
                .all(|t| *t == Transition::Unchanged)
        );
    }

    #[test]
    fn baseline_running_at_start_still_detects_exit() {
        use Transition::*;
        // TF2 already up when the service starts; a relaunch mid-way fires Started then Stopped.
        assert_eq!(
            transitions(&[true, false, true, false]),
            [Unchanged, Stopped, Started, Stopped]
        );
    }

    #[test]
    fn first_sample_is_only_a_baseline() {
        assert_eq!(transitions(&[false]), [Transition::Unchanged]);
        assert_eq!(transitions(&[true]), [Transition::Unchanged]);
    }

    #[test]
    fn summary_pluralizes() {
        assert_eq!(summary_text(1, 1), "1 demo, 1 mark to review");
        assert_eq!(summary_text(2, 1), "2 demos, 1 mark to review");
        assert_eq!(summary_text(3, 5), "3 demos, 5 marks to review");
    }

    #[test]
    fn notify_args_shape() {
        let a = notify_args("2 demos, 3 marks to review");
        assert_eq!(
            a,
            [
                "-a",
                "tf2demos",
                "-t",
                "0",
                "-A",
                "default=Review",
                "--",
                "TF2 closed",
                "2 demos, 3 marks to review"
            ]
        );
    }

    #[test]
    fn action_detection() {
        assert!(is_review_action("default\n"));
        assert!(is_review_action("default"));
        assert!(!is_review_action(""));
        assert!(!is_review_action("closed\n"));
        assert!(!is_review_action("2\n"));
    }
}
