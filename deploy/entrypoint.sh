#!/bin/sh
# Start Kerf's MCP server for one tenant. Extra arguments go to kerf-app (e.g. --project).
set -eu

# The token comes from KERF_MCP_TOKEN or, for a Docker secret, the file KERF_MCP_TOKEN_FILE names.
if [ -n "${KERF_MCP_TOKEN_FILE:-}" ]; then
  if [ -n "${KERF_MCP_TOKEN:-}" ]; then
    echo "kerf: set KERF_MCP_TOKEN or KERF_MCP_TOKEN_FILE, not both" >&2
    exit 2
  fi
  if ! KERF_MCP_TOKEN="$(cat "$KERF_MCP_TOKEN_FILE")"; then
    echo "kerf: cannot read KERF_MCP_TOKEN_FILE ($KERF_MCP_TOKEN_FILE)" >&2
    exit 2
  fi
  export KERF_MCP_TOKEN
fi
if [ -z "${KERF_MCP_TOKEN:-}" ]; then
  echo "kerf: KERF_MCP_TOKEN (or KERF_MCP_TOKEN_FILE) is not set. Every client needs this bearer" >&2
  echo "kerf: token; make one with: openssl rand -hex 32" >&2
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

# The health check reads the port from KERF_MCP_ADDR: change the address there, not with --mcp-addr.
exec kerf-app --headless --mcp-addr "${KERF_MCP_ADDR:-0.0.0.0:7777}" --data-dir /data "$@"
