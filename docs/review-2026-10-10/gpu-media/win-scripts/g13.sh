#!/bin/bash
B="-hide_banner -nostats -loglevel error"
VF="scale='min(640,iw)':-2:flags=bilinear,select='gt(scene,0.3)',showinfo"
t() { echo "== $1"; shift; ./tw.sh 2 "$@"; }
t "scene detect 4K HEVC10 original, auto(dxva2) [production]" $B -hwaccel auto -i media/uhd_hevc10_hlg.mp4 -map 0:v:0 -vf "\"$VF\"" -f null -
t "scene detect 4K HEVC10 original, d3d11va" $B -hwaccel d3d11va -i media/uhd_hevc10_hlg.mp4 -map 0:v:0 -vf "\"$VF\"" -f null -
t "scene detect 4K HEVC10 original, sw" $B -i media/uhd_hevc10_hlg.mp4 -map 0:v:0 -vf "\"$VF\"" -f null -
t "scene detect 4:2:2 4K original, auto [production]" $B -hwaccel auto -i media/uhd_hevc422_10.mov -map 0:v:0 -vf "\"$VF\"" -f null -
t "scene detect from 720p all-intra PROXY" $B -i out/a.mp4 -map 0:v:0 -vf "\"$VF\"" -f null -
