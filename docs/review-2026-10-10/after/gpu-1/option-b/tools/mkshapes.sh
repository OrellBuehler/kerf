#!/bin/bash
FF=${FF:-/home/user/kerf/crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu}
D=$1
L="testsrc2=size=1280x720:rate=10:duration=1"
enc(){ "$FF" -hide_banner -loglevel error -y "$@" 2>&1 | head -2; }
enc -f lavfi -i "$L" -frames:v 1 -pix_fmt yuvj420p $D/photo.jpg
enc -f lavfi -i "$L" -frames:v 1 -c:v libwebp -pix_fmt yuv420p $D/photo.webp
enc -f lavfi -i "$L" -frames:v 1 -pix_fmt rgb24 $D/logo-rgb24.png
enc -f lavfi -i "$L,format=rgba" -frames:v 1 -pix_fmt rgba $D/logo-rgba.png
enc -f lavfi -i "$L,format=gray" -frames:v 1 -pix_fmt gray $D/grey.png
enc -f lavfi -i "$L,format=pal8" -frames:v 1 $D/pal8.gif
enc -f lavfi -i "$L" -frames:v 1 -pix_fmt bgr24 $D/image.bmp
enc -f lavfi -i "$L" -frames:v 1 -pix_fmt rgb24 $D/image.tiff
enc -f lavfi -i "$L" -c:v mjpeg -q:v 3 -pix_fmt yuvj420p $D/webcam.mkv
enc -f lavfi -i "$L" -c:v mjpeg -q:v 3 -pix_fmt yuvj422p $D/webcam422.avi
enc -f lavfi -i "$L" -c:v prores_ks -pix_fmt yuv422p10le $D/prores.mov
enc -f lavfi -i "$L" -c:v libx264 -qp 18 -pix_fmt yuvj420p $D/h264-yuvj.mp4
enc -f lavfi -i "$L" -c:v libx264 -qp 18 -pix_fmt yuv420p $D/h264.mp4
enc -f lavfi -i "$L" -c:v libx264rgb -qp 18 -pix_fmt rgb24 $D/h264-rgb.mp4
enc -f lavfi -i "$L" -c:v libvpx-vp9 -b:v 1M -pix_fmt yuv420p $D/vp9.webm
enc -f lavfi -i "$L" -c:v ffv1 -pix_fmt yuv420p $D/ffv1.mkv
enc -f lavfi -i "$L" -c:v ffv1 -pix_fmt bgr0 $D/ffv1-rgb.mkv
enc -f lavfi -i "$L" -c:v mpeg2video -pix_fmt yuv420p $D/mpeg2.mpg
enc -f lavfi -i "$L" -c:v rawvideo -pix_fmt yuyv422 $D/raw.avi
enc -f lavfi -i "$L,format=yuva420p" -c:v ffv1 -pix_fmt yuva420p $D/alpha.mkv
enc -f lavfi -i "$L" -frames:v 1 -pix_fmt yuv420p -c:v libaom-av1 -cpu-used 8 $D/photo.avif
enc -f lavfi -i "$L,scale=720:576" -c:v dvvideo -pix_fmt yuv420p $D/dv.dv
