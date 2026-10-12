#!/bin/bash
set -e
FF=/home/user/kerf/crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu
D=${1:-media}
mkdir -p $D
GRAD="gradients=size=1280x720:rate=10:duration=1:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=1280:y1=720:speed=0.00001"
DET="testsrc2=size=1280x720:rate=10:duration=1"
GRADSD="gradients=size=720x576:rate=10:duration=1:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=720:y1=576:speed=0.00001"
DETSD="testsrc2=size=720x576:rate=10:duration=1"
none="setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown"
b709="setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709"
b601="setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m"
enc() { # name source matrix label
  $FF -hide_banner -loglevel error -y -f lavfi -i "$2,format=gbrp,scale=out_color_matrix=$3:out_range=tv,format=yuv420p,$4" -c:v libx264 -preset ultrafast -qp 0 -g 10 -pix_fmt yuv420p $D/$1.mp4
}
enc u "$GRAD" bt709 "$none"          # untagged HD, BT.709 values
enc t "$GRAD" bt709 "$b709"          # same, tagged bt709
enc u2 "$DET" bt709 "$none"          # untagged HD detail
enc b709 "$DET" bt709 "$b709"        # BT.709 detail
enc sd601 "$DETSD" bt601 "$b601"     # SD smpte170m
enc sdu "$DETSD" bt601 "$none"       # SD untagged
enc hd601 "$DET" bt601 "$b601"       # HD tagged smpte170m
$FF -hide_banner -loglevel error -y -f lavfi -i "$DET,format=yuv420p,setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv,zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv,format=yuv420p10le" -c:v libx265 -x265-params log-level=error -color_primaries bt2020 -color_trc arib-std-b67 -colorspace bt2020nc $D/hlg.mp4
$FF -hide_banner -loglevel error -y -f lavfi -i "color=c=0xf0a020:s=320x180:d=1,drawbox=x=20:y=20:w=120:h=80:color=0x2060e0:t=fill,format=rgb24" -frames:v 1 $D/logo.png
$FF -hide_banner -loglevel error -y -f lavfi -i "$DET,format=gbrp" -c:v ffv1 -pix_fmt bgr0 $D/rgb.mkv
# yuvj (JFIF/MJPEG) HD, untagged
$FF -hide_banner -loglevel error -y -f lavfi -i "$DET" -c:v mjpeg -q:v 2 -pix_fmt yuvj420p $D/mjpeg.mkv
# alpha
$FF -hide_banner -loglevel error -y -f lavfi -i "$DET,format=yuva420p" -c:v ffv1 -pix_fmt yuva420p $D/alpha.mkv
ls -la $D
