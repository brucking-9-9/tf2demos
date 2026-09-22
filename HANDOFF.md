# tf2demos — implementation handoff

Written 2026-09-21 after a brainstorm session with brucking. This file is the
complete spec for another Claude Code session to implement. Everything here was
either decided by the user (marked **[user]**) or verified on the box
(marked **[verified]**). Assumptions are marked **[assumed]** and should be
confirmed with the user only if they materially change the work.

Project home: `~/Projects/tf2demos` (this directory). **[user]** wants its own
repo + `flake.nix`, later wired into `/etc/nixos/flake.nix` as an input.

---

## 1. What the tool is

A Rust desktop app + background watcher that manages Team Fortress 2 demos
recorded by TF2's built-in demo support (`ds_*` cvars):

1. **Watcher** (systemd user service) notices TF2 exit, ages/cleans the demo
   folder, and sends a mako notification offering to review new marks.
2. **Review** — a wizard that shows each bookmark ("mark") one at a time and
   asks what it was (surf stab, c-tap, matador, ...), plus class, rating, length.
3. **Manager GUI** — tabs Events / Demos / Review; search & filter; play a demo
   at a tick; export/freeze into a nested zip cold store; thaw on demand.

---

## 2. Verified facts about this machine and install **[verified]**

### Paths
- TF2 dir: `/home/brucking/.local/share/Steam/steamapps/common/Team Fortress 2/tf`
  (below: `tf/`).
- Demos: `tf/demos/` (default `ds_dir`). Today it holds 5 `.dem` + 5 `.json` +
  `_events.txt`, 99 MB total. All Casual payload maps.
- Relevant `tf/cfg/config.cfg` lines:
  ```
  bind "DOWNARROW" "record fix; stop; ds_record"
  bind "MOUSE4" "ds_mark"
  ds_enable "2"
  ds_min_streak "1000.000000"
  ds_kill_delay "1000.000000"
  ds_sound "0"
  ds_screens "0"
  ds_autodelete "1"
  ```
  `ds_mark` is called with no argument, so every bookmark value is "General".
  `ds_autodelete 1` is supposed to delete demos with no marks, but the user says
  it sometimes leaves them behind.
- The user manually renamed one demo (`Tight_scout_m.dem` was
  `2026-09-21_19-54-00`). `_events.txt` still refers to the old name. Two demos
  listed in `_events.txt` (2026-09-20_17-22-21, 2026-09-20_17-39-14) no longer
  exist on disk (manually deleted).

### File formats
- **`.dem` header** — fixed 1072 bytes, little-endian:
  | offset | type | field |
  |---|---|---|
  | 0 | char[8] | "HL2DEMO\0" |
  | 8 | i32 | demo protocol (3) |
  | 12 | i32 | network protocol |
  | 16 | char[260] | server name (Casual shows SDR relay `169.254.x.x:port`) |
  | 276 | char[260] | client (player) name — "brucking" |
  | 536 | char[260] | map name e.g. `pl_badwater` |
  | 796 | char[260] | game dir "tf" |
  | 1056 | f32 | playback time seconds |
  | 1060 | i32 | ticks |
  | 1064 | i32 | frames |
  | 1068 | i32 | signon length |
  Python snippet that worked: `struct.unpack_from("<fiii", h, 1056)`. No packet
  parsing is needed for indexing. TF2 ticks are 66.67/s (`tick / 66.6667 = seconds`).
- **`<name>.json` sidecar** written by ds when recording stops (tabs, trailing
  blank line inside the object):
  ```json
  {
  	"events": [
  		{
  			"name": "Bookmark",
  			"value": "General",
  			"tick": 2291
  		}
  	]
  	
  }
  ```
  A demo still recording has no `.json` yet. Other `name` values ds can emit:
  "Killstreak" (value = streak count) — handle generically.
- **`_events.txt`** (appended by ds, `>` separates sessions):
  ```
  >
  [2026/09/21 19:53] Bookmark General ("2026-09-21_19-51-20" at 6964)
  ```
- Observed mark clustering: 2026-09-09 demo has 4 marks at ticks
  48085/48131/48156/48181 (~1.5 s) = one event mashed. 2026-09-21_00-00-37 has
  3 within 26 ticks. Group marks within 2 s.

### Launching / detecting TF2
- Launch: `steam -applaunch 440 <args>`. Extra `+command` args work
  (`+playdemo demos/archive/2026/09/21/x.dem +demo_gototick 6964`). `playdemo`
  paths are relative to `tf/`. Steam ignores `-applaunch` if the game is already
  running. See `~/.claude/skills/run-tf2/SKILL.md` + `driver.py` for a working
  launcher (also handles `-condebug`, niri window focus, grim screenshots).
- Process detection: `pgrep -f 'tf_linux64|hl2_linux|/tf/bin/'` (game runs
  inside Steam's pressure-vessel, nested process tree). Steam itself:
  `pgrep -f ubuntu12_32/steam`.
- Game window title matches "Team Fortress" in `niri msg --json windows`.

### Desktop / tooling
- NixOS unstable, flake at `/etc/nixos`, host "19BK", T2 MacBook (iGPU, keep
  GPU work light). Wayland: niri + waybar + kitty + helix + mako.
- Installed: `python3`, `jq`, `fd`, `rg`, `wl-copy`, `notify-send`, `makoctl`,
  `grim`. **Not** installed: `cargo`, `rustc`, `fzf`, `gum`, `sqlite3`, `xxd`,
  `strings`. Rust toolchain must come from nix (`nix develop` shell in the
  project flake; `rustPlatform.buildRustPackage` for the package).
- Package policy: Nix-native only. No `cargo install` into home.
- mako: bg `#120b10`, text `#00ffc8`, border `#ff0055`, Terminus 12, sharp corners.
  `notify-send -A review=Review ...` returns the action id on click, which the
  watcher can use to launch the GUI.
- home-manager modules are imported from `/etc/nixos/home/modules/default.nix`
  (list of `./tools/*.nix` etc.). `/etc/nixos/home/**` is user-owned; the top
  level (`flake.nix`, `configuration.nix`, `.git`) is root-owned and new files
  need `sudo git -C /etc/nixos add`. Rebuild: `switch` alias
  (`sudo -H nixos-rebuild switch --flake /etc/nixos`). Test build without root:
  `nix build /etc/nixos#nixosConfigurations."19BK".config.system.build.toplevel --out-link /tmp/...`.
  Flake inputs use `inputs.nixpkgs.follows = "nixpkgs"` and `specialArgs`/
  `extraSpecialArgs = { inherit inputs; }` so HM modules can see `inputs`.
- The user's rice palette ("cyberpunk-neon", from `/etc/nixos/home/themes/discordo/cyberpunk.toml`):
  | role | hex |
  |---|---|
  | bg | `#120B10` |
  | selection / panel | `#563466` |
  | dim / muted | `#6D5A78` |
  | text | `#EEFFFF` |
  | cyan (primary accent) | `#00FFC8` |
  | pink (urgent) | `#FF0055` |
  | purple | `#D57BFF` |
  | yellow | `#F4EF00` |
  | green | `#00FF9C` |
  | blue | `#76C1FF` |
  Other themed configs to crib from: `home/themes/{btop,zellij}/cyberpunk.*`,
  `home/config/waybar/colors/colors.css`, `home/config/niri/config.kdl`.

---

## 3. Decisions **[user]**

| Topic | Decision |
|---|---|
| Labelling | **Post-session prompt only.** Keep the single `ds_mark` bind. No in-game label binds. |
| Vocabulary | Label list with number shortcuts (1 = surf stab, 2 = c-tap, 3 = matador, ...) **plus free-text fallback**; a new free-text label is appended to the list. **The growable list must live in a file the app owns** (`labels` inside `index.json`), not in the HM-generated config — HM-generated files are read-only store symlinks on this box (see CLAUDE.md on `settings.json`). The HM config only carries a *seed* list. |
| Event fields | label, **your class** (defaults to last used), **rating 1–5**, **length**. "Length" was chosen from a list; **[assumed]** it means clip length in seconds, defaulting to the demo's duration from the header and editable. Notes field was *not* selected. |
| Mark grouping | One card per event; marks within ~2 s merge into one card with a "N presses" hint. User records **one demo per life**, so multi-event demos are rare. |
| Trigger | **Background watcher = systemd user unit.** On TF2 exit: run aging, then mako notification "TF2 closed: N demos, M marks to review"; clicking opens the GUI in review mode. Nothing steals focus. |
| Aging | **24 h** after recording (file mtime), demo + sidecar move to `tf/demos/archive/<YYYY>/<MM>/<DD>/`. Demos > 24 h with **no `.json`** (unmarked, autodelete leftovers) are **deleted**. Reviewed-but-unlabelled demos are still archived. Never touch `tf/demos` while TF2 is running. |
| Naming | On archive **rename to `<date>_<map>.dem`** (e.g. `2026-09-21_19-51-20_pl_badwater.dem`, sidecar renamed too). Keep original name in the index. |
| Symlinks | Also maintain `tf/demos/archive/by-label/<label>/…` symlinks (readable names, e.g. `2026-09-21_pl_badwater_r4.dem`), regenerated from the index on every change. |
| Events log | Each day dir gets its own `events.txt` (new names, labels, ticks, class, rating). After folding a session's lines into the day files, **truncate the master `tf/demos/_events.txt`**. |
| Cold store | After **30 days** a day directory is zipped to `YYYY-MM-DD.zip` and placed inside **one master zip** `~/Archive/tf2-demos/tf2-demos.zip` (nested: master holds daily sub-zips). Demos then leave `tf/`. **Search still covers frozen demos** via the index; playback thaws (extracts) on demand. |
| Archive location | Inside `tf/` so `playdemo demos/archive/...` keeps working from the console. |
| Toolkit | **egui / eframe** (Claude's pick; user delegated: "whatever an AI would be best at ensuring it looks good"). Rationale: stable API, easy custom painting for the timeline, quick iteration verified by grim screenshots. |
| Layout | **Tabs: Events / Demos / Review (badge count).** Shared filter bar. Events tab = clip-library list (label · map · date · class · rating). Demos tab = master/detail with a timeline strip showing marks. |
| Theme | **Match the rice palette** (table above). Colours read from a `theme.toml` so the `rice` skill can retune them. |
| Input | **Keyboard-first, mouse works too.** j/k or arrows move, `/` search, Enter play, 1–9 label, c class, r rating, l length, Esc back, Tab switch tab; every action also a clickable button. |
| Playback | TF2 closed → `steam -applaunch 440 +playdemo <rel path> +demo_gototick <tick>` (**[assumed]**: whether `demo_gototick` queued at launch takes effect before playback starts is unverified; fallback is to also `wl-copy "demo_gototick <tick>"` so the user can paste it once the demo is up. Test the console form `playdemo X; demo_gototick N` first, it is the reliable one). TF2 running → `wl-copy "playdemo <rel>; demo_gototick <tick>"` + notification telling the user to paste in console. Frozen demo → thaw to `tf/demos/archive/_thaw/` first. |
| Recall aid in review | **Play-at-tick only.** Do *not* enable `ds_screens`. |
| Code home | Own repo here + flake exposing the package and a home-manager module (binary, watcher unit, config). Wired into `/etc/nixos/flake.nix` as an input — that file is root-owned, so ask the user before editing it. |

---

## 4. Proposed architecture

```
tf2demos/
├── flake.nix              # devShell (rust toolchain), packages.default, homeManagerModules.default
├── Cargo.toml             # single crate, binary `tf2demos`
├── src/
│   ├── main.rs            # clap: gui | review | watch | age | freeze | thaw | play | search | index
│   ├── config.rs          # ~/.config/tf2demos/config.toml + theme.toml
│   ├── demo.rs            # .dem header parser, sidecar .json parser, mark grouping
│   ├── index.rs           # index.json load/save, queries
│   ├── archive.rs         # aging, rename, day dirs, events.txt, by-label symlinks
│   ├── cold.rs            # nested zip freeze/thaw (zip crate)
│   ├── tf2.rs             # process detection, launch, clipboard
│   ├── watch.rs           # poll loop + notify-send action handling
│   └── ui/
│       ├── mod.rs         # eframe App, tabs, keymap
│       ├── theme.rs       # palette -> egui Visuals
│       ├── events_tab.rs
│       ├── demos_tab.rs   # includes timeline widget (painter)
│       └── review.rs      # one-card-at-a-time wizard
└── nix/hm-module.nix      # services.tf2demos.{enable, config}, systemd.user.services.tf2demos-watch
```

### Index (`tf/demos/archive/index.json`) — start with JSON, dataset is tiny
```json
{
  "version": 1,
  "demos": [{
    "id": "2026-09-21_19-51-20",
    "file": "archive/2026/09/21/2026-09-21_19-51-20_pl_badwater.dem",
    "original_name": "2026-09-21_19-51-20",
    "map": "pl_badwater", "server": "169.254.240.159:13144",
    "recorded_at": "2026-09-21T19:51:20", "seconds": 104.7, "ticks": 6978,
    "state": "hot" | "frozen",          "frozen_in": "2026-09-21.zip",
    "reviewed": true,
    "events": [{
      "tick": 6964, "presses": 1, "raw_ticks": [6964],
      "label": "matador", "class": "spy", "rating": 4, "length_s": 104.7
    }]
  }],
  "last_class": "spy"
}
```
Also stored at top level: `"labels": ["surf stab", "c-tap", "matador"]` — the live, growable
label list (seeded from config on first run; free text appends here). Config `labels` is only the seed.
`recorded_at`: parse from the `YYYY-MM-DD_HH-MM-SS` filename; for hand-renamed demos
(`Tight_scout_m.dem`) fall back to `mtime - header seconds`.

### Config (`~/.config/tf2demos/config.toml`, generated by the HM module)
```toml
tf_dir      = "/home/brucking/.local/share/Steam/steamapps/common/Team Fortress 2/tf"
archive_dir = "demos/archive"      # relative to tf_dir so playdemo works
cold_zip    = "/home/brucking/Archive/tf2-demos/tf2-demos.zip"
age_hours   = 24
freeze_days = 30
group_secs  = 2.0
poll_secs   = 5
seed_labels = ["surf stab", "c-tap", "matador"]   # copied into index.json on first run; this file is read-only
classes     = ["scout","soldier","pyro","demoman","heavy","engineer","medic","sniper","spy"]
```
`theme.toml`: the palette table from §2, keys `bg, panel, dim, text, cyan, pink, purple, yellow, green, blue`.
Both files are generated by the HM module and therefore read-only; the app must never write to them.

### Watcher loop (`tf2demos watch`)
1. Every `poll_secs`, check `pgrep -f 'tf_linux64|hl2_linux|/tf/bin/'` (or read `/proc` directly with `procfs`/`sysinfo`).
2. On running→stopped transition: wait ~5 s for ds to flush `.json`, then
   `age` (move >24 h demos with `.json`, delete >24 h without), `freeze` (>30 d),
   fold `_events.txt` into day `events.txt` files and truncate it, rebuild
   index for any new demos (parse header + sidecar, group marks).
3. Count unreviewed events. If > 0:
   `notify-send -a tf2demos -t 0 -A default=Review "TF2 closed" "N demos, M marks to review"`;
   if stdout == "default", spawn `tf2demos gui --review`.
   **[assumed]** mako's left click runs `invoke-default-action`, which fires the
   action keyed literally `default`, hence `-A default=...` (not `review=`).
   `-t 0` is required because the user's mako has `default-timeout = 5000`
   (5 s), so the prompt would otherwise vanish. Verify once with a manual
   `notify-send` before wiring it into the unit.
4. systemd unit: `Type=simple`, `Restart=on-failure`, `WantedBy=graphical-session.target`,
   `Environment` needs `WAYLAND_DISPLAY`/`DBUS_SESSION_BUS_ADDRESS` (HM's
   `systemd.user.services` under a graphical session inherits them via
   `graphical-session.target`; verify with `systemctl --user show-environment`).

### Nested zip cold store
- Master `tf2-demos.zip` entries = `YYYY-MM-DD.zip`, each written with
  `CompressionMethod::Stored` so the bytes are contiguous and seekable.
- Inside a day zip, `.dem`/`.json`/`events.txt` may be Deflated.
- Thaw one demo: open master → locate day entry → get its data start offset and
  size → wrap a `Take<Seek>` slice as a nested `ZipArchive` → extract the one
  `.dem` to `archive/_thaw/`. No full unpack.
- Adding a day: use `ZipWriter::new_append(file)` from the `zip` crate to append
  the new day entry in place (O(day size), not O(master size)). Stream the day
  zip in, don't buffer it. For safety, take a copy or verify the central
  directory reads back after the append before deleting the source day dir.
- After a successful freeze, delete the day dir from `tf/`; update index
  `state = frozen`; by-label symlinks for frozen demos point nowhere, so either
  drop them or point at the thaw path (suggest: drop, and re-create on thaw).
- by-label symlink names must include the tick
  (`2026-09-21_pl_badwater_t6964_r4.dem`) or two same-label events in one demo collide.

### Review wizard
- Queue = all events where `label == null` across unreviewed demos, oldest first.
- Card: map, `mm:ss` into demo (tick/66.67), demo date, "N presses" if grouped,
  label buttons `1..9` from config + text box (free text → appended to config),
  class selector (`c`, defaults to `last_class`), rating (`r` then 1–5 or click
  stars), length (`l`, default demo seconds), **Play at tick** (Enter or `p`),
  **Skip** (`s`), **Save & next** (Enter when label set). Esc leaves review;
  progress is saved per card.
- Demo marked `reviewed` when all its events are labelled or skipped.

### Timeline widget (Demos tab)
- egui `Painter`: a horizontal bar of demo length, ticks drawn as small
  diamonds coloured by label (hash label → palette accent), hover shows
  label/time, click selects, Enter plays at that tick.

### Playback helper
```
fn play(demo, tick) {
  if tf2_running() { wl-copy "playdemo {rel}; demo_gototick {tick}"; notify("Copied to clipboard — paste in console") }
  else { steam -applaunch 440 -novid +playdemo {rel} +demo_gototick {tick} }
}
```
`rel` is relative to `tf/` (e.g. `demos/archive/2026/09/21/x.dem`), `.dem` extension optional for TF2.
**[assumed]** `+demo_gototick` at launch — see Playback row in §3 for the fallback.

### Nix
- `flake.nix`: inputs nixpkgs (+ optionally rust-overlay or just nixpkgs `cargo`/`rustc`),
  `devShells.default` with cargo, rustc, rust-analyzer, clippy, pkg-config, and
  eframe's runtime deps: `libxkbcommon wayland libGL vulkan-loader fontconfig`
  (set `LD_LIBRARY_PATH` in the devShell; in the package use `autoPatchelfHook`
  or `wrapProgram --prefix LD_LIBRARY_PATH` because winit/wgpu dlopen these).
  Build with `--features wayland` on eframe/winit; X11 can stay off.
- `packages.default = rustPlatform.buildRustPackage { cargoLock.lockFile = ./Cargo.lock; ... }`.
- `homeManagerModules.default`: `options.services.tf2demos.{enable, settings}`;
  writes `~/.config/tf2demos/config.toml` + `theme.toml` (read-only symlinks —
  all mutable state goes in `index.json`), installs the binary,
  defines `systemd.user.services.tf2demos-watch`.
- In `/etc/nixos`: add input `tf2demos.url = "path:/home/brucking/Projects/tf2demos"`
  (or a GitHub URL once pushed — the user mirrors config to GitHub via
  `~/.dots/sync-nixos.sh`, SSH only) with `inputs.nixpkgs.follows = "nixpkgs"`;
  create `home/modules/tools/tf2demos.nix` that imports
  `inputs.tf2demos.homeManagerModules.default` and enables it; add it to
  `home/modules/default.nix`. **flake.nix is root-owned: ask before editing.**
  New files need `sudo git -C /etc/nixos add`.

---

## 5. Suggested build order
1. `flake.nix` devShell + Cargo skeleton; `demo.rs` header/sidecar parsers with
   tests against the real files in `tf/demos` (5 demos listed in §2).
2. `index.rs` + `tf2demos index` (scan hot + archive, write index.json).
3. `archive.rs`: `age` (dry-run flag first!), rename, day dirs, `events.txt`
   fold, `by-label` symlinks. Test on a copy of `tf/demos` in a temp dir.
4. `tf2.rs` + `play`. Verify a launch with `steam -applaunch 440 +playdemo ...`
   (takes over the desktop — coordinate with the user, see run-tf2 skill's
   `--i-am-at-my-desk` guard).
5. `watch.rs` + HM module unit; test with `systemctl --user`.
6. GUI: theme → Events tab → Demos tab + timeline → Review wizard. Screenshot
   with `grim` after focusing the window via `niri msg` to check the look.
7. `cold.rs` freeze/thaw with nested zips; test round-trip on a temp copy.
8. Wire into `/etc/nixos` (ask first), `switch`, then `~/.dots/sync-nixos.sh`.

## 6. Safety rules for the implementer
- Never move/delete in `tf/demos` while TF2 runs; never delete a `.dem` that
  has a `.json` sidecar; `age` and `freeze` get `--dry-run` and log every
  action; freezes verify the zip before deleting the source.
- The current `tf/demos` contents are real and the user cares about them. Do
  development runs against a copy.
- Keep the T2 MacBook iGPU in mind: eframe at default 60 fps repaint-on-input
  is fine; don't `request_repaint` continuously.

---

## Appendix A — the brainstorm Q&A, verbatim

The user's answers are the primary source; §3 is Claude's interpretation of them.
Re-read these when an interpretation seems off.

**Original request:** "A tf2 demos managager I would like you to ask me questions on it, I would like demos to be moved to a new folder after a day and i would like to be propted after each session about the events and what they were surf stab, c-tap, matador, and finaly a manager for all of this so i can interat and serch for diffrent demos. Please familrize your self with ./demos"

| # | Question | Answer (verbatim) |
|---|---|---|
| 1 | How should events get their labels? | "Post-session prompt only" |
| 2 | Where should the archive folder live? | "Can it be both in the archive and outsied of it as a partioned zip" |
| 3 | What should trigger the post-session prompt? | "Background watcher (systemd user unit)" |
| 4 | What form should the manager take? | "can you use rust plaese" |
| 5 | What do you mean by a partitioned zip outside the archive? | "Zip is long term but still acesible from the manager when serching and it's one master zip that has sub zips? which hold the daily demos" |
| 6 | For the Rust manager, what shape should the interactive part take? | "TUI works but could you look into gui" |
| 7 | How should the event vocabulary work? | "Config list + free text fallback (Recommended)" |
| 8 | What does 'moved after a day' mean, and what about skipped demos? | "24h and keep untagged and removed unmarked as some times unmarked demos dont get delited also archive/<year>/<month>/<day> would be nice" |
| 9 | Which Rust GUI toolkit? (egui / iced / GTK4+relm4 / Slint) | "What ever you think that An AI would be the best at ensuing that it looks good" → Claude chose egui/eframe |
| 10 | What should the main window be organised around? | "Both, as tabs" (Events / Demos / Review) |
| 11 | How should the review appear when TF2 closes? | "mako notification, click opens review (Recommended)" |
| 12 | During review, how will you remember what each mark was? (screenshots via ds_screens and/or play-at-tick) | "Play-at-tick only" |
| 13 | What should the app look like? | "Match the rice palette (Recommended)" |
| 14 | How should you drive it? | "Keyboard-first, mouse works too (Recommended)" |
| 15 | What should 'play at tick' do, given TF2 may or may not be running? | "Launch if closed, clipboard if running (Recommended)" |
| 16 | How should the review wizard step through marks? | "Can it be One mark at a time but be aware of bunch marks and also i tend to try to do one demo per life so muit marks are rare" |
| 17 | Where should the Rust project live / how installed? | "Own repo + flake, wired into /etc/nixos (Recommended)" |
| 18 | Where does the master zip live, and when does a demo move into it? | "~/Archive/tf2-demos/, after 30 days (Recommended)" |
| 19 | Filename policy: should the manager rename files? | "can you both Rename to date_map and also add a symlink folder for by-label also for each day create a events.txt for that day with the labels and clear the master one" |
| 20 | Which fields should each event carry besides the label? (offered: Notes, Your class, Rating 1-5, Victim class/weapon) | "Your class, Rating 1-5, Length" — "Length" was typed by the user, not an offered option; Claude interpreted it as clip length in seconds |

Mid-session the user also said: "can you ask me more front end questions thanks" (led to Q9–Q16) and finally "Please make all these instructions into a file for another session to figure out how to implement try to proived as much info as possible".
