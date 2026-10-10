#!/bin/bash
B="-y -hide_banner -nostats -loglevel error"
M=media/uhd_hevc10_hlg.mp4
TMH="zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
fg() { local tm="$1"; [ -n "$tm" ] && tm="$tm,"; echo "color=c=black:s=1920x1080:r=30:d=8,format=yuv420p[vbase];[0:v]trim=start=0:end=8,setpts=PTS-STARTPTS+0/TB,scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,${tm}format=yuv420p[v0];[vbase][v0]overlay=eof_action=pass:enable='between(t,0,8)'[outv]"; }
x() { # label tm hw encargs out
  echo "== $1"
  ./tw.sh 2 $B $3 -i $M -filter_complex "\"$(fg "$2")\"" -map "[outv]" -an $4 -pix_fmt yuv420p out/$5.mp4
}
for mode in HDR SDRLIKE; do
  tm=""; [ $mode = HDR ] && tm="$TMH"
  x "[$mode] X1 sw decode + libx264 crf20 medium" "$tm" "" "-c:v libx264 -crf 20 -preset medium -profile:v high" x1_$mode
  x "[$mode] X2 auto(dxva2) + libx264 crf20 medium" "$tm" "-hwaccel auto" "-c:v libx264 -crf 20 -preset medium -profile:v high" x2_$mode
  x "[$mode] X3 d3d11va + libx264 crf20 medium" "$tm" "-hwaccel d3d11va" "-c:v libx264 -crf 20 -preset medium -profile:v high" x3_$mode
  x "[$mode] X4 d3d11va + h264_amf cqp20 (Kerf spelling)" "$tm" "-hwaccel d3d11va" "-c:v h264_amf -rc cqp -qp_i 20 -qp_p 20 -profile:v high" x4_$mode
  x "[$mode] X5 d3d11va + hevc_amf cqp23" "$tm" "-hwaccel d3d11va" "-c:v hevc_amf -rc cqp -qp_i 23 -qp_p 23 -tag:v hvc1" x5_$mode
done
