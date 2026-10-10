#!/bin/bash
# usage: ffall.sh <proxy> <frames> ; env FF = ffmpeg binary. min of 3 runs.
FF=${FF:-ffmpeg}; P=$1; N=$2; W=960; H=540; FPS=24; R=3
BASE="color=c=black:s=${W}x${H}:r=${FPS},format=yuv420p[vbase]"
L="trim=start=0,setpts=PTS-STARTPTS,fps=${FPS}"
FIT="scale=${W}:${H}:flags=bilinear:force_original_aspect_ratio=decrease,pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2,setsar=1"
ENC="format=yuvj420p[outv]"
OPT="-map [outv] -frames:v $N -c:v mjpeg -q:v 6 -f image2pipe pipe:1"
C="$FF -hide_banner -nostats -loglevel error -nostdin"
m() { local label=$1; shift; ./runmin.py $N $R "$label" "$@"; }
m "decode only (null)" $C -threads 4 -i $P -frames:v $N -map 0:v:0 -f null -
m "FrameSource-style run (showinfo+y4m pipe)" $C -threads 4 -copyts -start_at_zero -i $P -an -sn -dn -map 0:v:0 -vf "showinfo=checksum=0,scale=out_range=tv,setpts=N/TB" -fps_mode passthrough -frames:v $N -f yuv4mpegpipe -pix_fmt yuv420p pipe:1
m "graph 1 layer -> null" $C -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[vbase][v0]overlay=eof_action=pass:format=auto,format=yuvj420p[outv]" -map "[outv]" -frames:v $N -f null -
m "stream: 1 layer + mjpeg q6" $C -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" $OPT
m "stream: 1 layer + eq" $C -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT,eq=brightness=0.05:contrast=1.1:saturation=1.2[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" $OPT
m "stream: 1 layer + clip fade (fade=t=in)" $C -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT,fade=t=in:st=0:d=3[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" $OPT
m "stream: 1 layer + drawtext caption" $C -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[vbase][v0]overlay=eof_action=pass:format=auto,drawtext=text='Hello captions world':fontfile=/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf:fontsize=48:fontcolor=white:box=1:boxcolor=black@0.5:boxborderw=12:x=(w-text_w)/2:y=h*0.82,$ENC" $OPT
m "stream: 1 layer + gblur sigma 8" $C -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT,gblur=sigma=8[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" $OPT
m "stream: 1 layer + ellipse mask (geq)" $C -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT,format=yuva420p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(lt(hypot((X-W/2)/(W*0.35),(Y-H/2)/(H*0.35)),1),255,0)'[v0];[vbase][v0]overlay=eof_action=pass:format=auto,$ENC" $OPT
m "stream: rotate 8deg + opacity .8" $C -threads 4 -i $P -filter_complex "$BASE;[0:v]$L,$FIT,format=yuva420p,colorchannelmixer=aa=0.8,rotate=8*PI/180:ow=rotw(8*PI/180):oh=roth(8*PI/180):c=black@0[v0];[vbase][v0]overlay=x=(W-w)/2:y=(H-h)/2:eof_action=pass:format=auto,$ENC" $OPT
m "stream: 3 layers (PiP opacity/rotate)" $C -threads 4 -i $P -i $P -i $P -filter_complex "$BASE;[0:v]$L,$FIT[v0];[1:v]$L,scale=384:216:flags=bilinear,format=yuva420p,colorchannelmixer=aa=0.8[v1];[2:v]$L,scale=288:162:flags=bilinear,format=yuva420p,rotate=8*PI/180:ow=rotw(8*PI/180):oh=roth(8*PI/180):c=black@0[v2];[vbase][v0]overlay=eof_action=pass:format=auto[o0];[o0][v1]overlay=x=500:y=30:eof_action=pass:format=auto[o1];[o1][v2]overlay=x=40:y=300:eof_action=pass:format=auto,$ENC" $OPT
m "stream: dissolve (2 alpha-faded layers)" $C -threads 4 -i $P -i $P -filter_complex "$BASE;[0:v]$L,$FIT,format=yuva420p,fade=t=out:st=0:d=10:alpha=1[v0];[1:v]$L,$FIT,format=yuva420p,fade=t=in:st=0:d=10:alpha=1[v1];[vbase][v0]overlay=eof_action=pass:format=auto[o0];[o0][v1]overlay=eof_action=pass:format=auto,$ENC" $OPT
