#!/bin/bash
# Healthy when the MCP listener answers an authenticated request (a request without the
# token would be logged as refused on every probe). A GET with no session opens nothing.
set -eu
token="${KERF_MCP_TOKEN:-}"
if [[ -z $token && -n ${KERF_MCP_TOKEN_FILE:-} ]]; then
  token="$(< "$KERF_MCP_TOKEN_FILE")"
fi
# The port of KERF_MCP_ADDR, which is what the entrypoint listens on.
port="${KERF_MCP_ADDR:-0.0.0.0:7777}"
port="${port##*:}"
exec 3<>"/dev/tcp/127.0.0.1/${port}"
printf 'GET /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer %s\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n' \
  "$token" >&3
read -r -t 5 _ status _ <&3
[[ $status =~ ^[1-4][0-9][0-9]$ && $status != 401 ]]
