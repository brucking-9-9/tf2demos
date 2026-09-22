# tf2demos — implementer's operating manual (session 2)

Read this whole file before doing anything. `HANDOFF.md` is the original spec
(data formats, decisions, architecture, the user's verbatim Q&A). This file is the
**operating manual**: what exists, what session 2 builds, which rules to obey,
where HANDOFF is out of date, and where to stop and ask.

**Where this file and HANDOFF.md disagree, this file wins.** Do not edit HANDOFF.md.

Rewritten 2026-09-21 at the end of session 1. **[user]** = decided by brucking;
**[verified]** = checked on this machine; **[s1]** = decided by the session-1
implementer under §5 autonomy and visible to the user in the final report.

---

## 1. Read this first

**What it is.** A Rust tool that manages Team Fortress 2 demos recorded by TF2's
built-in `ds_*` demo support. Session 1 shipped the daily organizing job and it
is **live on this machine**: `tf2demos organize` runs from a systemd user timer
installed by this repo's home-manager module. Session 2 builds the
"prompted after each session" loop **[user, 2026-09-21]**: a TF2-exit watcher,
a mako notification, an egui **review wizard** that labels marks, and
**play-at-tick**. The Events/Demos tabs, the timeline widget, and the
freeze/thaw cold store are session 3+ (§8).

**Environment** [verified 2026-09-21]:
- NixOS unstable, flake at `/etc/nixos`, host `19BK`, Wayland (niri + waybar +
  kitty + mako). The user's global rules in `~/.claude/CLAUDE.md` apply
  (nix-native only, `/etc/nixos` top level is root-owned, `switch` alias rebuilds).
- **No `cargo`/`rustc` on PATH.** Everything runs through this repo's devShell:
  `nix develop -c cargo <...>`. The shell is cached; nixpkgs is pinned in
  `flake.lock` to the running system's rev (`c043004d…`, Rust 1.97.1). If the
  system has moved since (a weekly flake-update timer exists), re-pin with
  `nix flake lock --override-input nixpkgs github:nixos/nixpkgs/$(nixos-version --json | jq -r .nixpkgsRevision)`
  so `nix develop` reuses the cached toolchain.
- **TF2 may be running at any moment.** Do not launch it, kill it, or assume it
  is closed. **Do not trust `pgrep -f 'tf_linux64|hl2_linux'`**: it matched its
  own wrapper shell in session 1 and reported TF2 running when it was not. Use
  `ps -eo args | grep -E '^\S*(tf_linux64|hl2_linux)' | grep -v grep`, or the
  tool's own `tf2::is_running()` (reads `/proc/*/comm`).
- Real demo directory: `/home/brucking/.local/share/Steam/steamapps/common/Team Fortress 2/tf/demos/`
  (below: `tf/demos`). **The user cares about every file in it.** Its layout now:
  ```
  tf/demos/<hot demos>.dem + .json     # under 24 h old, ds names or hand-renamed
  tf/demos/_events.txt                 # ds master log, lines for hot demos only
  tf/demos/archive/YYYY/MM/DD/<stem>_<map>.dem + .json + events.txt
  tf/demos/archive/by-label/<label>/<link>.dem   # regenerated every run, relative symlinks
  tf/demos/archive/events-orphans.txt
  tf/demos/archive/index.json          # the only mutable state
  ```
  Read-only commands against it are fine (`ls`, `cat index.json`,
  `tf2demos organize --dry-run`). **Every development run uses a scratch copy** (§6).
- Git: `main`, remote `origin = git@github.com:brucking-9-9/tf2demos.git`
  (public; SSH only, no `gh`, no HTTPS tokens). Identity is configured globally.
  Trailer on every commit: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

---

## 2. What exists (end of session 1) [verified]

```
tf2demos/
├── flake.nix            # devShell, packages.default (buildRustPackage, fileset src),
│                        # checks.{tests,clippy}, homeManagerModules.default
├── nix/hm-module.nix    # { self }: { config, lib, pkgs, ... }: services.tf2demos.*
├── Cargo.toml           # edition 2024; deps: anyhow chrono clap serde serde_json toml walkdir
├── src/main.rs          # clap: `--config <path>` (env TF2DEMOS_CONFIG), subcommand `organize [--dry-run]`
├── src/demo.rs          # Header::{parse,read,is_in_progress}, Sidecar::{parse,read}, Mark,
│                        # GroupedEvent, group_marks(), parse_ds_name(), TICKS_PER_SEC, HEADER_LEN
├── src/config.rs        # Config::{load,load_from,parse}, demos_dir(), archive_path(),
│                        # index_path(), events_master_path(); never writes the file
├── src/index.rs         # Index::{new,load_or_new,parse,save(atomic),by_id,by_original_name,
│                        # upsert,add_label}; DemoEntry, Event{tick,presses,raw_ticks,label,
│                        # class,rating,streak}, State{Hot,Frozen}
├── src/tf2.rs           # is_running() via /proc; env override TF2DEMOS_TF2_RUNNING=0|1
├── src/archive.rs       # organize()/organize_with(); pure helpers new_demo_name(),
│                        # recorded_at(), parse_events_line(), classify_fold(), plan_fold(),
│                        # by_label_link_name(), by_label_links(), relative_link_target()
└── tests/fixtures/      # 1072-byte .hdr per real demo + real .json sidecars (see §3 table)
```
63 unit tests; `nix flake check` runs tests + `clippy -D warnings`. Read
`src/archive.rs` before changing anything that touches the index: the labelling
flow in session 2 depends on how `organize` upserts entries (§4 step 0).

**Index schema** (`archive/index.json`, version 1) — HANDOFF §4 with these changes:
- Event field `streak: Option<u32>` replaces `length_s` **[user]**.
- Top-level `labels: Vec<String>` (seeded from config `seed_labels` on first
  creation; free-text labels append here via `Index::add_label`) and
  `last_class: Option<String>`.
- `id` = the stem as found on disk at archive time (`2026-09-21_19-51-20` or
  `Tight_scout_m`), same as `original_name` **[s1]**. `file` is relative to
  `tf_dir` (`demos/archive/2026/09/21/...dem`). `recorded_at` is a local-time
  `NaiveDateTime` serialized `2026-09-21T19:51:20`.
- `label/class/rating/streak` serialize as `null` when unset; `frozen_in` is omitted when `None`.

**Config** (`~/.config/tf2demos/config.toml`, read-only store symlink generated by
the HM module; the app never writes it): `tf_dir`, `archive_dir = "demos/archive"`,
`age_hours = 24`, `group_secs = 2.0`, `seed_labels`, `classes`. Missing file →
built-in defaults (same values). Unknown keys are an error, so **new keys need
both `src/config.rs` and `nix/hm-module.nix`**.

**HM module** `services.tf2demos.{enable, package, settings.<key>, onCalendar}`.
`settings` is a submodule with `freeformType = (pkgs.formats.toml {}).type` so one
key can be overridden. When enabled: `home.packages`, `xdg.configFile."tf2demos/config.toml"`,
`systemd.user.services.tf2demos-organize` (oneshot, Nice 10, idle IO, no
environment needed) and `.timers.tf2demos-organize` (`OnCalendar = "daily"`,
`Persistent`, `RandomizedDelaySec = "10min"`).

**Deployment** [verified]: `/etc/nixos/flake.nix` has input
`tf2demos = { url = "github:brucking-9-9/tf2demos"; inputs.nixpkgs.follows = "nixpkgs"; }`;
`/etc/nixos/home/modules/tools/tf2demos.nix` imports the module and enables it;
`home/modules/default.nix` imports that file. The lock pins rev `cc331fc`.
**After every push the lock must move** before `switch` sees the new code:
- `sudo -H nixos-rebuild switch --flake /etc/nixos --update-input tf2demos` — the
  NOPASSWD rule covers `nixos-rebuild` with any arguments [verified in sudoers];
  the flag is listed in `nixos-rebuild --help` [verified] but the end-to-end
  path is **unverified**. Fallback: the user runs `sudo nix flake update tf2demos --flake /etc/nixos`
  (password), then `switch`.
- To test the HM module from the local checkout **before** pushing:
  `--override-input tf2demos git+file:///home/brucking/Projects/tf2demos`
  (use `git+file:`, not `path:` — `path:` copies the untracked `target/` into the
  store). Also unverified. Both are `switch` = checkpoint §5.4.
- Then `~/.dots/sync-nixos.sh "<msg>"` mirrors `/etc/nixos` to GitHub.

**Live state** [verified 2026-09-21 22:00]: timer `tf2demos-organize.timer` next
fires 2026-09-22 00:04; first real run archived `2026-08-16_23-04-42` (pl_pier)
and `2026-09-09_22-09-51` (pl_borneo); three demos from 2026-09-21 are still hot;
`events-orphans.txt` holds the 7 lines for the two hand-deleted demos.

---

## 3. Corrections to HANDOFF.md (cumulative)

| HANDOFF says | Now |
|---|---|
| Watcher runs aging on TF2 exit (§3 Trigger, §4 Watcher loop) | **[user]** Organizing is the daily timer, done. The watcher exists **only** to prompt for labelling: on TF2 exit, count unlabelled marks and notify. It never moves or deletes files. |
| "Never touch `tf/demos` while TF2 is running" (§3, §6) | **[user]** Dropped except for the `_events.txt` rewrite, which `organize` skips while TF2 runs (`SKIP fold, TF2 running`). Session 2 writes to `index.json` only, which TF2 never touches; no TF2 gate needed for labelling. |
| Fold `_events.txt` into day files, then truncate (§3) | Done as: lines for archived demos → `# ds: <raw line>` appended to the day `events.txt`, master rewritten with the remaining lines. Lines matching nothing are orphaned **unless younger than `age_hours`** **[s1]** (protects hand-renamed hot demos like `Tight_scout_m`, whose ds name is gone from disk before it ages in). Hand-renamed demos are matched by `recorded_at ± 5 s` + tick ∈ `raw_ticks`. |
| Event field `length` (§3, §4) | **[user]** `streak: integer`, default 1 when the user leaves it blank. |
| Rename everything to `<date>_<map>.dem` (§3) | Rule is `<stem>_<map>.dem` for every demo; ds names and hand-renamed names alike. Sidecar renamed alongside **[s1]**. |
| by-label symlinks only for labelled demos | Every archived demo has a link. Unlabelled: `by-label/unlabelled/<stem>.dem`. Labelled: `by-label/<label>/<YYYY-MM-DD>_<map>_t<tick>_r<rating or 0>.dem`, one per labelled event, none in `unlabelled` once any event is labelled. `by_label_link_name()` already implements both; regeneration happens on every `organize` run. **Session 2 must call the same regeneration after saving labels** so the tree updates without waiting for the timer. |
| Watcher notification `-A default=Review` (§4) | Still **[assumed]**; verify once with a manual `notify-send` (mako `default-timeout = 5000` [verified in `mako.nix`], so `-t 0` is required). |
| Freeze after 30 days, nested zip (§3, §4) | **Deferred again.** No `cold.rs`, no `zip` crate, no `freeze_days`/`cold_zip` keys. Sub-zips are one per calendar day when it comes. |
| Subcommands `gui | review | watch | age | freeze | thaw | play | search | index` (§4) | Exist: `organize`. Session 2 adds `watch`, `review`, `play`. `gui` (tabs) is session 3. |
| Config keys `poll_secs`, `freeze_days`, `cold_zip` | Session 2 adds `poll_secs` (default 5). The other two stay out. |
| Flake input `path:/home/brucking/Projects/tf2demos` | `github:brucking-9-9/tf2demos`, public, done. |

Unchanged and authoritative: `.dem` header layout and sidecar JSON (HANDOFF §2),
2 s grouping, palette table (§2), review wizard behaviour (§4), playback helper (§4),
keyboard map (§3 Input), the rule that the app never writes `config.toml`/`theme.toml`.

Fixture expectations [verified] (`tests/fixtures/`): `2026-08-16_23-04-42` pl_pier
32562 ticks; `2026-09-09_22-09-51` pl_borneo 107751, 4 marks → 1 event;
`2026-09-21_00-00-37` pl_phoenix 23879, 3 → 1; `2026-09-21_19-51-20` pl_badwater
6978; `Tight_scout_m` pl_badwater 2453; `2026-09-21_20-42-43` pl_thundermountain
0 ticks = in progress (**synthetic** header; the real file was deleted before
session 1 started **[s1]**).

---

## 4. Session-2 scope, in build order

Stop after step 8. Confirm this scope with the user in your first message; it was
chosen 2026-09-21 as "watcher + review wizard + play", with GUI tabs and freeze later.

### Step 0 — decide how hot demos get labelled (design gap, decide at start)
The review must cover demos that are **not archived yet** (a session's demos are
under 24 h old when TF2 exits). Today only archived demos are in the index, and
`Index::upsert` **replaces** an entry with the same `id`. Recommended **[s1 proposal]**:
- At review time, insert hot demos into the index with `file = "demos/<stem>.dem"`,
  `state = Hot`, events from the sidecar (`group_marks`). `id`/`original_name` =
  on-disk stem, exactly what `organize` will use later, so the ids line up.
- Change `organize` so that when it archives a demo whose `id` is already in the
  index it **merges**: keeps `label/class/rating/streak/reviewed` per event
  (match by `tick`), updates `file`, `map`, header fields. Add a test: label a hot
  entry, run `organize`, labels survive. Never let an `organize` run drop a label.
- The review queue = every event with `label == null` across hot + archived
  demos, oldest first; hot demos are read from `tf/demos/*.json` directly for
  demos not yet in the index. Skip in-progress demos (no `.json`).
Write the decision into your final message. If you pick something else, keep the
invariant: **`organize` after `review` loses nothing.**

### Step 1 — `src/tf2.rs`: launch, clipboard, `tf2demos play`
- `play(demo_rel_path, tick)` per HANDOFF §4 Playback helper: TF2 running →
  `wl-copy "playdemo <rel>; demo_gototick <tick>"` + `notify-send`; TF2 closed →
  `steam -applaunch 440 -novid +playdemo <rel> +demo_gototick <tick>`.
  `<rel>` is relative to `tf/` (`demos/archive/2026/09/21/x.dem` or `demos/x.dem`).
- Subcommand `play <id> [--tick N]` (id or archived filename; default tick = first event).
- **Whether `+demo_gototick` at launch works is unverified** and verifying it
  means launching TF2 = checkpoint §5.5. Build the clipboard fallback regardless
  and ask the user to test the launch form once at the end (`~/.claude/skills/run-tf2/`
  explains the desktop takeover).

### Step 2 — `src/watch.rs` + `tf2demos watch`
- Loop every `poll_secs` (new config key, default 5, add to `config.rs` and the
  HM module): `tf2::is_running()`; on running → stopped, wait 5 s for ds to
  flush `.json`, build the review queue (step 0), and if non-empty:
  `notify-send -a tf2demos -t 0 -A default=Review "TF2 closed" "N demos, M marks to review"`.
  If stdout is `default`, spawn `tf2demos review`. Nothing steals focus.
- No file moves, no deletes, no `_events.txt` writes in the watcher.
- Test the transition logic with an injected "is running" closure; do not
  depend on TF2 for tests.

### Step 3 — `tf2demos review`: egui/eframe wizard
- Add `eframe`/`egui` (`--features wayland` on eframe/winit, X11 off). Nix:
  devShell needs `pkg-config libxkbcommon wayland libGL vulkan-loader fontconfig`
  and an `LD_LIBRARY_PATH`; the package needs `autoPatchelfHook` or a
  `wrapProgram --prefix LD_LIBRARY_PATH` because winit/wgpu `dlopen` them.
  Keep `nix flake check` headless: unit-test the queue/state machine, not the window.
- `theme.toml` (`~/.config/tf2demos/theme.toml`, HM-generated, read-only): keys
  `bg panel dim text cyan pink purple yellow green blue` from the HANDOFF §2
  palette; `src/ui/theme.rs` maps them to egui `Visuals`. Default palette
  compiled in so the app runs without the file.
- Wizard per HANDOFF §4 Review wizard: one card per event; map, `mm:ss`
  (`tick / 66.6667`), date, "N presses" if grouped; label buttons `1..9` from
  `index.labels` + free-text box (free text → `Index::add_label`); class (`c`,
  default `last_class`); rating (`r` then 1–5); **streak** (`s`? pick a key that
  does not collide with Skip; integer, default 1); Play at tick (Enter/`p` → step 1);
  Skip; Save & next. Esc leaves; progress saved per card via `Index::save`
  (atomic) and `last_class` updated. A demo becomes `reviewed` when all its
  events are labelled or skipped.
- After every save: regenerate `by-label/` (reuse the archive.rs function; make
  it public/callable without a full organize) so labelled links appear immediately.
- Look: match the rice. Verify with `grim` after focusing via `niri msg` (see
  `run-tf2` skill for the focus/screenshot pattern). Keep the T2 iGPU in mind:
  no continuous `request_repaint`.

### Step 4 — HM module additions
- `services.tf2demos.watcher.enable` (default true when `enable`), generating
  `systemd.user.services.tf2demos-watch`: `Type = "simple"`, `ExecStart = "<pkg> watch"`,
  `Restart = "on-failure"`, `Install.WantedBy = [ "graphical-session.target" ]`,
  `Unit.PartOf = [ "graphical-session.target" ]`. Verify with
  `systemctl --user show-environment` that `WAYLAND_DISPLAY` and
  `DBUS_SESSION_BUS_ADDRESS` reach the unit (HM's user services under a graphical
  session normally inherit them); `notify-send` and `wl-copy` need them, and the
  `review` GUI spawned from the watcher needs `WAYLAND_DISPLAY`.
- `services.tf2demos.theme` (attrs of hex strings, defaults = palette) →
  `xdg.configFile."tf2demos/theme.toml"`. `settings.poll_secs`.
- `PATH` for the watch unit must include `libnotify`, `wl-clipboard`, and `steam`
  (`/run/current-system/sw/bin` or `lib.makeBinPath`); the organize unit needs none.

### Step 5 — prove it locally (§6 + §7 checklist) before any push or `/etc/nixos` change.

### Step 6 — git: commit as work lands; **push is checkpoint §5.3**.

### Step 7 — `/etc/nixos`: move the lock and `switch` (§2 Deployment; checkpoints §5.2, §5.4).
No new files are needed there unless you add `services.tf2demos.theme` overrides.

### Step 8 — first live watcher check: `systemctl --user status tf2demos-watch`,
then ask the user to play one game and confirm the notification appears on exit.

---

## 5. Hard checkpoints — stop and ask; everything else is autonomous **[user]**

1. Any change to what `organize` **moves or deletes**, and the first run of a
   changed `organize` without `--dry-run` on the real `tf/demos`.
2. Any edit to a root-owned file in `/etc/nixos` (`flake.nix`, `configuration.nix`,
   `flake.lock`), and any `sudo git -C /etc/nixos add` (the user runs it; give
   the exact command with the `!` prefix so it runs in-session).
3. `git push`.
4. `nixos-rebuild switch` / the `switch` alias, including `--update-input` /
   `--override-input` forms.
5. Launching TF2 (`steam -applaunch 440`) for any reason, including verifying
   `+demo_gototick`. Read `~/.claude/skills/run-tf2/SKILL.md` first.

Design questions not on this list (step 0 included): decide, note the decision in
your final message, keep going. The user has asked to be asked questions when a
choice materially changes the work; batch them into one `AskUserQuestion` early.

---

## 6. Safety rules (cumulative)

- Never delete a `.dem` that has a `.json` sidecar. Never touch a file newer than
  `age_hours` except to **read** it for review. Never move or delete inside
  `archive/` except the regenerated `by-label/` symlinks.
- Never write `config.toml` or `theme.toml`. Mutable state is `index.json` only,
  written via `Index::save` (atomic rename).
- **Labels are precious.** Any code path that writes `index.json` must load the
  current file first and must not drop `label/class/rating/streak` set by another
  path (`organize` runs at midnight while the user may be reviewing).
- All development runs use a copy. Make it once per session:
  ```
  S=<your scratchpad>; mkdir -p $S/tf
  cp -a "/home/brucking/.local/share/Steam/steamapps/common/Team Fortress 2/tf/demos" $S/tf/demos
  ```
  with a scratch config `tf_dir = "$S/tf"`, `age_hours = 0` for organize tests
  (24 for rehearsing the real shape). The copy now includes `archive/` and
  `index.json`, so review/watch tests have real archived entries to label.
  `TF2DEMOS_TF2_RUNNING=0|1` overrides the process check for scratch runs.
- The watcher must never call `organize`; the timer does that.

---

## 7. Verification checklist (run all before saying done)

- `nix develop -c cargo test`, `nix flake check` (tests + clippy clean, headless),
  `nix build .#default` → `result/bin/tf2demos --help` lists `organize watch review play`.
- Existing session-1 checks still hold on a fresh scratch copy: `organize --dry-run`
  prints only `SKIP`/`LINK`/`DONE` on an organized tree; a real run twice changes
  nothing; `find -L archive/by-label -type l` prints nothing.
- Label round-trip on the scratch copy: label one archived event and one **hot**
  demo's event via the wizard (or a test that drives the same code), `cat index.json`
  shows them, `by-label/<label>/<date>_<map>_t<tick>_r<rating>.dem` exists and
  resolves, `by-label/unlabelled/` no longer lists that demo. Then run `organize`
  with `age_hours = 0`: the hot demo moves and **keeps its label**.
- Watcher transition test: injected running→stopped produces one notification
  call with the right counts; stopped→stopped produces none.
- Manual `notify-send -a tf2demos -t 0 -A default=Review "test" "body"` shows in
  mako and prints `default` when clicked [assumed; verify].
- GUI screenshot via `grim` shows the palette (bg `#120B10`, cyan `#00FFC8`, pink `#FF0055`).
- After `switch`: `systemctl --user status tf2demos-watch` active,
  `systemctl --user list-timers | grep tf2demos-organize` still present,
  `readlink ~/.config/tf2demos/theme.toml` points into `/nix/store`,
  `journalctl --user -u tf2demos-organize -n 20` shows the last nightly run clean.

---

## 8. Deferred — do not build this session

Events/Demos tabs + timeline widget (`tf2demos gui`), search/filter bar,
freeze/thaw cold store (nested zip, one sub-zip per calendar day, `~/Archive/tf2-demos/`),
`search`/`index` subcommands. HANDOFF §3–§5 still describe them, with the §3
corrections above.

---

## 9. Pointers

- `HANDOFF.md` §2 file formats + palette, §4 index/config/wizard/playback, Appendix A verbatim user answers.
- `src/archive.rs` `organize_with()` and `plan_fold()`: the only code that moves files; `by_label_links()` for step 3.
- `nix/hm-module.nix`: extend, keep `settings` freeform; `/etc/nixos/home/modules/terminal/ai/claude_code.nix` lines 221–250 for the unit pattern.
- `/etc/nixos/home/modules/notifications/mako.nix`: `default-timeout = 5000`.
- `/etc/nixos/home/modules/wm/niri.nix`: `rustPlatform` usage; `home/themes/discordo/cyberpunk.toml`, `home/config/waybar/colors/colors.css`: palette sources.
- `~/.claude/skills/run-tf2/`: launch/focus/screenshot driver (checkpoint §5.5).
- `~/.dots/sync-nixos.sh`: mirror `/etc/nixos` after changes.
- Memory dir `~/.claude/projects/-home-brucking-Projects-tf2demos/memory/`: `pgrep-self-match.md`, `session1-state.md`.
