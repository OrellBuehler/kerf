#!/bin/bash
B="-y -hide_banner -nostats -loglevel error"
M=media/uhd_hevc422_10.mov
TMP="zscale=tin=smpte2084:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
fg() { echo "color=c=black:s=1920x1080:r=30:d=8,format=yuv420p[vbase];[0:v]trim=start=0:end=8,setpts=PTS-STARTPTS+0/TB,scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,$TMP,format=yuv420p[v0];[vbase][v0]overlay=eof_action=pass:enable='between(t,0,8)'[outv]"; }
x() { echo "== $1"; shift; ./tw.sh 2 $B "$@" -filter_complex "\"$(fg)\"" -map "[outv]" -an $ENC -pix_fmt yuv420p out/r_$$.mp4; }
ENC="-c:v libx264 -crf 20 -preset medium -profile:v high"; x "R5II-like 4:2:2 PQ -> 1080p SDR: auto + x264 medium (current GUI default w/ GPU decode on)" -hwaccel auto -i $M
ENC="-c:v h264_amf -rc cqp -qp_i 26 -qp_p 26 -profile:v high"; x "R5II-like: auto + h264_amf qp26" -hwaccel auto -i $M
