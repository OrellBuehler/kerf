#!/bin/bash
FF=${FF:-ffmpeg}
P=$1; N=300; W=1920; H=1080; FPS=30
T=/usr/bin/time
OUT=/dev/null
run() {
  local name=$1; shift
  local out; out=$( { $T -f '%U %S %e' $FF -hide_banner -nostats -loglevel error -nostdin -y "$@" ; } 2>&1 | tail -1)
  echo "$name|$out" | awk -F'|' -v n=$N '{split($2,a," "); printf "%-52s cpu %7.2f ms/frame  wall %7.2f ms/frame (%.0f fps wall)\n",$1,(a[1]+a[2])*1000/n,a[3]*1000/n, n/a[3]}'
}
BASE="color=c=black:s=${W}x${H}:r=${FPS},format=yuv420p[vbase]"
L="trim=start=0,setpts=PTS-STARTPTS,fps=${FPS}"
FIT="scale=${W}:${H}:flags=bicubic:force_original_aspect_ratio=decrease,pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2,setsar=1"
ENC=(-c:v libx264 -crf 23 -pix_fmt yuv420p -f null -)
run "decode only (1080p h264)" -i $P -frames:v $N -map 0:v:0 -f null -
run "graph 1 layer, no encode" -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[vbase][v0]overlay=eof_action=pass:format=auto,format=yuv420p[outv]" -map "[outv]" -frames:v $N -f null -
run "graph 1 layer + x264 medium crf23" -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[vbase][v0]overlay=eof_action=pass:format=auto,format=yuv420p[outv]" -map "[outv]" -frames:v $N "${ENC[@]}"
run "graph 3 layers (alpha PiPs) + x264 medium" -i $P -i $P -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[1:v]$L,scale=768:432:flags=bicubic,format=yuva420p,colorchannelmixer=aa=0.8[v1];[2:v]$L,scale=576:324:flags=bicubic,format=yuva420p,rotate=8*PI/180:ow=rotw(8*PI/180):oh=roth(8*PI/180):c=black@0[v2];[vbase][v0]overlay=eof_action=pass:format=auto[o0];[o0][v1]overlay=x=1000:y=60:eof_action=pass:format=auto[o1];[o1][v2]overlay=x=80:y=600:eof_action=pass:format=auto,format=yuv420p[outv]" -map "[outv]" -frames:v $N "${ENC[@]}"
run "graph 1 layer + geq mask + x264 medium" -i $P -filter_complex "$BASE;[0:v]$L,$FIT,format=yuva420p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(lt(hypot((X-W/2)/(W*0.35),(Y-H/2)/(H*0.35)),1),255,0)'[v0];[vbase][v0]overlay=eof_action=pass:format=auto,format=yuv420p[outv]" -map "[outv]" -frames:v $N "${ENC[@]}"
run "graph 1 layer + x264 veryfast crf23" -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[vbase][v0]overlay=eof_action=pass:format=auto,format=yuv420p[outv]" -map "[outv]" -frames:v $N -c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p -f null -
