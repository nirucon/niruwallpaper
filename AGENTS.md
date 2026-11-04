# Agent assignment: NIRUWALLPAPER 0.2.0, a one-shot bug, efficiency and UX update

## 0. Quick start

1. Read this file, `README.md`, `src/window.rs`, `src/backend.rs`, `src/thumbs.rs` and
   `src/screens.rs`. The audit baseline is `fb5977c` (0.1.2, pinned by NIRUX 0.12.199).
   Revalidate each item against current HEAD and skip items that are already fixed,
   noting that you did.
2. Implement **W1–W7, then U1–U4 in order**. Commit per item. Keep fmt, clippy
   `-D warnings`, `cargo test --locked` (26 tests at baseline) and
   `tests/cli-smoke.sh` green.
3. Pass the gates in §4, release in §5 and report in §6.

The sister apps NIRUDISPLAY and NIRUTHEMESELECTOR have their own assignments
(`AGENTS.md` in those repos) with the same patterns: keyboard levels, a synchronous
`niructl`, polling and size clamping. Where a fix there already exists, port it here
instead of inventing a second version.

This is an **execution** assignment. Do not stop after a plan or a branch. Make sensible
decisions, document them, and continue without routine questions.

## 1. Invariants

- NIRUWALLPAPER owns no settings. Every read and change goes through `niructl`
  (`wallpaper list`, `status-json` and the apply commands). NIRUWM applies and rotates.
- Changes are **staged** and only Apply changes the desktop. Discard returns to the
  current state.
- Keep the per-screen rules in the README (shared default, per-output entries that
  survive, mirrored outputs, the theme color fallback).
- Keep `rust-version = 1.85` and `image` pinned below 0.25.10 (see the Cargo.toml
  comment).

## 2. Bugs (verified 2026-11-04 on niruxwork, in a separate Xvfb)

### W1: The mouse wheel stages a wallpaper (high)
With the pointer over the grid, 15 wheel-down notches did not scroll. They **staged
"Erik Satie Red" and switched "Follow theme" to off**: the status line said "Staged Erik
Satie Red" and Apply/Discard became active. Buttons 4 and 5 (wheel) are handled as
clicks.
**Fix:** buttons 4 and 5 scroll the grid by one row (Shift = page), buttons 6 and 7 are
ignored, and only button 1 selects. Add a test.

### W2: The grid is painted over the status line and key hints
At the default window size, the 4th and 5th rows of tiles are drawn on top of the
status text and hint line ("Tab move · arrows select …"). **Fix:** clip the grid to its
own rectangle, and compute `visible_rows` from the space actually left above the status
line and footer.

### W3: Previews for partly visible rows never load
Tiles in the partly visible rows (Forest Vigil, Hackerman Oldschool/Retro, Niru Carl
Larsson, …) kept showing **"Loading preview"** for more than 25 s. **Fix:** request
thumbnails for every row that intersects the viewport, plus one row of look-ahead. Show
a real error ("cannot read image") instead of loading forever. Use LRU eviction, as in
the NIRUTHEMESELECTOR assignment.

### W4: The open window polls `niructl` twice a second
`REFRESH = 1000 ms`, and a 20 s idle trace showed **40 spawns**: `niructl wallpaper list`
and `niructl wallpaper status-json` every second.
**Fix:**
- Watch with inotify the files that change the answer: the niruwm config, the
  wallpaper directories and the theme wallpaper dir (from niructl's source). Debounce by
  300 ms, then run one refresh.
- Also refresh on `FocusIn` and RandR screen changes.
- With nothing pending, `poll()` blocks without a timeout.

**Done when:** an idle open window starts 0 processes in 60 s, and a change made with
`niructl` in a terminal shows up within 1 s.

### W5: Apply blocks the UI
Apply runs `niructl` synchronously. Since NIRUWM 0.16.166, wallpapers are composed per
output with ffmpeg, which can take seconds on 4K or 8K images and multiple monitors, and
the window freezes during that time. **Fix:** run Apply on a worker with "Applying…",
disable the buttons, use a deadline (30 s, non-blocking reads, kill and reap), and ignore
late results.

### W6: Keys have only level 0
`Keys` keeps only the first keysym. That is fine for the current shortcuts, but use the
shared level/Caps helpers anyway (from nirulauncher's `keyboard_level`/
`keyboard_symbol`), so that a future filter or path field works with AltGr. Refresh the
map on `MappingNotify`.

### W7: Size clamping under tiling
`ConfigureNotify` clamps to `MIN_WIDTH` 660 even when the real window is smaller. Paint
for the real size, with a compact layout that puts the settings panel below the grid.

## 3. UX

### U1: A scrollbar and position
Show "Wallpapers 1–16 of 24" and a thin scrollbar that can be dragged. Add
PageUp/PageDown/Home/End.

### U2: A larger preview of the staged choice
The settings panel has a lot of empty space. Show a large preview of the staged
wallpaper rendered with the selected scale (fill/fit/stretch/center) on the selected
screen's aspect ratio, so that the scale buttons show their effect before Apply.

### U3: Add your own pictures
"My wallpapers" gets **Add pictures…**: a simple in-app list of `~/Pictures` and
`~/Downloads` images that copies the selected files into the configured wallpaper
directory, never moving them, with a name collision suffix. It also gets "Open folder"
through `xdg-open`.

### U4: Clearer screen choice
When there is more than one screen, show small proportional screen rectangles in the
header (like NIRUDISPLAY) instead of text-only targets, and mark which screens have their
own picture.

Out of scope: a wallpaper daemon, animated wallpapers, online galleries, and changes to
NIRUWM's composition.

## 4. Gates

Automated:
- fmt, clippy, tests, `tests/cli-smoke.sh`, and an MSRV job on 1.85 (if it is missing).
- New tests: W1 wheel handling (pure event → action), W2 layout clipping at the minimum
  and default sizes, the W3 request set for partly visible rows, the W4 debounce logic,
  and the U2 scale preview geometry.
- An Xvfb test, ignored by default and run in CI: point `NIRUWALLPAPER_NIRUCTL`, or the
  existing override, at a fixture `niructl` that serves 30 wallpapers and records argv.
  Check the wheel scroll, that no staging happens on the wheel, the idle spawn count
  (0), and that a slow Apply (5 s) does not block repainting.

Native, on **niruxwork** (the owner allows local testing):
- Record the owner's current wallpaper state (`niructl wallpaper status-json`) first,
  and restore it exactly afterwards.
- Test wheel, Apply and Discard, the own-pictures import (with a temporary test image
  that you remove afterwards) and the idle trace.
- On **othala** (authorized, 2 monitors): per-screen selection and U4.

## 5. Release

Bump to **0.2.0** and update the README. Tag `v0.2.0` through the normal workflow and
verify the assets and checksums. Then pin it in `nirucon/nirux` `manifest`:
- If `nirux/tools/suite-pin` exists, use it.
- Otherwise bump the manifest the usual way. Change only the niruwallpaper line, and
  rebase if other pins land at the same time.

Install through `nirux update` and verify the running binary.

## 6. Final report

For each item W1–W7 and U1–U4: the change, the tests and the evidence (idle spawns
before and after, screenshots of the layout fix). Include release URLs, installed
versions and remaining limits. Mark anything unfinished as **PARTIAL** and give the exact
remaining steps.
