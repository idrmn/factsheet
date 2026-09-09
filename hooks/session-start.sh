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
tmp=$(mktemp -d) || fail "cannot create a temp directory"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL --retry 3 -o "$tmp/$archive" "$base/$archive" \
  || fail "cannot download $base/$archive"
curl -fsSL --retry 3 -o "$tmp/checksums.txt" "$base/checksums.txt" \
  || fail "cannot download $base/checksums.txt"

expected=$(grep " $archive\$" "$tmp/checksums.txt" | cut -d ' ' -f 1)
actual=$(sha256 "$tmp/$archive")
[ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "checksum mismatch for $archive"

mkdir -p "$data/bin" || fail "cannot create $data/bin"
tar -xzf "$tmp/$archive" -C "$data/bin" || fail "cannot extract $archive"
chmod +x "$bin"

exec "$bin"
