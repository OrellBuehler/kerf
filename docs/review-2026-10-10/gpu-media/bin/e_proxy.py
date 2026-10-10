import sys, json, os
sys.path.insert(0, os.path.dirname(__file__))
from bench import run
FF9 = "/home/orell/github/kerf/crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu"
FF4 = "/usr/bin/ffmpeg"
M = "media/"
TM_HLG = "zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
TM_PQ = TM_HLG.replace("arib-std-b67", "smpte2084")
srcs = {
  "uhd_hevc10_hlg": (M+"uhd_hevc10_hlg.mp4", TM_HLG),
  "uhd_hevc422_pq": (M+"uhd_hevc422_10.mov", TM_PQ),
  "fhd_h264_sdr":  (M+"fhd_h264_sdr.mp4", None),
}
base = ["-hide_banner","-loglevel","error","-y"]
out = {}
for ff_name, ff in (("ff9", FF9),):
  for name,(src,tm) in srcs.items():
    scale = "scale='min(1280,iw)':-2:flags=bilinear"
    stages = {}
    stages["a_decode_only"] = [ff]+base+["-i",src,"-an","-map","0:v:0","-f","null","-"]
    stages["b_decode_scale"] = [ff]+base+["-i",src,"-an","-vf",scale,"-f","null","-"]
    if tm:
      stages["c_decode_scale_tonemap"] = [ff]+base+["-i",src,"-an","-vf",scale+","+tm,"-f","null","-"]
    vf = scale + ("," + tm if tm else "")
    stages["d_full_x264_intra"] = [ff]+base+["-i",src,"-an","-vf",vf,"-c:v","libx264","-preset","veryfast","-crf","24","-g","1","-threads","12","-pix_fmt","yuv420p","-f","mp4","proxies/%s.mp4"%name]
    for st, argv in stages.items():
      r = run(argv, n=3, warm=0)
      out[f"{ff_name}/{name}/{st}"] = r
      print(f"{ff_name}/{name}/{st}", json.dumps(r), flush=True)
json.dump(out, open("logs/proxy.json","w"), indent=1)
