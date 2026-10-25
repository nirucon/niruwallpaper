# Handoff: screens, bars, search and layouts

Written 2026-10-25 by the implementing agent. Revalidate every version and
observation before relying on it.

## What was asked, and what exists now

| Request | Where it lives |
|---|---|
| Workspaces, clock and status on **every** screen | NIRUBAR runs one bar per screen; each bar has its own workspace cells and its own clock. The `display` module now says which bar is which (`2 below`, `2 above`, `2 left`, `2 right`). |
| Place one screen **above** the other, or side by side, per machine | `niructl display rotate OUTPUT normal\|left\|right\|inverted`, `niructl display auto`, `niructl display save\|apply\|preview`, and the shorter `nirux layout rotate\|auto\|save\|apply\|list`. The layout is the screens' positions in `displays.toml`, so moving the pointer straight up or down reaches the other screen whichever way they are arranged. |
| Easy to configure from config or a bar module | One `nirux layout` command, or the launcher's **Displays** group: every rotation of every screen, "Arrange side by side", and "Save this layout as a profile" (the launcher asks for the name). |
| Different tiling on every screen **and** every workspace | A layout belongs to one workspace on one screen. `Super+T/Y/M/C/U/F` changes only the focused workspace; `niructl layout show\|save` reads and writes them; the manager saves each change to `[monitors] layouts` (or a screen's own `[[monitors.profiles]] layouts`) so it survives a logout and a crash. Every workspace starts tiled. |
| A really good web search, easy to extend | `nirulauncher --search`, `Super+S`. Ten engines built in; a new one is a `[[search.sources]]` entry (`id`, `label`, `url` with `{query}`). |
| Theme wallpapers | `nirux wallpapers status\|download\|remove`; every machine now has all ten sets. |
| Keep the suite consistent | Every component released from a tag, NIRUX 0.12.33 pins them all, CI green, and the installer updated on six machines. |

## Versions released in this round

| Component | Version |
|---|---|
| NIRUWM | 0.16.96 |
| NIRUBAR | 0.18.41 |
| NIRULAUNCHER | 0.2.37 |
| NIRUX | 0.12.33 |

NIRUWALLPAPER 0.1.1 and the remaining programs are unchanged from the
previous round.

## Verified on every machine

Machines: Void bare metal (`void`), Debian bare metal (`ingwaz-deb`), othala
(two 2560x1440 screens), Arch VM (192.168.122.8), Void VM (192.168.122.31) and
Debian VM (192.168.122.148). All six run NIRUX 0.12.33 with the versions
above, and the same checks pass on each:

- A real NIRUWM session starts and owns the display, with the bar mapped.
- `niructl monitors`, `niructl wallpaper status-json` and
  `niruwallpaper status` report.
- Ten search engines are listed and the search window opens.
- The launcher offers a rotation row for every screen and the arrangement row.
- `nirux layout list` works.
- Workspace 2 switched to monocle while workspace 1 stayed tiled, and the
  config recorded `layouts = ["tile", "monocle", …]`.
- `niructl layout save` is acknowledged by the manager.

On othala's own two-screen session the released `niructl` turned the upper
screen (`DisplayPort-5`) upright and the result was saved as the profile
**Two screens**; **Before** keeps the previous arrangement.

## Known limits

- Unplugging and replugging a screen while running was not done by hand; the
  code path is the RandR reconnect path the offline suite covers.
- The bar's `display` module says `2 below` or `2 above` only when the bar is
  started with its screen's RandR name, which NIRUWM passes. A bar started by
  hand without `--monitor` shows the plain count.
- `niructl layout NAME` needs a running manager, because it is a command.
  Editing `[monitors] layouts` by hand works before the next login.
- The two-screen pixel checks were done through RandR emulation. The real
  physical arrangement on othala was applied and is visible on the machine,
  but no screenshot of both screens was taken.

## Cleanup done

All test trees, screenshots, logs and temporary images were removed from the
six machines, and the temporary SSH key was taken out of every
`authorized_keys`.

## Where to look

- `niruwm/src/wm/mod.rs` — `rewrite_layouts`, `remember_layouts`, and the
  layout-file tests.
- `niruwm/src/monitor.rs` — per-workspace layouts per screen.
- `niruwm/src/wm/clients.rs` — `work_area`, `monitor_area`, `arrange`.
- `nirubar/src/status.rs` — `display_text` with the screen's place.
- `nirulauncher/src/websearch.rs` — the engine table and `[[search.sources]]`.
- `nirux/nirux` — the `layout` and `wallpapers` commands, and the SDDM rule.
