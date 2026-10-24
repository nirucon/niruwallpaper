# NIRU Wallpaper

Graphical wallpaper settings for the NIRUX X11 desktop.

NIRUWM applies the wallpaper and rotates it. This program is a front end for
that: it reads `niructl wallpaper status` and `niructl wallpaper list`, and
every change it makes goes back out through `niructl`. It owns no settings of
its own, runs no wallpaper daemon, and never reads the theme directory with its
own parser, so it cannot disagree with the window manager about what is shown.

## What it does

- **Follow the theme** — use the selected NIRUX theme's own wallpaper, so a
  theme change also changes the wallpaper.
- **Choose a theme wallpaper** — keep the current theme but show a different
  image from the installed set.
- **Choose your own image** — any image in `~/Pictures/Wallpapers` (or the
  directory configured in `niruwm/config.toml`).

It also sets the scale (`fill`, `fit`, `stretch`, `center`) and the rotation
interval, including off.

These are separate workflows on purpose. Picking an image stops theme following
in the same step, and the program refuses to apply an image and "follow the
theme" together rather than silently letting one win. Choosing a wallpaper never
changes the desktop theme.

The window follows the active theme live: the palette comes from the same theme
file NIRUWM and NIRUBAR read, including `appearance.toml` overrides.

## Previews

Thumbnails are cached under `$XDG_CACHE_HOME/niruwallpaper/thumbs`, keyed on each
image's path, size and modification time. An 8K file is decoded once and a
replaced file never shows a stale preview. A missing or unreadable image shows a
placeholder instead of failing.

## Usage

```sh
niruwallpaper              # open the window
niruwallpaper status       # the settings the window would show
niruwallpaper list         # available wallpapers, one per line
niruwallpaper set PATH     # apply one image
niruwallpaper --version | --help
```

In the window: `Tab` switches list, arrows select, `Enter` applies, `Esc` or `q`
closes. Clicking works for every control.

## Building

Rust 1.75 or newer.

```sh
cargo build --release
cargo test
sh tests/cli-smoke.sh target/release/niruwallpaper
```

The dependencies are the ones the suite already uses: `x11rb` for the X
connection, `image` for PNG and JPEG previews, and `ab_glyph` for text. There is
no GTK or Qt runtime requirement.

## Installing

NIRUX installs this program as part of the suite through its manifest. A release
also carries the desktop entry and icon.

## Files

| Path | Purpose |
|---|---|
| `~/.config/niruwm/config.toml` | The settings, written only by `niructl` |
| `$XDG_CACHE_HOME/niruwallpaper/thumbs` | Regenerable preview cache |

The program never modifies or deletes an image.

## Development

Read [AGENTS.md](AGENTS.md) first. The original implementation brief is in
[IMPLEMENTATION-PROMPT.md](IMPLEMENTATION-PROMPT.md); where the brief and the
shipped backend disagree, the backend's behaviour wins, because it is what the
desktop actually does.
