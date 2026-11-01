#!/usr/bin/env bash
# Install headless Kerf and FFmpeg on Ubuntu (24.04 in a Claude Code cloud environment, as root),
# so a session's agent can run `kerf-app --headless --stdio` as its MCP server.
#
# Paste into the environment's setup script:
#   curl -fsSL https://raw.githubusercontent.com/OrellBuehler/kerf/main/scripts/cloud-setup.sh | bash
#
# KERF_VERSION=0.30.0 pins a release (default: the latest). KERF_SHA256=<hex> checks the package.
# Safe to run again: an installed matching version is kept.
set -euo pipefail

repo=OrellBuehler/kerf
say() { echo "kerf-setup: $*" >&2; }

sudo=""
if [ "$(id -u)" -ne 0 ]; then
  sudo="sudo"
fi

version="${KERF_VERSION:-}"
if [ -z "$version" ]; then
  # The latest release's tag, from the redirect alone: github.com is reachable where the API may not be.
  url=$(curl -fsSIL -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest")
  version="${url##*/v}"
fi
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) say "could not tell the release version (got '$version'); set KERF_VERSION"; exit 1 ;;
esac

installed=$(dpkg-query -W -f '${Version}' kerf 2>/dev/null || true)
need_ffmpeg=1
command -v ffmpeg >/dev/null && command -v ffprobe >/dev/null && need_ffmpeg=0
if [ "$installed" = "$version" ] && [ "$need_ffmpeg" -eq 0 ]; then
  say "Kerf $version and FFmpeg are already installed"
  exit 0
fi

arch=$(dpkg --print-architecture)
case "$arch" in
  amd64 | arm64) ;;
  *) say "no Kerf package for $arch"; exit 1 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
deb="$work/Kerf_${version}_${arch}.deb"
say "downloading Kerf $version ($arch)"
curl -fsSL --retry 3 -o "$deb" "https://github.com/$repo/releases/download/v$version/Kerf_${version}_${arch}.deb"
if [ -n "${KERF_SHA256:-}" ]; then
  echo "$KERF_SHA256  $deb" | sha256sum -c - >/dev/null || { say "the package does not match KERF_SHA256"; exit 1; }
fi

say "installing Kerf, its GTK / WebKitGTK libraries and FFmpeg"
export DEBIAN_FRONTEND=noninteractive
$sudo apt-get update -qq
$sudo apt-get install -y -qq --no-install-recommends ffmpeg "$deb" >/dev/null

kerf-app --headless --help >/dev/null
say "ready: $(command -v kerf-app) $version, $(ffmpeg -version | sed -n 1p)"
