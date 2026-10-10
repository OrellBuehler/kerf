#!/bin/bash
B="-hide_banner -loglevel error"
TMH="zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
TMP="${TMH//arib-std-b67/smpte2084}"
fgA="color=c=black:s=960x540:d=0.1[base];[0:v]$TMH,trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
fgB="color=c=black:s=960x540:d=0.1[base];[0:v]trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease:force_divisible_by=2,$TMH,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
fgC="color=c=black:s=960x540:d=0.1[base];[0:v]trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease:force_divisible_by=2,${TMP},pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
fgD="color=c=black:s=960x540:d=0.1[base];[0:v]${TMP},trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
T="-map [outv] -frames:v 1 -q:v 4 -f image2pipe -vcodec mjpeg pipe:1"
echo "== 4K HLG original: tonemap BEFORE scale (production still), hw=auto"; ./tw.sh 5 $B -hwaccel auto -ss 3.5 -i media/uhd_hevc10_hlg.mp4 -filter_complex "\"$fgA\"" $T
echo "== 4K HLG original: tonemap AFTER scale (export order), hw=auto"; ./tw.sh 5 $B -hwaccel auto -ss 3.5 -i media/uhd_hevc10_hlg.mp4 -filter_complex "\"$fgB\"" $T
echo "== 4K HLG original: tonemap AFTER scale, d3d11va"; ./tw.sh 5 $B -hwaccel d3d11va -ss 3.5 -i media/uhd_hevc10_hlg.mp4 -filter_complex "\"$fgB\"" $T
echo "== 4:2:2 PQ original: tonemap BEFORE scale (production), sw"; ./tw.sh 3 $B -ss 3.5 -i media/uhd_hevc422_10.mov -filter_complex "\"$fgD\"" $T
echo "== 4:2:2 PQ original: tonemap AFTER scale, sw"; ./tw.sh 3 $B -ss 3.5 -i media/uhd_hevc422_10.mov -filter_complex "\"$fgC\"" $T
