import sys, json, os
sys.path.insert(0, os.path.dirname(__file__))
from bench import run
W="/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/gpu-media/"
FF9 = "/home/orell/github/kerf/crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu"
FF4 = "/usr/bin/ffmpeg"
B=["-hide_banner","-loglevel","error"]
def still(ff, src, ss, pre=""):
    fg = f"color=c=black:s=960x540:d=0.1[base];[0:v]{pre}trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
    return [ff]+B+["-ss",str(ss),"-i",src,"-filter_complex",fg,"-map","[outv]","-frames:v","1","-q:v","4","-f","image2pipe","-vcodec","mjpeg","pipe:1"]
TMH="zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p,"
res={}
for name, ff in (("ff9",FF9),("ff4",FF4)):
    res[name+"/version"]=run([ff,"-version"],n=7,warm=1)
    res[name+"/lavfi1"]=run([ff]+B+["-f","lavfi","-i","color=black:s=64x64:d=0.1","-frames:v","1","-f","null","-"],n=7,warm=1)
    res[name+"/still_proxy_x264intra"]=run(still(ff,W+"proxies/uhd_hevc10_hlg.mp4",3.5),n=7)
    res[name+"/still_fhd_original"]=run(still(ff,W+"media/fhd_h264_sdr.mp4",3.5),n=7)
    res[name+"/still_4k_hevc10_original_notm"]=run(still(ff,W+"media/uhd_hevc10_hlg.mp4",3.5),n=5)
    res[name+"/still_4k_hlg_original_TM_at_4k"]=run(still(ff,W+"media/uhd_hevc10_hlg.mp4",3.5,TMH),n=3)
    res[name+"/still_4k_422_original_notm"]=run(still(ff,W+"media/uhd_hevc422_10.mov",3.5),n=5)
    for k,v in res.items():
        if k.startswith(name): print(k, json.dumps(v), flush=True)
json.dump(res, open(W+"logs/scrub_linux.json","w"), indent=1)
