#!/usr/bin/env bash
# commit-msg hook (via prek): the house style for a commit subject —
# lowercase, imperative, no trailing period, short — and no AI attribution
# trailers, since the author is solely responsible for what ships.
set -euo pipefail

msg_file="$1"
subject="$(grep -v '^#' "$msg_file" | sed -n '/[^[:space:]]/{p;q}')"

fail() {
	echo "commit-msg: $1" >&2
	echo "  subject: $subject" >&2
	exit 1
}

[[ -n "$subject" ]] || fail "empty commit message"

# Generated subjects keep their own shape.
case "$subject" in
	Merge\ * | Revert\ * | fixup!* | squash!* | amend!*) exit 0 ;;
esac

[[ "$subject" =~ ^[a-z0-9] ]] || fail "start the subject lowercase (\"add feature\", not \"Add feature\")"
[[ "$subject" != *. ]] || fail "drop the trailing period from the subject"
((${#subject} <= 100)) || fail "subject is ${#subject} chars; keep it under 100 and put details in the body"

if grep -qiE '^(co-authored-by|generated-by|claude-session):.*(claude|anthropic|noreply@anthropic)|generated with .*(claude|ai)' "$msg_file"; then
	fail "remove the AI attribution trailer"
fi
