#!/usr/bin/env bash
# PostToolUse (Edit|Write): rustfmt the Rust file that was just written, so an
# agent's edits never leave the tree failing `cargo fmt --check`. A parse error
# goes back to the agent (exit 2) instead of being silently ignored.
set -uo pipefail

file="$(jq -r '.tool_input.file_path // empty')"
[[ "$file" == *.rs && -f "$file" ]] || exit 0
command -v rustfmt >/dev/null || exit 0

root="$(git -C "$(dirname "$file")" rev-parse --show-toplevel 2>/dev/null)" || exit 0
if ! out="$(rustfmt --edition 2021 --config-path "$root/rustfmt.toml" "$file" 2>&1)"; then
	echo "rustfmt failed on $file:" >&2
	echo "$out" >&2
	exit 2
fi
