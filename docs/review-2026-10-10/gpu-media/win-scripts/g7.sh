#!/bin/bash
B="-y -hide_banner -nostats -loglevel error"
M=media/uhd_hevc10_hlg.mp4
fg() { echo "color=c=black:s=1920x1080:r=30:d=8,format=yuv420p[vbase];[0:v]trim=start=0:end=8,setpts=PTS-STARTPTS+0/TB,scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p[v0];[vbase][v0]overlay=eof_action=pass:enable='between(t,0,8)'[outv]"; }
q() { # label encargs outname
  s=$(date +%s%N)
  ./ffmpeg.exe $B -hwaccel d3d11va -i $M -filter_complex "$(fg)" -map "[outv]" -an $2 -pix_fmt yuv420p out/$3.mp4 2>&1 | tail -2
  e=$(date +%s%N)
  f=out/$3.mp4; sz=$(stat -c %s $f)
  r=$(./ffmpeg.exe -hide_banner -nostats -i $f -i out/ref_SDRLIKE.mp4 -lavfi "[0:v][1:v]ssim;[0:v][1:v]psnr" -f null - 2>&1 | grep -E 'SSIM All|PSNR' | sed -E 's/.*All:([0-9.]+).*/ssim=\1/; s/.*average:([0-9.]+).*/psnr=\1/' | tr '\n' ' ')
  echo "$1 wall=$(( (e-s)/1000000 ))ms size=$sz $r"
}
q "x264 medium crf20"     "-c:v libx264 -crf 20 -preset medium" q_x264_20
q "x264 medium crf26"     "-c:v libx264 -crf 26 -preset medium" q_x264_26
q "h264_amf cqp20 (balanced)" "-c:v h264_amf -rc cqp -qp_i 20 -qp_p 20" q_a20
q "h264_amf cqp26 (balanced)" "-c:v h264_amf -rc cqp -qp_i 26 -qp_p 26" q_a26
q "h264_amf cqp30 (balanced)" "-c:v h264_amf -rc cqp -qp_i 30 -qp_p 30" q_a30
q "h264_amf cqp26 quality preset" "-c:v h264_amf -quality quality -rc cqp -qp_i 26 -qp_p 26" q_a26q
q "h264_amf vbr_peak 5M quality" "-c:v h264_amf -quality quality -rc vbr_peak -b:v 5M -maxrate 8M" q_a5m
q "hevc_amf cqp26" "-c:v hevc_amf -rc cqp -qp_i 26 -qp_p 26 -qp_b 26" q_h26
q "hevc_amf cqp29 quality" "-c:v hevc_amf -quality quality -rc cqp -qp_i 29 -qp_p 29" q_h29
q "av1_amf cqp 90 quality" "-c:v av1_amf -quality quality -rc cqp -qp_i 90 -qp_p 90" q_av1
