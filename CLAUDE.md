# tf2demos — implementer's operating manual

Read this whole file before doing anything. `HANDOFF.md` in this directory is the
full spec (data formats, decisions, architecture, the original Q&A). This file is
the **operating manual** for the session that builds it: what to build first, which
rules to obey, where HANDOFF is now out of date, and where to stop and ask.

**Where this file and HANDOFF.md disagree, this file wins.** Do not edit HANDOFF.md.

Written 2026-09-21 after a second Q&A round with the user (brucking). Everything
marked **[user]** was decided by them; **[verified]** was checked on this machine.

---

## 1. Read this first

**What it is.** A Rust tool that manages Team Fortress 2 demos recorded by TF2's
built-in `ds_*` demo support. Long-term it grows into a GUI with a labelling
wizard (HANDOFF §1). **This session builds only the daily organizing job**: a
`tf2demos organize` subcommand, packaged by a Nix flake, run by a systemd user
timer that a home-manager module installs.

**Environment you are in** [verified]:
- NixOS unstable, flake at `/etc/nixos`, host `19BK`, Wayland (niri). The user's
  global rules live in `~/.claude/CLAUDE.md` and apply here too (nix-native only,
  `/etc/nixos` top level is root-owned, `switch` alias rebuilds).
- **No `cargo`, `rustc`, or `rust-analyzer` on PATH.** nixpkgs has Rust 1.97.x.
  The toolchain comes from this repo's `flake.nix` devShell: run everything via
  `nix develop -c <cmd>` (or `nix develop` then work inside the shell).
- **TF2 may be running at any moment**, including while you work. Do not launch
  it, kill it, or assume it is closed. Check with
  `pgrep -f 'tf_linux64|hl2_linux'`.
- The real demo directory is
  `/home/brucking/.local/share/Steam/steamapps/common/Team Fortress 2/tf/demos/`
  (below: `tf/demos`). **The user cares about every file in it.** Develop and
  test against a copy in your scratchpad directory (§5). Read-only commands
  (`ls`, `head -c 1072`, `organize --dry-run`) against the real directory are fine.
- Project git identity is already configured globally (`brucking-9-9`,
  `bruck@tekle.com`). SSH auth only, no `gh`, no HTTPS tokens.

---

## 2. Corrections to HANDOFF.md

| HANDOFF says | Now **[user]** unless noted |
|---|---|
| A watcher runs aging when TF2 exits (§3 Trigger, §4 Watcher loop) | Organizing is a **daily systemd user timer**, unrelated to whether TF2 is running. The TF2-exit watcher exists only to prompt for labelling and is a **later session**. |
| "Never touch `tf/demos` while TF2 is running" (§3 Aging, §6) | Dropped for moves and deletes. The guard is the **24 h mtime threshold**: a demo cannot be 24 h old while still recording, and TF2 only plays archived demos by explicit path. Kept for **exactly one step**: rewriting `_events.txt`, because ds appends to that file mid-game. If `pgrep -f 'tf_linux64\|hl2_linux'` matches, skip the `_events.txt` rewrite (log "skipped, TF2 running") and do it on the next run. Everything else proceeds. |
| Fold `_events.txt` into day files, then truncate it (§3 Events log) | Fold **only the lines whose demo is now in the archive**, then **rewrite** `_events.txt` with the remaining lines (a demo under 24 h keeps its lines until it is archived). Lines whose demo exists neither in `tf/demos` nor in the archive nor in the index go to `archive/events-orphans.txt` (append, then remove from master). Match lines by the **original** ds name, e.g. `"2026-09-21_19-54-00"`, which the index stores as `original_name`. Two orphan lines exist today [verified]: demos `2026-09-20_17-22-21` and `2026-09-20_17-39-14` were deleted by hand. |
| Event field "length" = clip length in seconds (§3 Event fields, §4 index `length_s`) | The field is **`streak`**: an integer, the kill streak / combo length of the play. Default `1`. Replace `length_s` everywhere. |
| Rename everything to `<date>_<map>.dem` (§3 Naming) | ds-named demos (`YYYY-MM-DD_HH-MM-SS`) → `<date>_<map>.dem`. **Hand-renamed** demos keep the user's name and get the map appended: `Tight_scout_m.dem` → `Tight_scout_m_pl_badwater.dem`. `recorded_at` for those = `mtime − header seconds`. `original_name` in the index is the name as found on disk. |
| by-label symlinks only for labelled demos (§3 Symlinks, §4) | **Every archived demo always has a symlink.** Unlabelled: `archive/by-label/unlabelled/<stem>.dem` where `<stem>` is the archived filename without extension. Labelled (later session): `archive/by-label/<label>/<date>_<map>_t<tick>_r<rating>.dem`. Symlinks are **relative** (`../../2026/09/21/x.dem`) so the tree survives a move of `tf/`. The whole `by-label/` tree is deleted and regenerated from the index on every run. Implement the naming function for both cases now, even though labels do not exist yet. |
| Freeze to a nested zip after 30 days (§3 Cold store, §4 Nested zip) | **Deferred. Do not build.** No `cold.rs`, no `zip` crate, no `freeze_days`/`cold_zip` config keys. When it is built later, sub-zips are **one per calendar day**, never per play session. |
| Subcommand `age` (§4 main.rs) | Named **`organize`**. It does age + rename + day `events.txt` + `_events.txt` fold + `by-label/` regeneration + index update in one pass. `--dry-run` prints every action it would take and writes nothing. |
| Flake input `path:/home/brucking/Projects/tf2demos` (§4 Nix) | `github:brucking-9-9/tf2demos`, a **public** repo (root fetches it during `switch`; private would need tokens, which the user forbids). |
| Config key `poll_secs`, `freeze_days`, `cold_zip` | Not in session 1. Keep `tf_dir`, `archive_dir`, `age_hours`, `group_secs`, `seed_labels`, `classes`. |

Unchanged and still authoritative: `.dem` header layout and sidecar JSON (§2 File
formats), the 2 s mark grouping, the index schema in §4 (with `streak`), the
config file location and the rule that the app **never writes** `config.toml`.

---

## 3. Session-1 scope, in build order

Stop after step 8. GUI, review wizard, watcher, play, freeze are §7.

### Step 1 — repo skeleton
- `git init` (branch `main`), `.gitignore` with `target/` and `result`.
- `Cargo.toml`: package `tf2demos`, edition 2024, binary `tf2demos`. Dependencies
  for this session only: `clap` (derive), `serde`, `serde_json`, `toml`, `chrono`,
  `anyhow`, `walkdir`. **No `eframe`/`egui` yet**, so the Nix package needs no
  Wayland/GL runtime dependencies and `buildRustPackage` stays trivial.
- `flake.nix` outputs:
  - `devShells.default`: `cargo rustc rust-analyzer clippy rustfmt`.
  - `packages.default`: `rustPlatform.buildRustPackage` with
    `cargoLock.lockFile = ./Cargo.lock`, `meta.mainProgram = "tf2demos"`.
  - `checks`: run `cargo test` and `cargo clippy -- -D warnings` (e.g. via
    `packages.default.overrideAttrs` with `checkPhase`, or a small derivation).
  - `homeManagerModules.default` (step 5). A bare `import ./nix/hm-module.nix`
    cannot see this flake's package, so wrap it:
    `homeManagerModules.default = { pkgs, lib, ... }@args: (import ./nix/hm-module.nix args) // { config.services.tf2demos.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.default; }`
    or, cleaner, have `nix/hm-module.nix` take `self` as an extra argument:
    `homeManagerModules.default = import ./nix/hm-module.nix { inherit self; };`
    with the module file written as `{ self }: { config, lib, pkgs, ... }: { ... }`.
- **Pin this flake's nixpkgs to the system's rev** so `nix develop` reuses the
  cached toolchain and the `follows` in `/etc/nixos` changes nothing. Find the
  rev with:
  ```
  nix flake metadata /etc/nixos --json | jq '.locks.nodes | to_entries[] | select(.key|test("nixpkgs")) | {key, rev: .value.locked.rev, date: .value.locked.lastModified}'
  ```
  The lock node is not necessarily named plain `nixpkgs` and an earlier probe
  returned a suspiciously old rev, so cross-check with
  `nixos-version --json | jq -r .nixpkgsRevision`, which reports the rev of the
  **running** system; that is the one to pin against. Then
  `nix flake lock --override-input nixpkgs github:nixos/nixpkgs/<rev>`.
- Generate the lockfile inside the shell: `nix develop -c cargo generate-lockfile`.
  Commit `Cargo.lock`; the Nix build needs it.

### Step 2 — `src/demo.rs`: header, sidecar, grouping
- Header parser per HANDOFF §2 table (1072 bytes, little-endian, `HL2DEMO\0`
  magic, strings are NUL-padded `char[260]`). Return a struct with server, client,
  map, seconds, ticks, frames. Reject files shorter than 1072 bytes or with a bad
  magic with a clear error.
- Sidecar parser: `{"events":[{"name","value","tick"}]}`; tabs and a blank line
  inside the object are normal. Unknown `name` values pass through.
- Grouping: sort marks by tick, merge any within `group_secs` (66.6667 ticks/s)
  of the previous mark into one event with `presses` = count and `raw_ticks`.
- **Tests with committed fixtures.** Cut the first 1072 bytes of every real
  demo into `tests/fixtures/<name>.hdr` (`head -c 1072 "<tf>/demos/<name>.dem"`)
  and copy the real `.json` sidecars beside them. These are tiny. Expected
  values [verified 2026-09-21]:

  | file | map | ticks | seconds | marks (grouped) |
  |---|---|---|---|---|
  | `2026-08-16_23-04-42` | `pl_pier` | 32562 | 488.43 | 1 → 1 event |
  | `2026-09-09_22-09-51` | `pl_borneo` | 107751 | 1616.27 | 4 (48085…48181) → 1 event, 4 presses |
  | `2026-09-21_00-00-37` | `pl_phoenix` | 23879 | 358.18 | 3 (18564…18590) → 1 event, 3 presses |
  | `2026-09-21_19-51-20` | `pl_badwater` | 6978 | 104.67 | 1 → 1 event |
  | `Tight_scout_m` | `pl_badwater` | 2453 | 36.79 | 1 (2291) → 1 event |
  | `2026-09-21_20-42-43` | `pl_thundermountain` | 0 | 0.0 | no sidecar: was recording when checked |

  Every header has demo protocol 3, network protocol 24, client `brucking`, game dir `tf`.
  The last row is the in-progress case: ticks 0 and no `.json`. It must parse
  without error and be reported as "in progress / unfinished".

### Step 3 — `src/config.rs` and `src/index.rs`
- Config: `~/.config/tf2demos/config.toml`, overridable by `--config <path>` and
  env `TF2DEMOS_CONFIG`. Keys and defaults (HANDOFF §4, trimmed):
  ```toml
  tf_dir      = "/home/brucking/.local/share/Steam/steamapps/common/Team Fortress 2/tf"
  archive_dir = "demos/archive"   # relative to tf_dir so `playdemo demos/archive/...` works
  age_hours   = 24
  group_secs  = 2.0
  seed_labels = ["surf stab", "c-tap", "matador"]
  classes     = ["scout","soldier","pyro","demoman","heavy","engineer","medic","sniper","spy"]
  ```
  The file is a read-only Nix store symlink on this box. **The app never writes it.**
- Index: `<tf_dir>/<archive_dir>/index.json`, schema from HANDOFF §4 with
  `streak` (integer) instead of `length_s`, plus top-level `labels` (seeded from
  `seed_labels` on first creation, growable later) and `last_class`. Load,
  save (atomic: write `index.json.tmp` then rename), lookup by `id` and by
  `original_name`. All mutable state lives here.

### Step 4 — `src/archive.rs` and `tf2demos organize [--dry-run]`
Rules, applied to `<tf_dir>/demos` (non-recursive, skipping `archive/`):
1. Candidate = every `*.dem` whose mtime is older than `age_hours`. Anything
   newer is skipped and logged as "too new".
2. Candidate **with** a `.json` sidecar → parse header + sidecar → destination
   `archive/<YYYY>/<MM>/<DD>/` from `recorded_at` (filename date for ds names,
   `mtime − seconds` otherwise) → new name per §2 → move `.dem` and `.json`
   (rename within the same filesystem; verify destination exists and sizes
   match before treating the move as done) → add/replace the index entry →
   append a line to `archive/<Y>/<M>/<D>/events.txt` per grouped event
   (`<new name>  tick=<tick> presses=<n> label=- class=- rating=- streak=-`).
3. Candidate **without** a sidecar → **delete** (these are `ds_autodelete`
   leftovers or crashed recordings). Log the name and size. Never delete a
   `.dem` that has a sidecar.
4. Fold `_events.txt` per §2 (skip the rewrite if TF2 is running).
5. Delete and regenerate `archive/by-label/` from the index.
6. Save the index.
- Every action is one line on stdout (`MOVE`, `DELETE`, `FOLD`, `ORPHAN`,
  `LINK`, `SKIP <reason>`); stdout lands in the journal under the timer.
- `--dry-run` performs steps 1–5 in memory and prints the same lines prefixed
  `[dry-run]`, writing nothing.
- **Idempotent**: running `organize` twice on an organized tree changes nothing
  and the second run prints only `SKIP` lines or nothing.
- Refuse to run without `--dry-run` if `<tf_dir>/demos` does not exist (guards a
  wrong `tf_dir`).

### Step 5 — `nix/hm-module.nix`
Model on `/etc/nixos/home/modules/terminal/ai/claude_code.nix` lines 221–250
(`claude-flake-report`: oneshot service + `Persistent = true` timer). "Use NixOS
to its fullest" **[user]** means: typed options, generated config, declarative
timer, nothing hand-installed.
- `options.services.tf2demos.enable = lib.mkEnableOption "...";`
- `options.services.tf2demos.settings` typed as `(pkgs.formats.toml {}).type`,
  defaults = the config block in step 3.
- `options.services.tf2demos.package` defaulting to this flake's package.
- `config` when enabled: `home.packages = [ cfg.package ]`,
  `xdg.configFile."tf2demos/config.toml".source = settingsFormat.generate ...`,
  `systemd.user.services.tf2demos-organize` (`Type = "oneshot"`,
  `ExecStart = "${lib.getExe cfg.package} organize"`, `Nice = 10`,
  `IOSchedulingClass = "idle"`), `systemd.user.timers.tf2demos-organize`
  (`OnCalendar = "daily"`, `Persistent = true`, `RandomizedDelaySec = "10min"`,
  `Install.WantedBy = [ "timers.target" ]`).
- The service needs no Wayland/DBus environment in this session (no
  notifications yet).

### Step 6 — prove it locally
Run the §6 checklist against the scratch copy **before** any git push or
`/etc/nixos` change. Iterating through GitHub is slow (push → lock update in
root-owned `/etc/nixos` → `switch`), so develop entirely inside this flake and
wire it in once at the end.

### Step 7 — git and GitHub
- Commit as work lands, short conventional messages, `Co-Authored-By` trailer
  per the global rules.
- **CHECKPOINT (§4.3):** ask the user to create the empty **public** repo
  `brucking-9-9/tf2demos` on github.com (no README, no license, so the first push
  is clean). Then:
  ```
  git remote add origin git@github.com:brucking-9-9/tf2demos.git
  git push -u origin main
  ```

### Step 8 — wire into `/etc/nixos`
**CHECKPOINT (§4.2) before touching anything here**; `flake.nix` is root-owned.
1. `flake.nix` input:
   ```nix
   tf2demos = {
     url = "github:brucking-9-9/tf2demos";
     inputs.nixpkgs.follows = "nixpkgs";
   };
   ```
2. New `home/modules/tools/tf2demos.nix` (user-owned dir, no sudo to create):
   ```nix
   { inputs, ... }:
   {
     imports = [ inputs.tf2demos.homeManagerModules.default ];
     services.tf2demos.enable = true;
   }
   ```
   `inputs` reaches HM modules through `extraSpecialArgs` already [verified].
3. Add `./tools/tf2demos.nix` to the imports list in `home/modules/default.nix`.
4. The user must stage the new file (`.git` is root-owned):
   `sudo git -C /etc/nixos add home/modules/tools/tf2demos.nix`.
5. Test build without root, from your scratchpad:
   `nix build /etc/nixos#nixosConfigurations."19BK".config.system.build.toplevel --out-link <scratchpad>/result --no-write-lock-file`
   (never create `./result` inside `/etc/nixos`). The new input needs a
   `flake.lock` entry and the lock is root-owned, so without
   `--no-write-lock-file` the unprivileged build errors out; with it, expect a
   warning. Alternatively the user runs `sudo nix flake lock /etc/nixos`
   (password prompt, not NOPASSWD). `switch` runs as root and writes the lock.
6. **CHECKPOINT (§4.4):** `switch`. Then `~/.dots/sync-nixos.sh "tf2demos: organize timer"`.
- Optional, **unverified**: `nixos-rebuild switch --flake /etc/nixos --override-input tf2demos path:/home/brucking/Projects/tf2demos`
  may let you test the HM module before pushing. Confirm the flag is accepted
  by this `nixos-rebuild` before relying on it.

---

## 4. Hard checkpoints — stop and ask; everything else is autonomous **[user]**

1. The first run of `organize` **without** `--dry-run` against the real `tf/demos`.
2. Any edit to a root-owned file in `/etc/nixos` (`flake.nix`, `configuration.nix`),
   and any `sudo git -C /etc/nixos add` (the user runs it).
3. `git push` (the user must have created the repo first).
4. `nixos-rebuild switch` / the `switch` alias.
5. Launching TF2 (`steam -applaunch 440`) for any reason. Not expected this session.
   If it ever is: `~/.claude/skills/run-tf2/SKILL.md` explains why it seizes the desktop.

Design questions that are not on this list: decide, note the assumption in your
final message, keep going.

---

## 5. Safety rules (HANDOFF §6, amended)

- Never delete a `.dem` that has a `.json` sidecar. Never touch a file newer than
  `age_hours`. Never move or delete inside `archive/` except the regenerated
  `by-label/` symlinks.
- Never write `config.toml` (or the future `theme.toml`). Mutable state is
  `index.json` only.
- A move is complete only when the destination exists with the same size; only
  then remove the source. Save the index after the moves, atomically.
- **All development runs use a copy.** Make it once per session:
  ```
  cp -a "/home/brucking/.local/share/Steam/steamapps/common/Team Fortress 2/tf/demos" <scratchpad>/tf/demos
  ```
  and a scratch config with `tf_dir = "<scratchpad>/tf"` **and `age_hours = 0`**,
  so every demo qualifies regardless of the date you run on (the expectations in
  §6 assume that; the real config keeps 24). The real directory is ~117 MB
  [verified]; the copy is cheap. Reset the copy with the same command.
- `_events.txt` rewrite is the one TF2-aware step (§2); do not add other gates.

---

## 6. Verification checklist (run all before saying done)

- `nix develop -c cargo test` passes using the header fixtures (all six rows of
  the step-2 table asserted exactly, plus grouping and in-progress cases).
- `nix flake check` passes (tests + clippy clean).
- `nix build .#default` yields `result/bin/tf2demos`; `result/bin/tf2demos --help`
  lists `organize`.
- `organize --dry-run --config <scratch>` on a fresh copy prints: 5 `MOVE` lines
  with the new names (`2026-08-16_23-04-42_pl_pier.dem`,
  `2026-09-09_22-09-51_pl_borneo.dem`, `2026-09-21_00-00-37_pl_phoenix.dem`,
  `2026-09-21_19-51-20_pl_badwater.dem`, `Tight_scout_m_pl_badwater.dem`), the
  correct day directories, one `SKIP too new` or `DELETE` for
  `2026-09-21_20-42-43.dem` depending on its age at run time, `FOLD` lines for the
  archived demos including the `Tight_scout_m` line matched via its original
  name `2026-09-21_19-54-00`, and 2 `ORPHAN` lines. (If the real directory has
  changed since 2026-09-21, adjust expectations from what is actually there.)
- Real run on the copy, then a second run: the second run changes nothing.
  `find <scratch>/tf/demos` shows the day tree, `events.txt` per day,
  `events-orphans.txt`, `index.json`, and `by-label/unlabelled/` with 5 relative
  symlinks that resolve (`find -L … -type l` prints nothing).
- After `switch`: `systemctl --user list-timers | grep tf2demos-organize`;
  `systemctl --user start tf2demos-organize` then
  `journalctl --user -u tf2demos-organize -n 50`; `readlink ~/.config/tf2demos/config.toml`
  points into `/nix/store`.

---

## 7. Deferred — do not build this session

GUI (egui/eframe, `theme.toml`, palette in HANDOFF §2), review wizard, TF2-exit
watcher + mako notification, `play` / clipboard helper, freeze/thaw cold store,
labelled symlink names with tick and rating (needs labels, but the naming
function exists from step 4 so the tree regenerates correctly later). When these
come, HANDOFF §3–§5 still describe them, with the §2 corrections above.

---

## 8. Pointers

- `HANDOFF.md` §2 file formats, §4 index/config schema, Appendix A verbatim user answers.
- `/etc/nixos/home/modules/terminal/ai/claude_code.nix` lines 221–250: user timer pattern.
- `/etc/nixos/home/modules/wm/niri.nix`: `rustPlatform` usage in this config.
- `/etc/nixos/home/modules/default.nix`: HM module import list.
- `~/.dots/sync-nixos.sh`: mirrors `/etc/nixos` to GitHub after changes.
- `~/.claude/skills/run-tf2/`: TF2 launch driver (later sessions only).
- `/etc/nixos/home/themes/discordo/cyberpunk.toml`: palette for the GUI session.
