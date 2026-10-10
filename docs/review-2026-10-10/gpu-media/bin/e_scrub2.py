import sys, json, os
sys.path.insert(0, os.path.dirname(__file__))
from bench import run
W="/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/gpu-media/"
FF9 = "/home/orell/github/kerf/crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu"
B=["-hide_banner","-loglevel","error"]
P=W+"proxies/uhd_hevc10_hlg.mp4"
fg="color=c=black:s=960x540:d=0.1[base];[0:v]trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
tail=["-map","[outv]","-frames:v","1","-q:v","4","-f","image2pipe","-vcodec","mjpeg","pipe:1"]
cases={
 "baseline (production still argv)": [FF9]+B+["-ss","3.5","-i",P,"-filter_complex",fg]+tail,
 "+ -analyzeduration 0 -probesize 65536": [FF9]+B+["-analyzeduration","0","-probesize","65536","-ss","3.5","-i",P,"-filter_complex",fg]+tail,
 "+ -threads 1 -filter_threads 1 -filter_complex_threads 1": [FF9]+B+["-threads","1","-filter_threads","1","-filter_complex_threads","1","-ss","3.5","-i",P,"-filter_complex",fg]+tail,
 "+ both": [FF9]+B+["-analyzeduration","0","-probesize","65536","-threads","1","-filter_threads","1","-filter_complex_threads","1","-ss","3.5","-i",P,"-filter_complex",fg]+tail,
 "simple -vf scale (no composite graph), threads 1": [FF9]+B+["-threads","1","-filter_threads","1","-ss","3.5","-i",P,"-frames:v","1","-vf","scale=960:540","-q:v","4","-f","image2pipe","-vcodec","mjpeg","pipe:1"],
 "trivial lavfi null (spawn+graph floor)": [FF9]+B+["-f","lavfi","-i","color=black:s=64x64:d=0.1","-frames:v","1","-f","null","-"],
}
for k,v in cases.items():
    r=run(v,n=15,warm=2); print(f"{k:62s} {r}")
