#!/bin/bash
FF=${FF:-ffmpeg}; P=$1; N=300; W=1920; H=1080; FPS=30; R=3
C="$FF -hide_banner -nostats -loglevel error -nostdin -y"
BASE="color=c=black:s=${W}x${H}:r=${FPS},format=yuv420p[vbase]"
L="trim=start=0,setpts=PTS-STARTPTS,fps=${FPS}"
FIT="scale=${W}:${H}:flags=bicubic:force_original_aspect_ratio=decrease,pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2,setsar=1"
m() { local label=$1; shift; ./runmin.py $N $R "$label" "$@"; }
G1="$BASE;[0:v]$L,$FIT[v0];[vbase][v0]overlay=eof_action=pass:format=auto,format=yuv420p[outv]"
m "decode only (1080p h264 source)" $C -i $P -frames:v $N -map 0:v:0 -f null -
m "graph, 1 layer, no encode" $C -i $P -filter_complex "$G1" -map "[outv]" -frames:v $N -f null -
m "graph 1 layer + libx264 medium crf23 (ffmpeg default)" $C -i $P -filter_complex "$G1" -map "[outv]" -frames:v $N -c:v libx264 -crf 23 -pix_fmt yuv420p -f null -
m "graph 1 layer + libx264 veryfast crf23" $C -i $P -filter_complex "$G1" -map "[outv]" -frames:v $N -c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p -f null -
G3="$BASE;[0:v]$L,$FIT[v0];[1:v]$L,scale=768:432:flags=bicubic,format=yuva420p,colorchannelmixer=aa=0.8[v1];[2:v]$L,scale=576:324:flags=bicubic,format=yuva420p,rotate=8*PI/180:ow=rotw(8*PI/180):oh=roth(8*PI/180):c=black@0[v2];[vbase][v0]overlay=eof_action=pass:format=auto[o0];[o0][v1]overlay=x=1000:y=60:eof_action=pass:format=auto[o1];[o1][v2]overlay=x=80:y=600:eof_action=pass:format=auto,format=yuv420p[outv]"
m "graph 3 layers (alpha PiPs), no encode" $C -i $P -i $P -i $P -filter_complex "$G3" -map "[outv]" -frames:v $N -f null -
m "graph 3 layers + libx264 medium" $C -i $P -i $P -i $P -filter_complex "$G3" -map "[outv]" -frames:v $N -c:v libx264 -crf 23 -pix_fmt yuv420p -f null -
GM="$BASE;[0:v]$L,$FIT,format=yuva420p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(lt(hypot((X-W/2)/(W*0.35),(Y-H/2)/(H*0.35)),1),255,0)'[v0];[vbase][v0]overlay=eof_action=pass:format=auto,format=yuv420p[outv]"
m "graph ellipse mask (geq), no encode" $C -i $P -filter_complex "$GM" -map "[outv]" -frames:v $N -f null -
m "graph ellipse mask (geq) + libx264 medium" $C -i $P -filter_complex "$GM" -map "[outv]" -frames:v $N -c:v libx264 -crf 23 -pix_fmt yuv420p -f null -
