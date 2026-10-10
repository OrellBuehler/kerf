#!/bin/bash
B="-hide_banner -loglevel error"
fg="color=c=black:s=960x540:d=0.1[base];[0:v]trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
t() { echo "== $1"; shift; ./tw.sh 15 "$@"; }
T="-map [outv] -frames:v 1 -q:v 4 -f image2pipe -vcodec mjpeg pipe:1"
t "baseline proxy still (production argv, sw)" $B -ss 3.5 -i out/a.mp4 -filter_complex "\"$fg\"" $T
t "proxy still + threads 1/1/1" $B -threads 1 -filter_threads 1 -filter_complex_threads 1 -ss 3.5 -i out/a.mp4 -filter_complex "\"$fg\"" $T
t "proxy still, hwaccel auto + threads 1/1/1" $B -threads 1 -filter_threads 1 -filter_complex_threads 1 -hwaccel auto -ss 3.5 -i out/a.mp4 -filter_complex "\"$fg\"" $T
t "proxy still, baseline + hwaccel auto (production today)" $B -hwaccel auto -ss 3.5 -i out/a.mp4 -filter_complex "\"$fg\"" $T
t "version floor" -version
