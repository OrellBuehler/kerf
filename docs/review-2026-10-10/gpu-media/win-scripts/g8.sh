#!/bin/bash
B="-y -hide_banner -nostats -loglevel error"
M=media/uhd_hevc10_hlg.mp4
TMH="zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
SC="scale='min(1280,iw)':-2:flags=bilinear"
echo "== reference CPU chain, 1 frame @ t=3.5 -> png"
./ffmpeg.exe $B -ss 3.5 -hwaccel d3d11va -i $M -an -frames:v 1 -vf "$SC,$TMH" out/tm_cpu.png
echo "== OpenCL tonemap (mobius), 1 frame"
./ffmpeg.exe $B -ss 3.5 -init_hw_device opencl=ocl -filter_hw_device ocl -hwaccel d3d11va -i $M -an -frames:v 1 -vf "$SC,format=p010le,hwupload,tonemap_opencl=tonemap=mobius:param=0.7:desat=0:r=tv:p=bt709:t=bt709:m=bt709:format=nv12,hwdownload,format=nv12" out/tm_ocl.png 2>&1 | tail -3
echo "== PSNR/SSIM ocl vs cpu"
./ffmpeg.exe -hide_banner -nostats -i out/tm_ocl.png -i out/tm_cpu.png -lavfi "[0:v][1:v]ssim;[0:v][1:v]psnr" -f null - 2>&1 | grep -E 'SSIM|PSNR' | tr '\n' ' '; echo
echo "== libplacebo-free alternatives: simple colorspace-only (what the fallback does)"
./ffmpeg.exe $B -ss 3.5 -hwaccel d3d11va -i $M -an -frames:v 1 -vf "$SC,colorspace=all=bt709:iall=bt2020:fast=1,format=yuv420p" out/tm_cs.png
./ffmpeg.exe -hide_banner -nostats -i out/tm_cs.png -i out/tm_cpu.png -lavfi "[0:v][1:v]psnr" -f null - 2>&1 | grep -E 'PSNR' | tr '\n' ' '; echo
echo "== OpenCL chain full proxy timing (240f): d3d11va decode -> CPU scale -> p010 -> OCL tonemap -> AMF intra"
./tw.sh 2 $B -init_hw_device opencl=ocl -filter_hw_device ocl -hwaccel d3d11va -i $M -an -vf "$SC,format=p010le,hwupload,tonemap_opencl=tonemap=mobius:param=0.7:desat=0:r=tv:p=bt709:t=bt709:m=bt709:format=nv12,hwdownload,format=nv12" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -pix_fmt nv12 -f mp4 out/ocl_proxy.mp4
echo "== same, CPU zscale chain, AMF intra (g1 C)"
./tw.sh 2 $B -hwaccel d3d11va -i $M -an -vf "$SC,$TMH" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -pix_fmt nv12 -f mp4 out/cpu_proxy.mp4
