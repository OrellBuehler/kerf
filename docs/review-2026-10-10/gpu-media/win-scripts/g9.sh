#!/bin/bash
B="-hide_banner -loglevel error"
still() { # label hw ss input
  local fg="color=c=black:s=960x540:d=0.1[base];[0:v]trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
  echo "== $1"
  ./tw.sh 7 $B $2 -ss $3 -i $4 -filter_complex "\"$fg\"" -map "[outv]" -frames:v 1 -q:v 4 -f image2pipe -vcodec mjpeg pipe:1
}
for hw in "" "-hwaccel auto" "-hwaccel d3d11va"; do
  still "proxy 720p intra  hw=[$hw]" "$hw" 3.5 out/a.mp4
done
for hw in "" "-hwaccel auto" "-hwaccel d3d11va"; do
  still "FHD h264 original  hw=[$hw]" "$hw" 3.5 media/fhd_h264_sdr.mp4
done
for hw in "" "-hwaccel auto" "-hwaccel d3d11va"; do
  still "4K HEVC10 original (no tonemap) hw=[$hw]" "$hw" 3.5 media/uhd_hevc10_hlg.mp4
done
