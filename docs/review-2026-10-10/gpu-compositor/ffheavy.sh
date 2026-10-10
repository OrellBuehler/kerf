#!/bin/bash
FF=${FF:-ffmpeg}
# What heavier graphs cost per frame in FFmpeg (CPU ms/frame), 960x540@24, real proxy, mjpeg q6 out (today's stream)
P=$1; N=$2; W=960; H=540; FPS=24
T=/usr/bin/time
OUT=/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/gpu-compositor/ff.out
FONT=$(fc-match -f '%{file}' sans 2>/dev/null || echo /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf)
run() {
  local name=$1; shift
  local out; out=$( { $T -f '%U %S %e' $FF -hide_banner -nostats -loglevel error -nostdin "$@" > $OUT ; } 2>&1 | tail -1)
  echo "$name|$out" | awk -F'|' -v n=$N '{split($2,a," "); printf "%-52s cpu %7.2f ms/frame  wall %7.2f ms/frame\n",$1,(a[1]+a[2])*1000/n,a[3]*1000/n}'
}
BASE="color=c=black:s=${W}x${H}:r=${FPS},format=yuv420p[vbase]"
L="trim=start=0,setpts=PTS-STARTPTS,fps=${FPS}"
FIT="scale=${W}:${H}:flags=bilinear:force_original_aspect_ratio=decrease,pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2,setsar=1"
ENC="format=yuvj420p[outv]"
OPT=(-map "[outv]" -frames:v $N -c:v mjpeg -q:v 6 -f image2pipe pipe:1)
run "1 layer (baseline)" -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" "${OPT[@]}"
run "1 layer + eq (colour)" -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT,eq=brightness=0.05:contrast=1.1:saturation=1.2[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" "${OPT[@]}"
run "1 layer + fade-in alpha (transition/fade)" -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT,format=yuva420p,fade=t=in:st=0:d=3:alpha=1[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" "${OPT[@]}"
run "1 layer + drawtext (caption)" -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[vbase][v0]overlay=eof_action=pass:format=auto,drawtext=text='Hello captions world':fontfile=$FONT:fontsize=48:fontcolor=white:box=1:boxcolor=black@0.5:boxborderw=12:x=(w-text_w)/2:y=h*0.82,$ENC" "${OPT[@]}"
run "1 layer + gblur sigma 8 (video effect)" -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT,gblur=sigma=8[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" "${OPT[@]}"
run "1 layer + ellipse mask (geq alpha)" -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT,format=yuva420p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(lt(hypot((X-W/2)/(W*0.35),(Y-H/2)/(H*0.35)),1),255,0)'[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" "${OPT[@]}"
run "1 layer rotate 8deg + opacity .8 (alpha path)" -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT,format=yuva420p,colorchannelmixer=aa=0.8,rotate=8*PI/180:ow=rotw(8*PI/180):oh=roth(8*PI/180):c=black@0[v0];[vbase][v0]overlay=x=(W-w)/2:y=(H-h)/2:eof_action=pass:format=auto,$ENC" "${OPT[@]}"
run "3 layers (base + 2 PiP w/ opacity/rotate)" -threads 4 -i $P -i $P -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[1:v]$L,scale=384:216:flags=bilinear,format=yuva420p,colorchannelmixer=aa=0.8[v1];[2:v]$L,scale=288:162:flags=bilinear,format=yuva420p,rotate=8*PI/180:ow=rotw(8*PI/180):oh=roth(8*PI/180):c=black@0[v2];[vbase][v0]overlay=eof_action=pass:format=auto[o0];[o0][v1]overlay=x=500:y=30:eof_action=pass:format=auto[o1];[o1][v2]overlay=x=40:y=300:eof_action=pass:format=auto,$ENC" "${OPT[@]}"
run "dissolve 2 clips (xfade-like: 2 alpha layers)" -threads 4 -i $P -i $P -filter_complex "$BASE;[0:v]$L,$FIT,format=yuva420p,fade=t=out:st=0:d=10:alpha=1[v0];[1:v]$L,$FIT,format=yuva420p,fade=t=in:st=0:d=10:alpha=1[v1];[vbase][v0]overlay=eof_action=pass:format=auto[o0];[o0][v1]overlay=eof_action=pass:format=auto,$ENC" "${OPT[@]}"
