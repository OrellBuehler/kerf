#!/usr/bin/env bash
# Start the image the way a host would (read-only root, throwaway token) and check that it
# serves MCP: it refuses to start without a token, every runtime library resolves, the
# health check passes, a request without the token is 401, initialize is 200, a tool answers.
#
#   deploy/smoke-test.sh IMAGE
set -euo pipefail

image="${1:?usage: deploy/smoke-test.sh IMAGE}"
name="kerf-smoke-$$"
token="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')"
port=17777
url="http://127.0.0.1:${port}/mcp"
accept='Accept: application/json, text/event-stream'
json='Content-Type: application/json'
work="$(mktemp -d)"

cleanup() {
  if (($? != 0)); then
    docker logs "$name" 2>&1 | sed 's/^/  | /' >&2 || true
  fi
  docker rm -f "$name" > /dev/null 2>&1 || true
  rm -rf "$work"
}
fail() {
  echo "smoke test: $*" >&2
  exit 1
}

set +e
docker run --rm "$image" > /dev/null 2>&1
code=$?
set -e
[[ $code == 2 ]] || fail "expected exit 2 without KERF_MCP_TOKEN, got $code"

trap cleanup EXIT
docker run -d --name "$name" --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges \
  -e KERF_MCP_TOKEN="$token" -p "127.0.0.1:${port}:7777" "$image" > /dev/null

missing="$(docker exec "$name" sh -c 'ldd /usr/local/bin/kerf-app /usr/local/bin/ffmpeg | grep "not found" || true')"
[[ -z $missing ]] || fail "unresolved libraries: $missing"
docker exec "$name" ffmpeg -hide_banner -version | head -1

for _ in $(seq 60); do
  health="$(docker inspect -f '{{.State.Health.Status}}' "$name")"
  [[ $health == healthy ]] && break
  [[ $(docker inspect -f '{{.State.Running}}' "$name") == true ]] || fail "the container exited"
  sleep 2
done
[[ $health == healthy ]] || fail "not healthy: $health"

init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}'
status="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$url" -H "$json" -H "$accept" -d "$init")"
[[ $status == 401 ]] || fail "without the token: expected 401, got $status"

auth="Authorization: Bearer ${token}"
status="$(curl -s -D "$work/headers" -o "$work/init" -w '%{http_code}' -X POST "$url" -H "$auth" -H "$json" -H "$accept" -d "$init")"
[[ $status == 200 ]] || fail "initialize: expected 200, got $status: $(cat "$work/init")"
session="$(tr -d '\r' < "$work/headers" | awk -F': ' 'tolower($1) == "mcp-session-id" { print $2 }')"
[[ -n $session ]] || fail "initialize returned no session id"
grep -q '"serverInfo"' "$work/init" || fail "initialize: $(cat "$work/init")"

sid="Mcp-Session-Id: ${session}"
status="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$url" -H "$auth" -H "$sid" -H "$json" -H "$accept" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}')"
[[ $status == 202 ]] || fail "initialized: expected 202, got $status"

call='{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"project_info","arguments":{}}}'
status="$(curl -s -o "$work/call" -w '%{http_code}' -X POST "$url" -H "$auth" -H "$sid" -H "$json" -H "$accept" -d "$call")"
[[ $status == 200 ]] || fail "project_info: expected 200, got $status"
if ! grep -q '"result"' "$work/call" || grep -q '"isError":true' "$work/call"; then
  fail "project_info: $(cat "$work/call")"
fi

echo "smoke test: ok"
