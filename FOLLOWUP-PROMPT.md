# NIRUX wallpaper and suite consistency followup

Updated: 2026-10-25. Task IDs: WALL-001, WALL-002, DOCS-001.

This is the current followup assignment for an explicitly commissioned agent.
The application already exists: improve it, do not rebuild it from the original
implementation brief. Revalidate all observations below against current source.
Do not publish this working brief or private handoff material as release text.

## Authorization and scope

An owner instruction to execute this file authorizes local implementation,
documentation and non-destructive isolated tests for the tasks below. Merely
reading or maintaining this file does not start implementation.

- WALL-001 and WALL-002: implementation in niruwallpaper, niruwm and nirux only.
- DOCS-001: documentation, license metadata and justified Rust minimum-version
  metadata corrections in nirux, niruwm, nirubar, nirulauncher, niruwallpaper,
  nirufetch, nirushot, niruscreensaver, nirulock, nirunotify and niruwebapp.
- No unrelated features, framework migration or broad dependency upgrades.
- Do not change GitHub descriptions again: those have already been normalized.
- Ask separately before commits/pushes, tags, releases, visibility changes,
  installations, privileged operations or changes to the owner's live desktop.
  Record ready-to-release manifest changes locally without publishing pins to
  unavailable artifacts. Do not claim an unreleased feature is already shipped.
- No subagents unless the owner explicitly authorizes delegation.

Read applicable AGENTS.md files in every target. Inspect local status and remote
source, preserve other work, and record starting branches, SHAs and versions.
Use isolated worktrees where needed. Do not treat audit snapshots as a writable
primary checkout without checking their origin and intended use.

The private planning repository is not an implementation target. If separately
authorized to maintain it, follow its instructions and documentation checker.

## Baseline to verify, not a version prescription

The 2026-10-25 audit found NIRUX 0.12.11 with these published manifest pins:

| Component | Version |
|---|---|
| niruwm | 0.16.84 |
| nirubar | 0.18.37 |
| niruscreensaver | 0.8.6 |
| nirulock | 0.16.13 |
| nirulauncher | 0.2.32 |
| nirunotify | 0.3.11 |
| nirushot | 0.4.4 |
| niruwallpaper | 0.1.0 |
| niruwebapp | 0.1.3 |
| nirufetch | 0.1.4 |

Published manifest, release tags and source package versions agreed, and the
NIRUWM release's nirubar-version asset matched the suite pin. Binary behavior
and real multi-monitor flows were not verified. Niruwallpaper's 26 unit tests
passed; that is not desktop acceptance evidence. Never downgrade to this table.

## WALL-001: stabilize existing wallpaper flows first

Inspect src/backend.rs, src/window.rs, src/thumbs.rs and src/theme.rs in
niruwallpaper; wallpaper.rs, config.rs, bin/niructl.rs and monitor events in
niruwm; installer ownership and wallpaper integration in nirux.

Address these findings with regression tests:

1. Live state and appearance: the GUI currently loads its palette at startup
   and does not automatically refresh externally changed state or image lists.
   Follow actual theme and appearance overrides live, including atomic config
   replacement. Preserve pending edits and expose explicit conflict handling.
   Do not overwrite dirty forms silently or poll expensively when idle.
2. Honest effective state: status currently describes configuration rather than
   proving what the WM rendered. Distinguish configured, pending, effective and
   failed state. Do not mark an image active when wallpaper ownership is disabled
   or infer a random rotation's actual image from the theme setting alone.
3. Coherent modes: choosing an image enters single mode, which ignores rotation
   intervals. Make single/fixed/random, rotation Off and theme following explicit
   and non-contradictory. Disable or explain ineffective controls; provide a
   deliberate transition to slideshow behavior. Theme following must not be
   silently undone by the rotation timer or missing-image fallback.
4. Transactional apply: current GUI commands update each field separately.
   Add a narrowly scoped validated backend transaction with one acknowledged
   activation. Preserve existing CLI compatibility, comments, permissions,
   unknown keys and unrelated settings. Prevent lost concurrent edits. Reject
   malformed config intact; partial persistence must not masquerade as success.
   On failure, refresh/report actual state and roll back only when safe against
   intervening edits. A saved config alone is not proof of successful activation.
5. Image formats: backend discovery includes WebP and BMP, but preview decoding
   currently enables only PNG/JPEG. Align documented, previewed and applicable
   formats with verified backend capabilities; explain unsupported files.
6. Thumbnail work: image::open currently decodes originals before thumbnailing,
   on the paint path. Use bounded background work, visible-first loading,
   cancellation, memory/decode limits and in-memory reuse of small previews.
   Key cache invalidation robustly, including same-size rapid replacements;
   handle corrupt caches and failed decodes without repeated blocking retries.
   Do not claim decoder-level downsampling unless it actually occurs.
7. Keyboard UX: arrows currently move selection, Space stages it and Enter
   applies pending state. Make this discoverable and consistent. All controls
   need visible focus and keyboard access, including scaling, rotation, theme
   following and discard. Avoid accidental application on focus/hover.
8. Installer icon: nirux currently prepends an ownership comment to PNG bytes.
   Install the PNG unchanged and track binary ownership separately using the
   installer's safe ownership conventions. Test fresh install, update, rollback
   and uninstall with modified/unrelated files. Do not fix installation by
   weakening ownership guards or overwriting an unowned icon.

Keep NIRUWM the sole wallpaper owner, niructl the control interface and the
existing XDG-aware shared config the source of truth. No GUI setter subprocess,
second settings store, independent rotation timer or new wallpaper daemon.
Image files must never be modified or deleted by normal wallpaper selection.

Complete and test this phase before introducing per-monitor state.

## WALL-002: explicit multi-monitor behavior

Implement backend support first, then expose it in niruwallpaper. Treat the
following as the intended defaults and acceptance requirements:

- Default: the same image on every active monitor, scaled/cropped independently
  for each monitor's geometry. Do not stretch one canvas across the desktop.
  Default rotation changes the common image together on all inheriting outputs.
- Optional: an explicit per-monitor image and scale override. Keep the default
  GUI simple, with a clear common/all-displays choice and an advanced per-display
  choice. Users can reset one output to inherit the common settings.
- Preserve global theme following: inherited outputs follow it; explicit local
  image overrides remain intact until explicitly reset. Preview and status must
  explain the effective image, scale and inheritance for each output.
- New monitors inherit the common setting. Disconnected outputs retain saved
  overrides for reconnection. Never identify monitors by vector/index position.
  Reuse stable output names and the suite's existing monitor identity machinery;
  document port-change limitations. Do not add speculative identity subsystems.
- Handle hotplug, output reordering, resolution changes, negative origins,
  portrait layouts and different aspect ratios. Reapply without losing overrides
  or needlessly resetting rotation. Define a deterministic policy for mirrored
  outputs sharing a CRTC, which cannot show different images in the same area.
- Migrate existing configurations without changing their intended common image,
  mode, theme behavior, file lists or rotation settings. Preserve unknown fields.
- Verify setter capabilities rather than assuming feh, hsetroot, nitrogen and
  xwallpaper have equivalent per-output/scale semantics. Choose and document a
  consistent supported strategy and ensure required packages on all supported
  distributions. Never silently substitute a spanning canvas or different scale.
- Extend niructl with structured per-output status and transactional updates
  where needed, preserving existing commands and old common-setting semantics.
- No panoramic spanning mode, separate per-monitor slideshow schedules or
  greeter redesign in this task. Desktop per-monitor overrides must not alter
  the existing login-background policy or explicit login-image override.

## DOCS-001: consistent and accurate suite documentation

- Use the shared component README introduction linking to NIRUX. Adopt the
  suite's existing Early beta wording consistently unless the owner has since
  chosen a different status. Use NIRUX as the suite name consistently while
  preserving established product/display names and the author's introduction.
- Bring niruwallpaper's README into that structure and add the missing MIT
  LICENSE file after confirming the owner's existing suite copyright notice.
  Its Cargo metadata already declares MIT; do not relicense unrelated work.
- Fix nirubar's statement that four themes are built in: ten were present at
  audit time. Verify the actual current theme inventory before writing a count.
- Correct minimum Rust versions against the locked dependency graph. At audit
  time image 0.25.10 required Rust 1.88.0, while niruwallpaper advertised 1.75
  and nirushot declared 1.85. Verify with the claimed toolchain where available;
  otherwise state that verification is missing. Do not force every component to
  the same Rust version or downgrade dependencies just for cosmetic uniformity.
- Document actual installation, modes, precedence, supported formats, shortcuts,
  monitor defaults/overrides, limitations and troubleshooting. Remove claims of
  live synchronization or successful application not supported by evidence.
- Keep independent component versions. Verify source version, --version output,
  tags, assets/checksums, bundled nirubar version and suite manifest compatibility.
  Do not bump every component merely to make numbers visually identical.
- Preserve the already normalized GitHub descriptions, existing shortcuts and
  unrelated defaults. No mass prose rewrite or new documentation framework.

## Verification and completion gates

Run relevant format/lint/unit/CLI tests and add focused regression fixtures.
Tests must use synthetic images and sandboxed config/cache locations, never
the owner's real desktop settings. Keep changes in reviewable task-sized diffs.

Required behavioral coverage:

- External config/theme/appearance changes, dirty-form conflicts, invalid config,
  concurrent writes, failed setter/reload, disabled ownership and backend absence.
- All supported scales and modes, effective rotation, missing-theme-image solid
  fallback and return from a custom image to theme following.
- PNG/JPEG and every additional supported format; corrupt/huge images, spaces,
  Unicode, symlinks, dash-prefixed paths and same-size rapid file replacement.
  Pass subprocess arguments safely; never concatenate untrusted shell commands.
- Measure startup, idle cost, responsiveness and peak memory with at least 20
  synthetic large images and a larger directory. Report measurements and limits.
- Real keyboard-only operation and usable geometry on small and portrait screens.
- Two and three outputs where available, mixed resolutions, portrait/landscape,
  mirrors, hotplug/reconnection/reordering, restart and retained overrides.
  Verify actual rendered output with distinguishable fixtures, not status alone.
- Fresh install/update/rollback/owned uninstall in disposable environments,
  including a decodable unchanged PNG icon and preserved user files/settings.
- Arch, Debian and Void glibc normal-user GUI coverage on explicitly authorized
  disposable targets. Virtual RandR tests are useful but do not replace physical
  hotplug evidence. Do not restart the owner's display manager or run power,
  lock/authentication, sudo-policy or real file-transfer tests without permission.

If a target or physical monitor setup is unavailable, report NOT RUN and the
remaining acceptance gate. Never weaken tests or claim complete multi-monitor
support from a build, mock or configured monitor list alone.

Deliver a local handoff containing per-task status, changed repositories/files,
starting/ending SHAs where applicable, exact tests and results, evidence of GUI
flows, skipped coverage, open decisions and a proposed release order. Release
only with separate owner approval: publish dependency artifacts/checksums first,
then the compatible suite manifest. Never move tags or replace release assets.
