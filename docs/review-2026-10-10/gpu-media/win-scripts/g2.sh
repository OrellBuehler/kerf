#!/bin/bash
B="-hide_banner -loglevel error -y"
M=media/uhd_hevc10_hlg.mp4
SC="scale='min(1280,iw)':-2:flags=bilinear"
run() { echo "== $1"; shift; ./tw.sh 2 "$@"; }
run "S1 SDR-like (no tonemap) software + x264 intra" $B -i $M -an -vf "$SC" -c:v libx264 -preset veryfast -crf 24 -g 1 -threads 12 -pix_fmt yuv420p -f mp4 out/s1.mp4
run "S2 auto + AMF" $B -hwaccel auto -i $M -an -vf "$SC" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -threads 12 -pix_fmt nv12 -f mp4 out/s2.mp4
run "S3 d3d11va + AMF" $B -hwaccel d3d11va -i $M -an -vf "$SC" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -threads 12 -pix_fmt nv12 -f mp4 out/s3.mp4
run "S5 d3d11va + x264 intra" $B -hwaccel d3d11va -i $M -an -vf "$SC" -c:v libx264 -preset veryfast -crf 24 -g 1 -threads 12 -pix_fmt yuv420p -f mp4 out/s5.mp4
run "S6 d3d11va + CPU scale, decode-only (null)" $B -hwaccel d3d11va -i $M -an -vf "$SC" -f null -
M2=media/uhd_hevc422_10.mov
run "R1 4:2:2 PQ: software decode only" $B -i $M2 -an -map 0:v:0 -f null -
run "R2 4:2:2 PQ: hwaccel auto decode only (expect software fallback)" $B -hwaccel auto -i $M2 -an -map 0:v:0 -f null -
run "R3 4:2:2 PQ: hwaccel d3d11va decode only" $B -hwaccel d3d11va -i $M2 -an -map 0:v:0 -f null -
