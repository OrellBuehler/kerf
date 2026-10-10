#!/bin/bash
B="-hide_banner -loglevel error -y"
M=media/uhd_hevc10_hlg.mp4
TM="zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
SC="scale='min(1280,iw)':-2:flags=bilinear"
run() { echo "== $1"; shift; ./tw.sh 2 "$@"; }
run "A production default: hwaccel auto + CPU scale + CPU tonemap + h264_amf(cqp24,g1)" $B -hwaccel auto -i $M -an -vf "$SC,$TM" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -threads 12 -pix_fmt nv12 -f mp4 out/a.mp4
run "B hwaccel auto + libx264 intra" $B -hwaccel auto -i $M -an -vf "$SC,$TM" -c:v libx264 -preset veryfast -crf 24 -g 1 -threads 12 -pix_fmt yuv420p -f mp4 out/b.mp4
run "C hwaccel d3d11va + h264_amf" $B -hwaccel d3d11va -i $M -an -vf "$SC,$TM" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -threads 12 -pix_fmt nv12 -f mp4 out/c.mp4
run "D all software: hwaccel none + libx264 intra" $B -i $M -an -vf "$SC,$TM" -c:v libx264 -preset veryfast -crf 24 -g 1 -threads 12 -pix_fmt yuv420p -f mp4 out/d.mp4
run "E hwaccel none + h264_amf" $B -i $M -an -vf "$SC,$TM" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -threads 12 -pix_fmt nv12 -f mp4 out/e.mp4
