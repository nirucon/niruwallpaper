#!/bin/sh
# Check the command-line behaviour that needs no X server and no NIRUWM.
#
# Run after a build: sh tests/cli-smoke.sh target/release/niruwallpaper
set -eu
bin=${1:-target/release/niruwallpaper}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { printf 'cli-smoke: %s\n' "$*" >&2; exit 1; }

version=$("$bin" --version)
# NIRUX verifies a staged component by reading the second field of --version,
# so the version must be exactly there.
field=$(printf '%s\n' "$version" | awk '{print $2}')
[ "$field" = "$(awk -F'"' '/^version = / {print $2; exit}' Cargo.toml)" ] ||
  fail "the second field of --version must be the version, got $field"

"$bin" --help | grep -q 'Usage: niruwallpaper' || fail '--help has no usage line'

# An unknown command must fail loudly rather than open a window.
if "$bin" nonsense >/dev/null 2>&1; then fail 'an unknown command succeeded'; fi

# `set` without a path must be refused before touching any setting.
if "$bin" set >/dev/null 2>&1; then fail 'set without a path succeeded'; fi

# A missing argument is named rather than opening a window.
"$bin" --help | grep -q 'screen' || fail '--help does not mention the per-screen status'

# Without a desktop installed, the state commands must explain themselves and
# fail, not print empty settings.
for command in status list; do
  if HOME="$tmp" PATH=/nonexistent "$bin" "$command" >/dev/null 2>"$tmp/err"; then
    fail "$command succeeded without niructl"
  fi
  grep -q 'niructl' "$tmp/err" || fail "$command did not explain the missing niructl"
done

printf 'cli-smoke: ok\n'
