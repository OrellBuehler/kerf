#!/bin/bash
B="-hide_banner -loglevel error"
TMH="zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
TMP="${TMH//arib-std-b67/smpte2084}"
still() { # $1 label, $2 hwargs, $3 ss, $4 input, $5 tonemap-prefix(or empty)
  local label="$1" hw="$2" ss="$3" in="$4" tm="$5"
  local pre=""; [ -n "$tm" ] && pre="$tm,"
  local fg="color=c=black:s=960x540:d=0.1[base];[0:v]${pre}trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
  echo "== $label"
  ./tw.sh 5 $B $hw -ss $ss -i $in -filter_complex "\"$fg\"" -map "[outv]" -frames:v 1 -q:v 4 -f image2pipe -vcodec mjpeg pipe:1
}
still "fhd_h264 original (sw)" "" 3.5 media/fhd_h264_sdr.mp4 ""
still "fhd_h264 original (auto=dxva2)" "-hwaccel auto" 3.5 media/fhd_h264_sdr.mp4 ""
still "4K hevc10 HLG proxy-AMF (SDR 1280, intra)" "" 3.5 out/a.mp4 ""
still "4K hevc10 HLG proxy-x264 (SDR 1280, intra)" "" 3.5 out/d.mp4 ""
still "4K hevc10 HLG ORIGINAL, tonemap at full-res in graph (sw)" "" 3.5 media/uhd_hevc10_hlg.mp4 "$TMH"
still "4K hevc10 HLG ORIGINAL, tonemap at full-res in graph (auto=dxva2)" "-hwaccel auto" 3.5 media/uhd_hevc10_hlg.mp4 "$TMH"
still "4K hevc422 PQ ORIGINAL, tonemap at full-res in graph (sw)" "" 3.5 media/uhd_hevc422_10.mov "$TMP"
