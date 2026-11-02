#!/bin/sh
# Start Kerf's MCP server for one tenant. Extra arguments go to kerf-app (e.g. --project).
set -eu

if [ -z "${KERF_MCP_TOKEN:-}" ]; then
  echo "kerf: KERF_MCP_TOKEN is not set. Every client needs this bearer token; make one with" >&2
  echo "kerf:   openssl rand -hex 32" >&2
  exit 2
fi

# Optional: fill the cache volume before serving (whisper[:NAME], kokoro[:VOICE], demucs, soundfont).
# A model that cannot be fetched now is fetched on first use instead.
if [ -n "${KERF_PREFETCH_MODELS:-}" ]; then
  code=0
  kerf-app --headless --data-dir /data --prefetch-models "$KERF_PREFETCH_MODELS" || code=$?
  case $code in
    0) ;;
    2) exit 2 ;;
    *) echo "kerf: some models could not be prefetched; they download on first use" >&2 ;;
  esac
fi

exec kerf-app --headless --mcp-addr "${KERF_MCP_ADDR:-0.0.0.0:7777}" --data-dir /data "$@"
