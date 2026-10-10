#!/bin/bash
FF=${FF:-ffmpeg}
P=$1; W=960; H=540; FPS=24
BASE="color=c=black:s=${W}x${H}:r=${FPS},format=yuv420p[vbase]"
L="trim=start=0,setpts=PTS-STARTPTS,fps=${FPS}"
FIT="scale=${W}:${H}:flags=bilinear:force_original_aspect_ratio=decrease,pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2,setsar=1"
ENC="format=yuvj420p[outv]"
t() { # label, args...
  local label=$1; shift
  local r=$(for i in $(seq 1 15); do s=$(date +%s%N); $FF -hide_banner -nostats -loglevel error -nostdin "$@" -frames:v 1 -c:v mjpeg -q:v 6 -f image2pipe pipe:1 >/dev/null; e=$(date +%s%N); echo $(( (e-s)/1000 )); done | sort -n | awk '{a[NR]=$1} END{printf "min %.0f med %.0f ms", a[1]/1000, a[int((NR+1)/2)]/1000}')
  echo "$label: $r"
}
t "time-to-first-frame, 1 layer" -ss 2 -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" -map "[outv]"
t "time-to-first-frame, 3 layers (3 inputs)" -ss 2 -i $P -ss 2 -i $P -ss 2 -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[1:v]$L,scale=384:216:flags=bilinear,format=yuva420p,colorchannelmixer=aa=0.8[v1];[2:v]$L,scale=288:162:flags=bilinear,format=yuva420p,rotate=8*PI/180:ow=rotw(8*PI/180):oh=roth(8*PI/180):c=black@0[v2];[vbase][v0]overlay=eof_action=pass:format=auto[o0];[o0][v1]overlay=x=500:y=30:eof_action=pass:format=auto[o1];[o1][v2]overlay=x=40:y=300:eof_action=pass:format=auto,$ENC" -map "[outv]"
