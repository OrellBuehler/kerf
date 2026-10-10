#!/bin/bash
B="-y -hide_banner -nostats -loglevel error"
M=media/uhd_hevc10_hlg.mp4
SC="scale='min(1280,iw)':-2:flags=bilinear"
TMH="zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
for tmname in mobius bt.2390 hable; do
  ./ffmpeg.exe $B -ss 3.5 -hwaccel d3d11va -i $M -an -frames:v 1 -vf "libplacebo=w=1280:h=720:tonemapping=$tmname:colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv:format=yuv420p" out/lp_$tmname.png 2>&1 | head -2
  r=$(./ffmpeg.exe -hide_banner -nostats -i out/lp_$tmname.png -i out/tm_cpu.png -lavfi "[0:v][1:v]psnr" -f null - 2>&1 | grep PSNR | sed -E 's/.*average:([0-9.]+).*/\1/')
  echo "libplacebo $tmname vs CPU zscale-mobius: PSNR=$r dB"
done
echo "== timing: d3d11va decode + libplacebo (scale+tonemap mobius) + AMF intra proxy"
./tw.sh 2 $B -hwaccel d3d11va -i $M -an -vf "libplacebo=w=1280:h=720:tonemapping=mobius:colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv:format=yuv420p" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -pix_fmt nv12 -f mp4 out/lp_proxy.mp4
echo "== timing: d3d11va decode + libplacebo scale only (SDR-like) + AMF intra"
./tw.sh 2 $B -hwaccel d3d11va -i $M -an -vf "libplacebo=w=1280:h=720:format=yuv420p10le" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -pix_fmt nv12 -f mp4 out/lp_proxy2.mp4
