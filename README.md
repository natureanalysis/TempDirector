[中文](README_ZH.md) | **English**

# TempDirector

Redirect the cache / temp directories of hundreds of applications to a RAM disk with
**directory junctions**, and let a boot-time guardian rebuild that directory tree on the
RAM disk after every logon — cutting SSD writes while keeping hot cache I/O in memory.

**Core trait: zero UAC prompts in the normal case** (directory junctions don't need
`SeCreateSymbolicLinkPrivilege`); elevation only kicks in on demand when a source path
lives under `Program Files` / `ProgramData` or another protected location.

---

## Status

| Milestone | Scope | Status |
|---|---|---|
| **M1** | Link engine core + safe moving + rule store + boot rebuild + CLI | ✅ Done, verified end-to-end on real hardware |
| **M2** | Tauri 2 desktop UI (list / search / filter / multi-select / dialogs / detail panel / tray / progress / system notifications) | ✅ Done, verified with a real launch |
| **M3** | Autostart at logon (scheduled task) + on-demand elevation worker + persisted result notices | ✅ Done and verified on real hardware (the task really runs) |
| **M3.5** | Ops hardening: RAM-disk water-level monitoring + per-rule pre-authorized auto-clean + scheduled off-peak cleaning + savings stats + drag-and-drop rule ordering + exclusion patterns + one-click restore | ✅ Done |
| M4 | Windows service (logon events + user token + patrol self-healing) + named-pipe IPC | Planned |
| M5 | Built-in preset rule library (templates for ~100 apps) + NSIS installer | Planned |

M1 produced `td-core` (the engine library) and `tempdirector` (the CLI);
M2 produced `td-gui` (`tempdirector-gui.exe`, the UI);
M3 wired up "rebuild at logon" and made protected-path operations elevate on demand.
All three binaries share the same `td-core`; the UI layer contains no link logic.

---

## Desktop UI

The UI speaks **both Chinese and English**: switch with the dropdown at the top right.
The first launch follows your system language (English by default on non-Chinese systems),
and the choice is remembered.

**Main window**:

![Main window](docs/screenshots/main-window-en.png)

* **List**: status badges (✅ OK / ⚠️ broken link or empty target / ❌ not linked / 🚫 foreign reparse point),
  the reason needing attention right under the name, source and target paths side by side,
  plus indicator columns for "wanted enabled" and "rebuild on boot".
* **Search**: search box (name / source path / target subpath), category filter,
  "enabled only", "needs attention only", "select all current results".
* **Batch**: after checking boxes, one click each for Apply / Restore / Enable / Disable /
  Check locks / Relink only / Delete.
* **Detail panel**: select a row to expand full info (source template and expanded path,
  link type, warnings, last applied time and size), with per-rule action buttons.
* **Add / edit rule**: live validation (path expansion result, missing environment variables,
  protected location, whether the target already has content), and the source directory
  can be picked with the system folder dialog.
* **Progress overlay**: a batch apply shows per-rule progress (from the backend
  `td-progress` event), so you never get "I clicked and nothing happened".
* **Activity log**: event timeline + operation history tabs, including failure reasons.
* **Tray**: show main window / run boot rebuild now / quit (closing the window quits for
  now; a resident form is M3).

**Add rule**:

![Add rule](docs/screenshots/add-rule-en.png)

**Settings** (autostart / scheduled cleaning / target roots):

![Settings](docs/screenshots/settings-en.png)

**Batch apply progress overlay**:

![Applying rules](docs/screenshots/progress-en.png)

### Build and run

```powershell
scripts\cargo.cmd build -p td-gui            # debug build (with console, handy for logs)
scripts\cargo.cmd build -p td-gui --release  # release build (no console window)
# output: target\release\tempdirector-gui.exe
```

The interface is **static files** (`crates/td-gui/ui/`, embedded into the exe at compile
time) — **no npm, no bundler**. To look at the UI alone (without starting Tauri), open
`crates/td-gui/ui/index.html?mock=1` in a browser; it mounts an in-memory fake dataset.
Add `&progress=1` for the progress overlay, `&modal=1` / `&settings=1` / `&ops=1` for the
various dialogs.

---

## Autostart and elevation

### Rebuild automatically at logon

```powershell
tempdirector autostart status     # check whether the task is registered
tempdirector autostart enable     # register (per-user task, no admin rights needed)
tempdirector autostart run        # trigger one run right now, to verify the setup
tempdirector autostart disable
```

The same switches live in the in-app "Settings".

* What gets registered is a **per-user logon task** (`InteractiveToken` + `LeastPrivilege`
  + `Priority 4`); registering, querying and deleting need no elevation (verified).
* The task action is `<GUI path> boot-rebuild`, and the GUI is
  `windows_subsystem = "windows"` in release builds, so **no console window flashes at
  logon**.
* In portable mode (data dir pointed to by `TEMPDIRECTOR_DATA_DIR`), registration also
  writes `--data-dir "<dir>"` into the task — scheduled tasks don't inherit your shell's
  environment variables; without it you'd get "registered with database A, runs database B
  at logon".
* **Failures are never silent**: the rebuild report is stored (`settings.last_boot_report`)
  and written to `logs/boot-rebuild.log`; the UI shows a banner at the top next time it opens.

### When administrator rights are needed

Directory junctions themselves need no privileges, but writing caches under
`C:\Program Files` / `C:\ProgramData` / `C:\Windows` needs admin rights. Such rules carry a
"needs admin" mark in the UI, and a batch apply asks whether you want to elevate.

Elevation goes through a **one-shot worker**: write a job file containing only rule ids →
`ShellExecuteExW(runas)` to relaunch itself → the worker re-runs every safety check →
writes back the result file → the source process reads it. Three constraints:

1. **The job contains no paths, only rule ids** — even a forged job can only trigger
   actions that are already legal in the rule store.
2. **The job must live inside `<data dir>\jobs\`**, otherwise it's rejected outright
   (verified: placed anywhere else → exit code 1).
3. The worker re-runs the exact same validation path as the GUI/CLI (blacklist,
   self-loop, occupancy detection), with no exemptions.

The CLI can also drive it directly:

```powershell
tempdirector elevated <job.json>     # normally not needed manually; the GUI launches it itself
```

---

## Behaviors verified on real hardware

Everything below was run against a real filesystem on this machine (not unit-test mocks):

| Scenario | Result |
|---|---|
| Same-volume redirect | goes through `rename`: 3 files / 2 MB, 71ms, all metadata preserved |
| Cross-volume redirect | copy → verify → delete source: 2 files / 1 MB, 25ms |
| Independent link verification | `cmd dir /AL` shows `<JUNCTION>`; `fsutil reparsepoint query` shows tag `0xa0000003` (Mount Point) |
| Read/write through the link | reading `a.txt` / a 2 MB binary / files in subdirectories all work; writes land on the RAM disk side |
| **Simulated RAM-disk reboot** | links show `⚠️ broken`; `boot-rebuild` recreates the directories; links become usable again (empty target, as expected); writes succeed again |
| Restore (RAM disk has content) | remove link → content moved back to the source directory → target directory gone |
| Restore (RAM disk empty) | remove link → recreate the empty original directory; `fsutil` confirms it's no longer a reparse point |
| Rule store export / import | JSON round-trips correctly, Chinese rule names and paths lossless |
| Audit trail | `ops` has the full operation history, `events` has the event timeline |
| **GUI real launch** | window opens normally, embedded frontend loads, styles apply (CSP doesn't block), IPC works (shows real ✅/⚠️/❌ states from the real database), tray in place |
| **Logon task really runs** | after `autostart enable`, `schtasks /Query /XML` independently verifies the command and arguments; `autostart run` triggers a run that **really creates the target directories**, leaves no GUI window behind, and writes the log |
| **Elevation worker (positive)** | a hand-written job with only rule ids → exit code 0, result file written, source directory becomes a junction (`fsutil` verifies `0xa0000003`), readable through the link |
| **Elevation worker (negative)** | placing the job outside `jobs\` → exit code 1, result file states the refusal reason, disk untouched |

---

## Quick start

```powershell
# 1) Build (the script fills in the MSVC + Rust toolchain environment itself)
scripts\cargo.cmd build --release
# output: target\release\tempdirector.exe

# 2) Register the RAM disk as a target root
tempdirector roots add RAMDisk "R:"

# 3) Add a rule (source paths support %VAR% templates, expanded per user on import)
tempdirector rules add "Chrome cache" `
  "%LOCALAPPDATA%\Google\Chrome\User Data\Default\Cache" `
  "browsers\chrome-cache" --category browser --enable

# 4) Inspect first: health check + occupancy + dry run
tempdirector status --all
tempdirector locks "<rule id of the Chrome cache rule>"
tempdirector apply --all --dry-run

# 5) Run for real (checks occupancy first; aborts without touching anything when occupied)
tempdirector apply --all

# 6) Boot rebuild (for the production wiring, see "Wiring up the boot guardian")
tempdirector boot-rebuild

# 7) Undo by restoring
tempdirector restore --all
```

> The data directory defaults to `%PROGRAMDATA%\TempDirector` (shared by the service and the GUI).
> Override it with the `TEMPDIRECTOR_DATA_DIR` environment variable or the `--data-dir`
> argument (handy for portable / test setups).

---

## Command reference

```
overview
  doctor                     environment self-check: data dir, link capability, target volumes, rule states, pending fixes
  version

target roots (RAM disk)
  roots list
  roots add <name> <path> [--timeout ms] [--hdd]
  roots rm <id>
  volume <R: or R:\Dir> [--timeout ms]        probe / wait for the volume to be ready

rules
  rules list [--enabled] [--category cat] [--search term] [--json]
  rules add <name> <source template> <target subpath> [--root id] [--category cat]
            [--link-type junction|symlink_dir] [--enable] [--no-boot] [--notes note]
  rules rm <id...>
  rules enable|disable <id...|--all>

inspection and operations
  status [--enabled|--all] [--category cat] [--search term] [--no-persist] [--json]
  locks <rule id|directory path> [--json]     check occupancy and name the process holding it
  apply <id...|--all> [--dry-run] [--merge] [--sha256] [--skip-lock-check]
  restore <id...|--all> [--dry-run] [--sha256]
  relink <id>                                 rebuild the link only (data untouched)

boot rebuild
  boot-rebuild [--json]
  autostart status                           whether the "rebuild at logon" task is registered
  autostart enable [--exe <path>]            register (per-user, no admin rights needed)
  autostart disable
  autostart run                              trigger one run manually, to verify the setup

elevation worker (normally not needed manually)
  elevated <job.json>                        run a job as administrator (rule ids only, no paths)

config migration
  export <file.json>
  import <file.json> [--dry-run] [--ram-root R:]

audit
  events [--limit N]
  ops [--limit N]                            operation history (including pending fixes)
```

Category values: `browser` / `design` / `dev` / `system` / `media` / `office` / `game` / `other`

---

## Three safety rules that never bend

1. **Never delete source-directory content before the data check passes.**
   The cross-volume order is fixed: enumerate → compute space → copy → verify
   (file count + total bytes, optional SHA-256) → only then delete the source.

2. **Deleting a link may only remove the link entry itself; never recurse into a reparse point.**
   There is no code path that calls `remove_dir_all` on a user path;
   `delete_tree` judges each entry's type and, on a reparse point, deletes only the link.
   Breaking this rule would delete real data on the RAM disk.

3. **Anything you don't understand, don't touch.**
   Foreign reparse points (OneDrive placeholders, `AppExecLink`, volume mount points) are
   reported, never processed; after a software update overwrites a link with a real
   directory, the boot guardian **won't** move anything on its own — it only raises a
   warning for a human.

Details (including 20+ edge cases) are in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) (Chinese).

---

## Wiring up the boot guardian

After M3, the recommended setup is one line:

```powershell
tempdirector autostart enable     # register the per-user logon task; every logon rebuilds automatically
```

Task details, portable-mode handling, and why failures are never silent: see
"Autostart and elevation" above. If you later need it earlier and more controllable
(truly "before all other startup software"), that's the M4 service + logon events +
user token approach.

When troubleshooting:

* `tempdirector autostart status` — is the task there, and what command does it run
* `%PROGRAMDATA%\TempDirector\logs\boot-rebuild.log` — the execution trail of the silent task
* the banner at the top of the UI — whether the last rebuild has anything to watch

---

## Repository layout

```
crates/
  td-core/                 engine library (no GUI dependencies)
    src/util.rs            path normalization, \\?\ long paths, env var expansion, volume detection
    src/safety.rs          rule blacklist (volume roots / system dirs / self-loops)
    src/link/              reparse point identification, Junction create/remove
    src/fsops/             occupancy detection (+Restart Manager), safe moving, merging, verification
    src/engine.rs          orchestration: decision tree → move → link → rollback → persist
    src/store/             SQLite schema / migrations / DAO / operation history
    src/boot/              waiting for volume readiness, batch rebuild at boot, DACL re-apply
    src/autostart.rs       logon task register / query / remove (schtasks + task XML)
    src/elevate.rs         elevation state detection, launching a process as admin and waiting
    src/worker.rs          the elevation worker's job protocol and executor (the trust anchor lives here)
    src/portable.rs        config import / export
  td-cli/                  tempdirector.exe (management / elevation worker / boot rebuild)
  td-gui/                  tempdirector-gui.exe (Tauri 2 UI)
    src/api.rs             business layer: takes &Store, emits serde structs (unit-testable without a window)
    src/commands.rs        thin #[tauri::command] forwarding + progress events + folder dialog
    src/submode.rs         headless submodes: boot-rebuild / elevated (intercepted before Tauri)
    src/lib.rs             entry: submodes → in-memory store fallback → mark_stale_ops → register commands
    src/tray.rs            tray menu (show window / rebuild now / quit)
    ui/                    index.html / style.css / app.js (static files, embedded at compile time)
    presets.json           built-in preset list (cache dirs of common apps, embedded at compile time)
docs/ARCHITECTURE.md       architecture and edge cases (Chinese)
docs/screenshots/          UI screenshots (one Chinese set + one English set: main-window / add-rule / settings / progress)
scripts/cargo.cmd          dev build wrapper (fills in MSVC + Rust environment)
scripts/make_icon.py       app icon generation (pure PIL, re-runnable)
scripts/take-screens.cjs   UI screenshot generation (mock data + headless browser, Chinese and English sets, re-runnable)
rebuild.bat                one-click full rebuild (clean + release) and desktop shortcut refresh
```

## Preset rules (built-in + user extensions)

The **Presets** dropdown at the top of the "Add rule" dialog merges two layers:

1. **Built-in**: `crates/td-gui/presets.json`, embedded into the binary at compile time.
   Inclusion criteria: pure cache/temp data (safe to discard; the app rebuilds it),
   paths stable across machines, existence and size verified on real hardware. Editing
   this file requires a rebuild.
2. **User-external**: `%PROGRAMDATA%\TempDirector\presets.json` (the data directory; in
   portable mode, the directory `TEMPDIRECTOR_DATA_DIR` points to). Format is identical to
   the built-in file, **no rebuild needed**:

```json
[
  {
    "name": "My app cache",
    "category": "dev",
    "source_template": "%LOCALAPPDATA%\\MyApp\\Cache",
    "target_subpath": "dev\\myapp-cache",
    "note": "optional: a hint shown to the user"
  }
]
```

Merge rules: a user entry whose `source_template` matches (case-insensitive) an built-in
entry **overrides** it (that's how you change a preset's default subpath); everything else
is **appended**. The category must be one of
`browser / design / dev / system / media / office / game / other`. File missing = built-in
presets only; a syntax error or an invalid entry skips just that entry, leaving the rest intact.

Both layers only show entries whose source directory **actually exists on this machine** —
software you haven't installed won't appear in the dropdown.

### Overriding a built-in preset (example)

To change the built-in "User TEMP" preset's default target subpath, put an entry with a
**byte-for-byte identical** `source_template` in the external file — it replaces the
built-in entry instead of adding a new one:

```json
[
  {
    "name": "User TEMP",
    "category": "system",
    "source_template": "%LOCALAPPDATA%\\Temp",
    "target_subpath": "system\\temp-on-ram",
    "note": "my own target subpath"
  }
]
```

Where to put it: save the above as `%PROGRAMDATA%\TempDirector\presets.json` (or
`presets.json` in the directory `TEMPDIRECTOR_DATA_DIR` points to, in portable mode) and
reopen the UI — the preset dropdown now shows the new subpath. This chain is verified for
real by the integration test `crates/td-gui/tests/preset_override.rs`
(write external file → point at the data directory → assert the built-in entry was
overridden and there's exactly one entry for that source).

---

## Development

```powershell
scripts\cargo.cmd check --workspace --all-targets
scripts\cargo.cmd test  --workspace          # 104 unit tests (td-core 88 + td-gui 16)
scripts\cargo.cmd build --release            # both exes at once
python scripts\make_icon.py                  # re-run when the icon needs changing
rebuild.bat                                  # full rebuild: clean + release + refresh the desktop shortcut
rebuild.bat noclean                          # skip clean, incremental release build only
```

Retaking the UI screenshots for this README (no Tauri startup; mock data + headless
browser; one Chinese set and one English set):

```powershell
# requires playwright (npm i playwright); uses the Edge / Chrome already installed
node scripts\take-screens.cjs --lang en     # English four: main-window / add-rule / settings / progress
node scripts\take-screens.cjs --lang zh     # Chinese four (overwrites the existing ones)
node scripts\take-screens.cjs --lang en --check   # only scan for missing translations (Han characters / bare keys), no screenshots
```

What `rebuild.bat` does: `cargo clean` → `cargo build --release` → after verifying the
artifacts exist, (re)create the desktop shortcut `%USERPROFILE%\Desktop\TempDirector.lnk`
pointing at the freshly built `target\release\tempdirector-gui.exe` (with the app icon).
The shortcut is refreshed on every rebuild because `cargo clean` wipes `target\` too, and
a .lnk created before the rebuild would dangle. When double-clicked, the window pauses for
a keypress (run it from an existing console and it just exits), so you can read the build
result.

Why `scripts/cargo.cmd` exists: the MSVC toolchain needs `vcvars64.bat` first, and in a
restricted shell `PATH` may be empty while `RUSTUP_HOME` / `CARGO_HOME` may not point where
they should. The script fills the environment in, then forwards to cargo.

(This file must stay pure ASCII — `cmd.exe` reads batch files with an OEM code page, and
Chinese comments make it fail to parse.)

### About the tauri version: the whole family must be locked to one generation

Writing just `tauri = "2.11.5"` is not enough. `tauri-codegen` / `tauri-macros` /
`tauri-utils` / `tauri-runtime` are all **range dependencies** that resolve to newer
versions independently; the result is generated retry code in `tauri-macros` referencing
error variants that don't exist in `tauri` itself, and the build blows up with 27 errors
like `no variant named UnexpectedMenuKind`. The fix is `cargo update -p <crate>
--precise <ver>` to align the lockfile with one verified combination
(`tauri 2.11.5 / tauri-build 2.6.3 / tauri-codegen 2.6.3 / tauri-macros 2.6.3 /
tauri-plugin 2.6.3 / tauri-utils 2.9.3 / tauri-runtime 2.11.3 / tauri-runtime-wry 2.11.4 /
wry 0.55.1`). **When upgrading tauri, move the whole set together — don't touch only the
one version number in the main crate.**
