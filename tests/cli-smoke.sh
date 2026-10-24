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
case "$version" in "NIRU Wallpaper "*) ;; *) fail "unexpected version line: $version" ;; esac

"$bin" --help | grep -q 'Usage: niruwallpaper' || fail '--help has no usage line'

# An unknown command must fail loudly rather than open a window.
if "$bin" nonsense >/dev/null 2>&1; then fail 'an unknown command succeeded'; fi

# `set` without a path must be refused before touching any setting.
if "$bin" set >/dev/null 2>&1; then fail 'set without a path succeeded'; fi

# Without a desktop installed, the state commands must explain themselves and
# fail, not print empty settings.
if HOME="$tmp" PATH=/nonexistent "$bin" status >/dev/null 2>"$tmp/err"; then
  fail 'status succeeded without niructl'
fi
grep -q 'niructl' "$tmp/err" || fail 'status did not explain the missing niructl'

printf 'cli-smoke: ok\n'
