import sys, os, subprocess, json
sys.path.insert(0,'bin')
from bench import run
W="/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/gpu-media/"
FF9="/home/orell/github/kerf/crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu"
B=["-hide_banner","-loglevel","error","-y"]
src=W+"proxies/uhd_hevc10_hlg.mp4"   # already the 720p SDR (tone-mapped) all-intra proxy: use as a clean 720p source
out={}
for name,enc in (("intra_g1",["-g","1"]),("g6",["-g","6","-bf","0"]),("g12",["-g","12","-bf","0"]),("g30_bf2",["-g","30"]),("mjpeg_q4",None)):
    dst=W+f"tmp/gop_{name}.mp4"
    if enc is None:
        cmd=[FF9]+B+["-i",src,"-an","-c:v","mjpeg","-q:v","4","-pix_fmt","yuvj420p","-f","mp4",dst]
    else:
        cmd=[FF9]+B+["-i",src,"-an","-c:v","libx264","-preset","veryfast","-crf","24"]+enc+["-pix_fmt","yuv420p","-f","mp4",dst]
    subprocess.run(cmd,check=True)
    sz=os.path.getsize(dst)
    fg="color=c=black:s=960x540:d=0.1[base];[0:v]trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
    # worst-case seek: last frame before next keyframe -> ss just before a keyframe boundary
    res={}
    for label,ss in (("ss=3.5",3.5),("ss=3.9",3.9)):
        res[label]=run([FF9]+B[:-1]+["-ss",str(ss),"-i",dst,"-filter_complex",fg,"-map","[outv]","-frames:v","1","-q:v","4","-f","image2pipe","-vcodec","mjpeg","pipe:1"],n=7)["wall_med"]
    dec=run([FF9]+B[:-1]+["-threads","1","-i",dst,"-map","0:v:0","-f","null","-"],n=3,warm=1)["wall_med"]
    out[name]={"MB":round(sz/1e6,2),"still_ms":{k:round(v*1000) for k,v in res.items()},"decode_1thread_ms_per_frame":round((dec-0.012)/240*1000,2)}
    print(name,out[name],flush=True)
