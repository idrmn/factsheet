#!/bin/bash
# SessionStart hook: make sure the factsheet binary for this platform is in
# ${CLAUDE_PLUGIN_DATA}/bin, download it from the GitHub release that matches
# the plugin version when missing or outdated, then run it in the project cwd.
set -u

repo="idrmn/factsheet"
root="${CLAUDE_PLUGIN_ROOT:?CLAUDE_PLUGIN_ROOT is not set}"
data="${CLAUDE_PLUGIN_DATA:?CLAUDE_PLUGIN_DATA is not set}"
version=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$root/.claude-plugin/plugin.json" | head -n 1)

fail() {
  echo "factsheet: $1" >&2
  echo "## Project memory (unavailable)"
  echo "factsheet plugin: $1. Tell the user; do not try to fix it yourself."
  exit 0
}

target() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)
  case "$arch" in
    x86_64 | amd64) arch="x86_64" ;;
    aarch64 | arm64) arch="aarch64" ;;
    *) return 1 ;;
  esac
  case "$os" in
    Linux) echo "${arch}-unknown-linux-musl" ;;
    Darwin) echo "${arch}-apple-darwin" ;;
    MINGW* | MSYS* | CYGWIN*) echo "${arch}-pc-windows-msvc" ;;
    *) return 1 ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

exe="factsheet"
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) exe="factsheet.exe" ;; esac
bin="$data/bin/$exe"

if [ -x "$bin" ] && [ "$("$bin" --version 2>/dev/null)" = "factsheet $version" ]; then
  exec "$bin"
fi

[ -n "$version" ] || fail "cannot read version from plugin.json"
triple=$(target) || fail "unsupported platform $(uname -s) $(uname -m)"
command -v curl >/dev/null 2>&1 || fail "curl is not installed"

base="https://github.com/$repo/releases/download/v$version"
archive="factsheet-$triple.tar.gz"
# Staging inside $data keeps the final mv on one filesystem, so it is atomic.
mkdir -p "$data/bin" || fail "cannot create $data/bin"
tmp=$(mktemp -d "$data/tmp.XXXXXX") || fail "cannot create a temp directory in $data"
trap 'rm -rf "$tmp"' EXIT

# No retries: two 20 s downloads stay inside the 60 s hook timeout so fail() can report.
fetch() { curl -fsSL --connect-timeout 10 --max-time 20 -o "$1" "$2"; }
fetch "$tmp/$archive" "$base/$archive" || fail "cannot download $base/$archive"
fetch "$tmp/checksums.txt" "$base/checksums.txt" || fail "cannot download $base/checksums.txt"

expected=$(grep " $archive\$" "$tmp/checksums.txt" | cut -d ' ' -f 1)
actual=$(sha256 "$tmp/$archive")
[ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "checksum mismatch for $archive"

# Extract aside and move into place, so a parallel session never sees a partial binary.
mkdir -p "$tmp/out" || fail "cannot create $tmp/out"
tar -xzf "$tmp/$archive" -C "$tmp/out" || fail "cannot extract $archive"
chmod +x "$tmp/out/$exe"
mv -f "$tmp/out/$exe" "$bin" || fail "cannot install $bin"

rm -rf "$tmp"
exec "$bin"
