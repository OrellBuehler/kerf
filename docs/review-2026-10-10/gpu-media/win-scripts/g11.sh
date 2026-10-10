#!/bin/bash
B="-hide_banner -loglevel error -nostdin"
M=media/uhd_hevc10_hlg.mp4
VF="fps=fps=2:start_time=0:round=up,scale=170:96:flags=area,format=yuv420p"
t() { echo "== $1"; shift; ./tw.sh 3 "$@"; }
t "filmstrip 4K HEVC10 8s sw threads=4" $B -threads 4 -i $M -an -vf $VF -frames:v 16 -pix_fmt yuv420p -f rawvideo -
t "filmstrip 4K HEVC10 8s auto(dxva2) threads=4" $B -threads 4 -hwaccel auto -i $M -an -vf $VF -frames:v 16 -pix_fmt yuv420p -f rawvideo -
t "filmstrip 4K HEVC10 8s d3d11va" $B -threads 4 -hwaccel d3d11va -i $M -an -vf $VF -frames:v 16 -pix_fmt yuv420p -f rawvideo -
t "filmstrip 4K HEVC10 8s skip_frame nokey sw threads=4" $B -threads 4 -skip_frame nokey -i $M -an -vf $VF -frames:v 16 -pix_fmt yuv420p -f rawvideo -
t "filmstrip 4:2:2 4K 8s sw threads=4" $B -threads 4 -i media/uhd_hevc422_10.mov -an -vf $VF -frames:v 16 -pix_fmt yuv420p -f rawvideo -
t "filmstrip proxy 720p intra sw threads=4" $B -threads 4 -i out/a.mp4 -an -vf $VF -frames:v 16 -pix_fmt yuv420p -f rawvideo -
