# NIRU Wallpaper

**Part of [NIRUX - A minimal X11 desktop by Nicklas Rudolfsson](https://github.com/nirucon/nirux).**

**Early beta.** A small X11 settings window for the NIRUX wallpaper with picture
previews, and a choice per screen. It owns no settings of its own.

NIRUWM applies the wallpaper and rotates it. This program is a front end for
that: it reads what `niructl` reports and every change it makes goes back out
through `niructl`. There is no wallpaper daemon, no second configuration file
and no parser of the theme directory here, so the window cannot disagree with
the window manager about what is shown.

## What it does

- **Follow the theme** — use the selected NIRUX theme's own wallpaper, so a
  theme change also changes the wallpaper.
- **Choose a theme wallpaper** — keep the current theme but show a different
  picture from the installed set.
- **Choose your own picture** — any picture in `~/Pictures/Wallpapers` (or the
  directory configured in `niruwm/config.toml`).
- **Set one screen alone** — with more than one screen, the header lists
  *All screens* and each connected screen. A screen that is set alone keeps its
  picture while every other screen follows the shared settings and the theme.

It also sets the scale (`fill`, `fit`, `stretch`, `center`) and the rotation
interval, including off.

## How the screens behave

These are the rules the window shows and NIRUWM enforces:

- The default is the **same picture on every screen**, scaled and cropped for
  each screen's own size and position. One picture is never stretched across
  the whole desktop.
- A screen may have **its own picture** instead. Choosing a picture for one
  screen stores an entry for that screen only; the other screens are not
  touched.
- A screen with its own picture **survives** a theme change, a reload, a
  rotation tick and a reconnect. Only its own entry changes it, or the reset
  button in the window.
- New screens inherit the shared settings. A screen that is unplugged keeps its
  entry, so the same cable in the same port brings its picture back.
- Screens are identified by their **RandR output name** (`eDP-1`, `HDMI-1`).
  A cable moved to another port is therefore a different screen and starts
  from the shared settings; the old entry stays for the original port.
- Two mirrored outputs share one display area and show the same picture.
- A theme without a picture paints that theme's own background colour on the
  screens that follow it. Screens with their own picture keep it.
- A single picture (`mode = "single"`) keeps one image, so rotation is
  explained as inactive instead of silently doing nothing.

The window refreshes itself about once a second and follows the active theme
live, including `appearance.toml` overrides. A staged, unapplied change is
never replaced by a refresh; the footer says when the desktop changed
meanwhile.

## Previews

Previews are made by a small number of background workers, visible tiles
first, so a folder of 8K pictures never blocks the window. Each preview is kept
in memory and in `$XDG_CACHE_HOME/niruwallpaper/thumbs-v2`, keyed on the file's
path, size, modification time and a sample of its content, so a replaced file
never shows a stale picture. PNG, JPEG, WebP and BMP are previewed; anything
else, and a corrupt or missing file, shows a placeholder.

## Usage

```sh
niruwallpaper              # open the window
niruwallpaper status       # the settings and each screen's state
niruwallpaper list         # available wallpapers, one per line
niruwallpaper set PATH     # apply one picture to the shared settings
niruwallpaper --version | --help
```

In the window:

| Key | Action |
|---|---|
| `Tab` / `Shift+Tab` | move between every control; the focus ring shows where you are |
| arrows | choose a picture in the grid |
| `Space` | choose the picture, or press the focused control |
| `Enter` | apply, or press the focused control |
| `Ctrl+D` | discard the staged change |
| `A` | return to the picture grid |
| `F1` | help |
| `Esc` or `q` | close |

The status line names the screen and picture each connected screen shows, and
says when no running window manager confirmed the result. Clicking works for
every control.

## Building

Rust 1.85 or newer.

```sh
cargo build --release --locked
cargo test --locked
sh tests/cli-smoke.sh target/release/niruwallpaper
```

The dependencies are the ones the suite already uses: `x11rb` for the X
connection and RandR, `image` for PNG, JPEG, WebP and BMP previews, and
`ab_glyph` for text. There is no GTK or Qt runtime requirement.

## Installing

NIRUX installs this program as part of the suite through its manifest. A release
also carries the desktop entry and icon, which NIRUX installs unchanged.

## Files

| Path | Purpose |
|---|---|
| `~/.config/niruwm/config.toml` | Every setting, written only by `niructl` |
| `$XDG_CACHE_HOME/niruwallpaper/thumbs-v2` | Regenerable preview cache |

The program never modifies or deletes an image.

## Troubleshooting

- **`the niructl command was not found`** — NIRUX is not installed, or the
  session is not a NIRUX session. The window and `status` need NIRUX.
- **`NIRUWM does not own the wallpaper`** — `[wallpaper] enabled = false` in
  `~/.config/niruwm/config.toml`. Enable it; NIRUX does this on install.
- **A screen shows its theme colour** — that theme has no picture installed.
  `nirux wallpapers download` installs the optional set.
- **Rotation does nothing** — a single picture keeps one image. Choose the
  theme or another picture and set an interval.

## Development

Read [AGENTS.md](AGENTS.md) first. The original implementation brief is in
[IMPLEMENTATION-PROMPT.md](IMPLEMENTATION-PROMPT.md). The current followup
assignment is [FOLLOWUP-PROMPT.md](FOLLOWUP-PROMPT.md); neither document starts
implementation without an explicit owner instruction. Where a brief and the
shipped backend disagree, the backend's behaviour wins, because it is what the
desktop actually does.
