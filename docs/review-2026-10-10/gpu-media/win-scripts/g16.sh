#!/bin/bash
B="-y -hide_banner -nostats -loglevel error"
M=media/uhd_hevc10_hlg.mp4
ENC="-c:v h264_amf -rc cqp -qp_i 26 -qp_p 26 -pix_fmt yuv420p out/g16.mp4"
t() { echo "== $1"; shift; ./tw.sh 2 "$@"; }
t "0 decode only, d3d11va -> null" $B -hwaccel d3d11va -i $M -an -map 0:v:0 -f null -
t "1 decode + scale 1080p -> null" $B -hwaccel d3d11va -i $M -an -vf "scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2" -f null -
t "2 decode + scale + AMF" $B -hwaccel d3d11va -i $M -an -vf "scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2,format=yuv420p" $ENC
t "3 = 2 + trim/setpts/pad/setsar/fps" $B -hwaccel d3d11va -i $M -an -vf "trim=start=0:end=8,setpts=PTS-STARTPTS,scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p" $ENC
t "4 = production graph (3 + color base + overlay)" $B -hwaccel d3d11va -i $M -an -filter_complex "\"color=c=black:s=1920x1080:r=30:d=8,format=yuv420p[vbase];[0:v]trim=start=0:end=8,setpts=PTS-STARTPTS+0/TB,scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p[v0];[vbase][v0]overlay=eof_action=pass:enable='between(t,0,8)'[outv]\"" -map "[outv]" $ENC
