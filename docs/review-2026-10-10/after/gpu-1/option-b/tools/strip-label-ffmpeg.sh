#!/bin/bash
args=()
for a in "$@"; do args+=("${a//setparams=colorspace=bt709,/}"); done
exec "$REAL_FFMPEG" "${args[@]}"
