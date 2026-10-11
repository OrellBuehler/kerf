#!/bin/sh
# A clip for measuring the A/V offset of playback on a real machine: 30 s of 30 fps video whose every
# frame shows its own number, and on every whole second (frames 0, 30, 60, ...) the whole frame flashes
# white for two frames; the audio is silence with a 10 ms 1 kHz beep at every whole second, so the flash
# and the beep are meant to be the same instant. Import it, put it on the timeline, play it, and record
# the screen and the sound together (a phone filming the screen with the speaker in view, or a screen
# capture plus a loopback of the audio): the offset is the distance between a flash and its beep.
#
#   KERF_FFMPEG=/path/to/ffmpeg sh make-av-fixture.sh out.mp4
set -eu
out=${1:-av-barcode.mp4}
ff=${KERF_FFMPEG:-ffmpeg}
"$ff" -hide_banner -loglevel error -y \
  -f lavfi -i "color=c=black:s=1280x720:r=30:d=30" \
  -f lavfi -i "aevalsrc='if(lt(mod(t,1),0.01),0.8*sin(2*PI*1000*t),0)':s=48000:d=30" \
  -vf "drawtext=text='%{frame_num}':fontsize=220:fontcolor=white:x=(w-tw)/2:y=(h-th)/2:start_number=0,geq=lum='if(lt(mod(N,30),2),255,lum(X,Y))':cb='128':cr='128'" \
  -c:v libx264 -g 1 -crf 18 -pix_fmt yuv420p -c:a aac -b:a 128k -shortest "$out"
echo "wrote $out"
