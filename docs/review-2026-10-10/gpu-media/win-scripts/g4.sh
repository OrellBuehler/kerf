#!/bin/bash
B="-hide_banner -loglevel error -nostdin"
fg() { echo "color=c=black:s=960x540:r=30:d=8,format=yuvj420p[vbase];[0:v]trim=start=0:end=8,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease:flags=bilinear,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuvj420p[v0];[vbase][v0]overlay=eof_action=pass:enable='between(t,0,8)'[outv]"; }
stream() { # label hw input q
  echo "== $1"
  ./tw.sh 3 $B $2 -i $3 -filter_complex "\"$(fg)\"" -map "[outv]" -an -c:v mjpeg -q:v $4 -f image2pipe pipe:1
}
stream "stream 240f from AMF intra proxy (960x540 mjpeg q6) sw decode" "" out/a.mp4 6
stream "stream 240f from x264 intra proxy sw decode" "" out/d.mp4 6
stream "stream 240f from AMF intra proxy, -hwaccel auto" "-hwaccel auto" out/a.mp4 6
stream "stream 240f from AMF intra proxy, q2" "" out/a.mp4 2
stream "stream 240f from 4K HLG ORIGINAL (no tonemap! decode+scale+mjpeg only) sw" "" media/uhd_hevc10_hlg.mp4 6
stream "stream 240f from 4K HLG ORIGINAL, d3d11va" "-hwaccel d3d11va" media/uhd_hevc10_hlg.mp4 6
stream "stream 240f from 4K HLG ORIGINAL, auto(dxva2)" "-hwaccel auto" media/uhd_hevc10_hlg.mp4 6
