# Handoff: wallpaper, SDDM and suite consistency

Written 2026-10-25 by the implementing agent. Revalidate
every version and observation before relying on it.

## Status

WALL-001, WALL-002 and DOCS-001 are implemented, released and installed on
every test machine. The owner still has to run the physical two-screen test.

## Releases

| Component | Version | Reason |
|---|---|---|
| NIRUWM | 0.16.88 | Per-screen wallpaper, rendered state, transactional apply, `niructl monitors` fix |
| NIRUBAR | 0.18.39 | Accepts `[[wallpaper.outputs]]`; ten theme palettes documented |
| NIRUWALLPAPER | 0.1.1 | Per-screen window, live theme, background previews, LICENSE, README |
| NIRULAUNCHER | 0.2.33 | One wallpaper entry that opens NIRU Wallpaper; themes stay |
| NIRUSHOT | 0.4.4 (docs) | Cargo.toml pin for image 0.25.8; no new tag needed |
| NIRUX | 0.12.18 | Pins the above, SDDM session handling, byte-exact icon |

Each release came from a tag, CI and the release workflow passed, and the
published `SHA256SUMS` were verified after download. `niruwallpaper.png` in the
NIRUX release was checked to be a valid PNG.

## What changed in NIRUWM

- Every screen is set at its own geometry; one picture is never stretched
  across the desktop.
- `[[wallpaper.outputs]]` gives one screen its own picture (mode `single` or
  `theme`, plus an optional scale). Entries survive theme changes, reloads,
  rotation and a reconnect; new screens inherit the common settings.
- A reload re-applies what each screen showed, instead of resetting a screen
  with its own picture.
- `niructl wallpaper apply` changes several settings as one validated
  transaction and waits for the window manager's acknowledgement.
  `status-json` reports the plan and the rendered state, and
  `output list|set|reset` covers per-screen settings.
- The WM publishes `_NIRUWM_WALLPAPER_STATE`.
- `niructl monitors` is routed before any X connection and succeeds without a
  display.

## What changed in NIRUWALLPAPER

- Structured report, screen choice (All screens plus one per screen), live
  theme and appearance, staged edits kept through a refresh.
- One apply is one transaction; failures are reported with the WM's words.
- Previews on two background workers, visible tiles first, PNG/JPEG/WebP/BMP,
  cache keyed on path, size, modification time and content edges.
- Focus ring, help view, per-setting keyboard access, resized window.
- Minimum Rust 1.85 to match the locked `image` crate; MIT LICENSE added.

## What changed in NIRUX

- SDDM: on install the remembered session is cleared so SDDM offers every
  installed session, NIRUWM first. Autologin, remembered user, theme and all
  other settings are untouched; the previous file is kept as
  `state.conf.nirux-backup`. A file that cannot be read is left alone.
- The launcher icon is installed byte for byte; ownership is recorded in a
  sidecar named by its SHA-256, so a replaced icon is never removed.
- The staged child receives the bootstrap's one sudo password, and works
  without a terminal.
- `keep` (an existing display manager) now maintains SDDM's state too.
- README documents the per-screen behaviour and the SDDM rule.

## Verification on the machines

Machines: Arch VM (192.168.122.8), Debian VM (192.168.122.148), Void VM
(192.168.122.31), Void bare metal (`void`), Debian bare metal (`ingwaz-deb`)
and othala.

Every machine, on NIRUX 0.12.18:

- Session starts under Xvfb, NIRUWM 0.16.88 owns the display.
- NIRUBAR is a dock with its 28 px strut.
- `niructl monitors`, `niructl wallpaper status-json`, `niruwallpaper status`
  and the launcher window all work.
- Every component reports its installed version.

Two-screen acceptance (real window manager, real `niructl`, real `feh`, two
emulated RandR screens at 1600x900 and 1080x1920):

- Each screen set at its own geometry, with `--no-fehbg`.
- One screen keeps its own picture while the other follows the theme.
- A theme change reaches only the inheriting screen.
- An invalid or malformed configuration never replaces the working file.
- A hotplugged screen inherits; a reconnect restores its own picture.
- A theme without a picture paints the theme's colour on that screen only.
- The window stores a per-screen picture when a screen is chosen and clicked.

Run on Arch, Debian, Void VMs, `void` and `ingwaz-deb`. The live repaint check
needs `import`; on the Void VM, which has no ImageMagick, it was NOT RUN.

## Limitations and open items

- Physical two-screen testing on othala (landscape below, portrait-inverted
  above) is the owner's next step; nothing is claimed for it here.
- Rotation of the upside-down screen is still done by a NIRUPI script; NIRUX
  has display profiles (`niructl display`) but no per-screen rotation command.
- Installation over plain SSH without a terminal still cannot prompt for
  sudo; a terminal (or `script -qec`) is the supported path. This was left as
  documented behaviour rather than adding another privileged path.
- `nirux update` over SSH without a terminal has the same limitation.
- The launcher's `--actions` opens a window, so it cannot be checked
  headlessly; the check runs it with a display and a timeout.

## Cleanup done

The temporary sudo rule, the test SSH key, the cloned installers, the staged
NIRUX trees and the test directories were removed from every machine after the
final checks. SDDM on othala was restored to its original configuration, with
the remembered session set to NIRUWM and the theme unchanged.
