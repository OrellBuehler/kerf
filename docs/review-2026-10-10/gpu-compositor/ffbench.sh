#!/bin/bash
FF=${FF:-ffmpeg}
# CPU time (user+sys) and wall per frame for the pieces of today's playback stream, on a real proxy.
P=$1; N=$2; W=${3:-960}; H=${4:-540}; FPS=${5:-24}
T=/usr/bin/time
run() { # name, then ffmpeg args
  local name=$1; shift
  local out; out=$( { $T -f '%U %S %e' $FF -hide_banner -nostats -loglevel error -nostdin "$@" > /tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/gpu-compositor/ff.out ; } 2>&1 | tail -1)
  local bytes; bytes=$(stat -c %s /tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/gpu-compositor/ff.out)
  echo "$name|$out|$bytes" | awk -F'|' -v n=$N '{split($2,a," "); printf "%-44s cpu %6.2f ms/frame (user %.2f sys %.2f) wall %6.2f ms/frame out %d B/frame\n",$1,(a[1]+a[2])*1000/n,a[1]*1000/n,a[2]*1000/n,a[3]*1000/n,$3/n}'
}
G="color=c=black:s=${W}x${H}:r=${FPS},format=yuv420p[vbase];[0:v]trim=start=0,setpts=PTS-STARTPTS,fps=${FPS},scale=${W}:${H}:flags=bilinear:force_original_aspect_ratio=decrease,pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[vbase][v0]overlay=x=0:y=0:eof_action=pass:format=auto"
run "decode only (null)"                 -threads 4 -i $P -frames:v $N -map 0:v:0 -f null -
run "decode 1 thread (null)"             -threads 1 -i $P -frames:v $N -map 0:v:0 -f null -
run "graph, bilinear overlay (null)"     -threads 4 -i $P -filter_complex "$G,format=yuvj420p[outv]" -map "[outv]" -frames:v $N -f null -
run "graph + mjpeg q6 (today's stream)"  -threads 4 -i $P -filter_complex "$G,format=yuvj420p[outv]" -map "[outv]" -frames:v $N -c:v mjpeg -q:v 6 -f image2pipe pipe:1
run "graph + mjpeg q2"                   -threads 4 -i $P -filter_complex "$G,format=yuvj420p[outv]" -map "[outv]" -frames:v $N -c:v mjpeg -q:v 2 -f image2pipe pipe:1
run "FrameSource run: showinfo+y4m (native)" -threads 4 -copyts -start_at_zero -i $P -an -sn -dn -map 0:v:0 -vf "showinfo=checksum=0,scale=out_range=tv,setpts=N/TB" -fps_mode passthrough -frames:v $N -f yuv4mpegpipe -pix_fmt yuv420p pipe:1
run "rawvideo yuv420p (native, no showinfo)" -threads 4 -i $P -an -map 0:v:0 -frames:v $N -f rawvideo -pix_fmt yuv420p pipe:1
