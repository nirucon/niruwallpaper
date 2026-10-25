# Implementation prompt — NIRU Wallpaper

Historical initial-build brief. The application is now implemented. For the
current stabilization, multi-monitor and suite documentation assignment, use
[FOLLOWUP-PROMPT.md](FOLLOWUP-PROMPT.md). Do not restart the original build or
treat the historical brief as authorization for additional features.

Use the following brief when the owner explicitly starts implementation.
Complete a coherent, tested delivery rather than publishing speculative fixes.
Do not promise completion in one unattended run or substitute weakened tests
for correctness. Record blockers and continue independent work when possible.

## Assignment

Build **niruwallpaper**, a polished graphical wallpaper settings frontend for
the NIRUX X11 desktop. Integrate it into the NIRUX installer and launcher so
fresh installations and upgrades receive a consistent, working application.

Allowed implementation repositories: niruwallpaper, nirux, niruwm and
nirulauncher. Change other components only with separate owner authorization.
Do not implement unrelated roadmap features or change working keybindings.
The private planning repository may receive a sanitized handoff if authorized.

The owner will use the desktop daily. Prioritize safe configuration writes,
clear behavior and small changes over visual novelty or extensive refactoring.
UI, documentation, errors and public release text must be in English.

## 1. Verify the current system before designing the solution

Read applicable AGENTS.md files and contribution instructions. Inspect current
remote source and local work without discarding changes. Record starting SHAs,
branches, component versions and a concise implementation/test plan.

Historical reference only: at brief creation, NIRUX 0.12.10 pinned NIRUWM
0.16.84, NIRUBAR 0.18.37 and NIRULAUNCHER 0.2.31. Revalidate these: this is not
an instruction to downgrade anything or use these versions for new releases.

Inspect at least:

- niruwm/src/wallpaper.rs: theme lookup, fallback, single/fixed/random modes,
  selection, scaling, rotation and ownership.
- niruwm/src/config.rs and src/bin/niructl.rs: parsing, validation, wallpaper
  commands, config persistence and reload acknowledgment.
- nirux installer, manifest, config.example.toml, integration assets, ownership,
  managed generations, update/rollback/uninstall and wallpaper bundle handling.
- nirux/terminal/nirux-theme-hook and current greeter synchronization mechanism.
- nirulauncher: desktop entries, action execution, favorites and window rules.
- Existing theme/appearance readers, reusable UI rendering, image dependencies,
  release workflows and normal-user X11 tests.

Verify actual command syntax by reading source/help. Do not invent flags,
functions, APIs or keyboard shortcuts. List existing capabilities and missing
backend operations before implementing the GUI.

### Architecture choice

Prefer the suite's established Rust/X11 and release conventions when they can
deliver a good thumbnail browser without a new framework. Audit the existing
renderer and image-loading code first. Do not copy a large UI implementation
blindly, add heavyweight dependencies without justification, or force an
Omarchy-specific Qt/Arch-only installation model onto NIRUX. If a different GUI
stack is materially better, document build/runtime/dependency costs across all
three distributions before adopting it. No embedded web browser or Electron.

## 2. One owner, one source of truth

- NIRUWM owns applying the desktop background and rotation. No new wallpaper
  daemon, parallel timer or direct feh/xsetroot calls from the GUI.
- The shared ~/.config/niruwm/config.toml, respecting XDG_CONFIG_HOME, is the
  source of truth. Do not create a second settings store for the same values.
- Use existing niructl wallpaper commands. Add narrowly scoped backend commands
  only where needed; structured status/list output is preferable to parsing
  human-oriented text. Preserve existing CLI compatibility.
- Batch related settings into one validated transaction and one acknowledged
  reload rather than several transient, inconsistent updates.
- Preserve comments, permissions, unknown settings and unrelated sections.
  Validate a candidate before atomic same-directory replacement. Handle
  concurrent manual edits without lost updates. Refuse malformed TOML intact.
  A rejected apply must leave the old working config active; if activation
  fails after persistence, restore safely only when no intervening edit exists.
- External edits and theme changes refresh the GUI. Watch parent directories
  and re-arm after rename/symlink changes. Dirty forms require explicit conflict
  handling, not silent replacement of either version.

## 3. UX and visual design

One compact, resizable window named **NIRU Wallpaper**, application/binary/
desktop-entry identity **niruwallpaper**. Match the active NIRUX theme live:
palette, typography, spacing, focus indication and contrast, including custom
appearance overrides. No hardcoded Noir-only colors. Follow the existing
appearance contract instead of introducing a competing parser or theme store.

Suggested layout (adapt responsively rather than copying rigid dimensions):

```text
NIRU Wallpaper                              [Search…]
[Official themes] [My wallpapers]

[thumbnail] [thumbnail] [thumbnail] [thumbnail]
 Noir ✓      Nord        Satie       …

[Large preview]                  Follow theme       On
                                 Scale             Fill
                                 Selection         Fixed / Random / Single
                                 Rotation          10 min / Off
                                 Image variant     4K / 8K (when supported)

[Add images…] [Download official wallpapers]       [Apply]
Status: active image / pending change / clear actionable error
```

Requirements:

- Show thumbnails with stable labels, active-image indication and a distinct
  pending selection. Browsing never applies or changes a desktop theme.
- Search filenames and official theme names. Useful empty, missing-image,
  unsupported-image and loading states; no silent blank panels.
- A selected preview must explain cropping/letterboxing for the active monitor
  and selected scale. Clearly distinguish a preview from the actual desktop.
- Follow theme and custom image behavior must be unambiguous. A single-image
  override disables effective theme following; returning to Follow theme must
  clear the override using the established backend semantics.
- Support Fill, Fit, Stretch and Center; existing fixed/random/single modes;
  rotation interval including Off; enabled/disabled wallpaper ownership where
  currently supported. Explain fixed selection versus explicitly pinned image.
- Preserve shuffle, file lists and other existing settings even when they are
  not edited in the first version. Never reset them merely by opening the app.
- Do not offer a cosmetic 4K/8K switch that the WM ignores. If resolution choice
  is not supported, implement a minimal consistent backend option with a safe
  documented default, or defer the control and state that limitation clearly.
- Own images must never be modified or deleted. Use a file/folder picker that
  works on the supported desktop without requiring unavailable portals.
  Adding a folder must not imply scanning the entire home or Nextcloud tree.
- Start with a centered floating window through existing NIRUWM rules, clamped
  to usable screen bounds. It must remain usable on small displays and with
  long paths, increased font sizes and scaled desktops.
- Tab/Shift+Tab navigation, visible focus, arrow-key thumbnail navigation,
  Enter to apply, Escape to cancel pending work/close safely, Ctrl+F to search,
  and an accessible shortcut help view. No accidental apply on focus or hover.
- No system/network changes or downloads merely from launching the app.
- Missing backend, reload failure, permission denial and offline operation must
  produce actionable in-window errors, not only stderr or false success.

## 4. Images and performance

- Decode and generate thumbnails outside the UI thread with bounded workers,
  bounded queues, cancellation and bounded memory. Load visible thumbnails
  first; never decode every 8K original to full size at startup.
- Cache thumbnails in the XDG cache directory, keyed by canonical image
  identity and change metadata. Handle stale, corrupted and removed entries.
- Validate readable regular image files and decode limits. Handle symlinks,
  spaces, Unicode, duplicate paths and filenames beginning with a dash safely.
  Invoke processes with argument vectors, never shell-concatenated paths.
- Keep the app cheap when idle and terminate its workers cleanly on close.
  Measure startup and memory using synthetic sets including 20 large images
  and a larger directory; record measurements rather than claiming "fast".

## 5. Official wallpaper flow

GitHub ships the default Noir images only. The other nine official themes are
optional downloads from the owner's server. Use the current verified NIRUX
bundle command and pinned checksum, not another downloader in the GUI.

- Revalidate the actual archive contents, filenames, URL, byte count and hash.
  Do not assume the earlier plan to remove Noir from the ZIP was carried out.
  A changed ZIP requires new pinned integrity metadata. Never disable checking
  or invent checksum bytes to make a download pass.
- Download requires explicit consent with actual approximate size and origin.
  Show progress, support cancellation, handle interrupted/offline/low-space
  cases, and never falsely report a partial set as complete.
- Stage and validate the complete expected set before installation. Prevent
  archive traversal/symlink escapes and oversized decompression. Use disk-backed
  temporary storage with space checks rather than assuming tmpfs is large.
- Preserve user-modified and unrelated files. Do not record an unrelated file's
  current hash as NIRUX-owned: that would authorize deleting it later.
- Map official archive aliases such as noir-the-norns to the actual theme slug
  used by NIRUWM, for both resolutions. Verify all nine downloaded themes.
- Noir from the release is authoritative: the optional bundle must not replace
  it with an older version. Updates must preserve downloaded wallpapers and
  ownership records. Repeated downloads must be genuinely idempotent and report
  missing 8K files accurately, not declare completion from 4K counts alone.
- Refresh thumbnails and current theme after download. Do not silently switch
  to a custom image or change the selected theme.
- A theme without its image uses its actual theme background color, not another
  theme's wallpaper. Test reload/startup/rotation do not undo that fallback.

## 6. Login-screen behavior and security

Expose clear status: login background follows the selected theme, or a user has
chosen an explicit override. Preserve that override unless the user explicitly
chooses to reset it. Explain that a running greeter sees its updated background
on its next start; never restart SDDM/LightDM during an ordinary theme change.

Revalidate the current synchronization implementation rather than trusting its
comments or prior claims. Important security constraint: a sudoers rule naming
a binary under ~/.local/bin is NOT safe merely because the command arguments
are restricted. The user can replace that binary and obtain arbitrary root
execution. Do not add, depend on or widen such a rule.

If privileged synchronization is required, use a root-owned helper in a
root-owned directory and a narrowly authorized fixed interface. No execution
of user-writable code as root; no arbitrary destination paths; no sourcing
user configuration as shell; safe bounded regular-file image reads; canonical
target allowlists and atomic writes. Preserve non-NIRUX greeter configurations.
Test symlinks, malicious arguments, unauthorized users and concurrent theme
changes. Rapid changes must end with the last requested theme's image.

Do not expand this into a general greeter rewrite. If existing unsafe rules
must be replaced for integration, deliver a focused migration and removal test.

## 7. Launcher and NIRUX integration

- Install a .desktop entry and icon: Name=NIRU Wallpaper, meaningful English
  comment and keywords Wallpaper;Background;Appearance;. It must be searchable
  and launchable from the normal application launcher.
- Add one discoverable action in the existing Wallpaper action group opening
  niruwallpaper. Keep command-line users and existing wallpaper actions working.
- No new default keybinding. Verify the actual effective bindings before
  suggesting an optional one in the README; do not overwrite an existing action.
- Add the component to manifest staging, checksum/version validation,
  generation activation, runtime package lists, diagnostics, rollback and owned
  uninstall. A manifest pin alone is not an integration.
- Fresh installs receive it; updates add it without resetting configuration.
  Refuse standalone install/uninstall over suite-managed links, following the
  existing ownership guards. Preserve unrelated launcher entries and user data.
- Ensure needed dynamic libraries/graphics/image plugins on Arch, Debian and
  Void glibc. `ldd` and --version alone are insufficient: launch the real GUI as
  a normal user on each distribution.
- Avoid changing workspace/focus/rotation behavior as a side effect of adding
  this application. Test existing terminal focus on workspaces other than 1.

## 8. Tests and acceptance gates

Use existing harnesses where available. Do not rewrite established tests or add
delays merely to hide a failure. Preserve adversarial burst tests; a workload
that is faster than a user can type can still expose a real state bug.

### Unit and fixture tests

- Config roundtrip, comment and permission preservation, invalid TOML, partial
  sections, unknown keys, safe values, concurrent edits and failed activation.
- Mode transitions, enabling/disabling follow-theme, scaling and rotation Off.
- Image discovery, symlinks, malformed/large images, lazy thumbnail loading and
  cache invalidation. No real user files in fixtures.
- Bundle consent, checksum mismatch, incomplete set, filename mapping,
  cancellation, no-space, preservation of modified files, repeated operations
  and update preservation.
- Theme colors/custom overrides and missing-image fallback.
- Helper authorization, root ownership, path and symlink safety if affected.
- Launcher discovery and missing-backend behavior.

### Real GUI and lifecycle

1. Fresh install, launch through the launcher and open a usable window.
2. Browse without changing the desktop; preview then apply one image.
3. Apply every scale, follow theme on/off, single override and rotation Off.
4. Edit config externally and verify refresh/conflict handling.
5. Test each official theme, with missing images and after explicit download.
6. Confirm actual setter choice/pixel output, not just a reported file path.
7. Change theme repeatedly; verify desktop, app and next greeter agree without
   logging out the user. Preserve explicit login-image override.
8. Close/reopen and verify persistence; run a repeated update; verify rollback
   and owned uninstall while keeping synthetic user images intact.
9. Exercise keyboard navigation, launcher activation, small-window geometry,
   long paths, missing fonts, larger font sizes and both light/dark palettes.
10. Regress terminal opening, focus cycling, workspace round trips, bar reload,
    notifications and the existing screensaver/lock ownership boundary.

Run relevant flows on the owner's three disposable VMs (Arch, Debian, Void)
and available Debian/Void hardware when explicitly authorized. Obtain current
connection details from the owner/session, never this repository. Preserve SSH,
Tailscale, Nextcloud, documents and unrelated packages. Do not purge/reinstall
an entire desktop or restart a display manager simply to test this GUI.
Never test suspend, shutdown, authentication or real file transfers without
separate explicit authorization. A VM without a backlight cannot validate
physical brightness; record not-run, not a pass.

Record exact commands, source SHAs, binary hashes, test fixtures, results and
missing coverage. Capture screenshots with synthetic data and no credentials.
Do not claim "all functionality tested" from executable-presence checks or a
small list of keyboard shortcuts.

## 9. Documentation and publication

README for niruwallpaper: status, screenshot, purpose, installation, supported
systems, dependencies, launcher/CLI usage, modes and precedence, shortcuts,
configuration ownership, download source and size, cancellation, troubleshooting,
uninstall/data retention, test instructions and honest limitations.

Update nirux README, installation documentation, component table, manifest,
canonical config examples and diagnostics consistently. Keep existing
NIRUX-specific defaults (browser, module list, window rules, screenshot bindings)
when updating examples; do not blindly copy the WM's template over them.

Work locally in small separable commits. Minimize GitHub Actions and API use:
run local tests and VM acceptance first, avoid speculative patch releases and
repeated polling. Before any push check remote changes and integrate them
without discarding user work. Review staged content for secrets/private details.

Publish only when the owner's release authorization applies and acceptance
gates pass. Keep this new repository private until visibility is explicitly
approved; a public NIRUX installer must not pin artifacts in an inaccessible
private repository. Resolve that release gate with the owner before rollout.

Use one immutable tag per release, built from that tag's exact source. Never
force-move published tags or overwrite assets to fix a release. Verify component
assets/checksums exist before publishing the suite manifest that pins them.
Retain artifacts referenced by current manifests and standalone installers.

## Definition of done

- A coherent NIRUX-styled GUI safely controls the existing backend.
- Launcher discovery works; no conflicting default shortcut is added.
- Config remains the source of truth, with safe atomic updates and no lost
  manual edits or unrelated settings.
- Thumbnails and previews stay responsive with real 4K/8K images.
- Modes, scale, theme following, rotation and downloads behave as documented.
- Every installed official wallpaper maps to the right theme; missing images
  have the correct solid fallback; Noir cannot be downgraded by the bundle.
- No new daemon, automatic download, root execution of writable code or
  passwordless arbitrary command is introduced.
- Fresh install/update/rollback/uninstall and real GUI flows have evidence on
  supported distributions; unavailable coverage is explicitly recorded.
- Docs and a sanitized handoff make the implementation understandable to the
  next developer without guessing.
- Final report names versions, repositories/commits, what actually shipped,
  what was tested and all remaining limitations. No unsupported success claims.
