#!/bin/bash
B="-y -hide_banner -nostats -loglevel error"
M=media/uhd_hevc10_hlg.mp4
TMH="zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
fg() { local tm="$1"; [ -n "$tm" ] && tm="$tm,"; echo "color=c=black:s=1920x1080:r=30:d=8,format=yuv420p[vbase];[0:v]trim=start=0:end=8,setpts=PTS-STARTPTS+0/TB,scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,${tm}format=yuv420p[v0];[vbase][v0]overlay=eof_action=pass:enable='between(t,0,8)'[outv]"; }
# references (lossless)
./ffmpeg.exe $B -hwaccel d3d11va -i $M -filter_complex "$(fg "")" -map "[outv]" -an -c:v libx264 -qp 0 -preset ultrafast -pix_fmt yuv420p out/ref_SDRLIKE.mp4
./ffmpeg.exe $B -hwaccel d3d11va -i $M -filter_complex "$(fg "$TMH")" -map "[outv]" -an -c:v libx264 -qp 0 -preset ultrafast -pix_fmt yuv420p out/ref_HDR.mp4
for mode in HDR SDRLIKE; do
  for x in x1 x2 x3 x4 x5; do
    f=out/${x}_${mode}.mp4
    sz=$(stat -c %s $f)
    r=$(./ffmpeg.exe -hide_banner -nostats -i $f -i out/ref_${mode}.mp4 -lavfi "[0:v][1:v]ssim;[0:v][1:v]psnr" -f null - 2>&1 | grep -E 'SSIM All|PSNR' | sed -E 's/.*All:([0-9.]+).*/ssim=\1/; s/.*average:([0-9.]+).*/psnr=\1/' | tr '\n' ' ')
    echo "$mode $x size=$sz $r"
  done
done
