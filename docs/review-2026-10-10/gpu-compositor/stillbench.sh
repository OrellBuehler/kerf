#!/bin/bash
FF=${FF:-ffmpeg}
# wall ms of a one-frame FFmpeg still (spawn+open+seek+decode+scale+mjpeg q4), the way get_timeline_frame pays it
P=$1; N=${2:-40}
ts=()
for i in $(seq 1 $N); do
  s=$(date +%s%N)
  $FF -hide_banner -nostats -loglevel error -nostdin -ss $(awk -v i=$i 'BEGIN{printf "%.3f", 0.5+ (i*0.23)%8}') -i $P -frames:v 1 -vf "scale=960:540:flags=bicubic:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2" -c:v mjpeg -q:v 4 -f image2pipe pipe:1 > /dev/null
  e=$(date +%s%N); ts+=($(( (e-s)/1000 )))
done
printf '%s\n' "${ts[@]}" | sort -n | awk '{a[NR]=$1} END{printf "n=%d min %.1f med %.1f p90 %.1f ms\n", NR, a[1]/1000, a[int((NR+1)/2)]/1000, a[int(NR*0.9)]/1000}'
