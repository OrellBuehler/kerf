#!/bin/bash
# Logs every ffmpeg argv (one line, fields separated by a unit separator) to $KERF_ARGV_LOG, then runs the real one.
if [ -n "$KERF_ARGV_LOG" ]; then
  line=""
  for a in "$@"; do line="$line$a"$'\x1f'; done
  printf '%s\n' "$line" >> "$KERF_ARGV_LOG"
fi
exec "$REAL_FFMPEG" "$@"
