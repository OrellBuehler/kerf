import subprocess, sys, math
import numpy as np
ff = sys.argv[1]
def frame(inp, chain, n=4):
    g = f"[0:v]{chain},select=eq(n\\,{n})[v]"
    r = subprocess.run([ff,'-hide_banner','-loglevel','error','-i',inp,'-filter_complex',g,'-map','[v]','-frames:v','1','-f','rawvideo','-pix_fmt','yuv420p','pipe:1'],capture_output=True)
    if r.returncode: print(r.stderr.decode()[:300])
    return np.frombuffer(r.stdout,dtype=np.uint8)
def psnr(a,b):
    d=a.astype(int)-b.astype(int); mse=(d*d).mean(); return (99.0 if mse==0 else 10*math.log10(255*255/mse), int(abs(d).max()))
L="setparams=colorspace=bt709"
fade="fade=t=in:st=0:d=0.8:color=white"
tail="fps=10,format=yuv420p"
import sys
W,H=sys.argv[2],sys.argv[3]
geom=f"scale={W}:{H}:force_original_aspect_ratio=decrease,pad={W}:{H}:(ow-iw)/2:(oh-ih)/2,setsar=1"
cases = {
 'tagged file':        ('media/t.mp4', f"{geom},fps=10,{fade},{tail}"),
 'untagged':           ('media/u.mp4', f"{geom},fps=10,{fade},{tail}"),
 'label after geometry': ('media/u.mp4', f"{geom},{L},fps=10,{fade},{tail}"),
 'label at head':      ('media/u.mp4', f"{L},{geom},fps=10,{fade},{tail}"),
 'label after scale only': ('media/u.mp4', f"scale={W}:{H}:force_original_aspect_ratio=decrease,{L},pad={W}:{H}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=10,{fade},{tail}"),
 'label just ahead of fade': ('media/u.mp4', f"{geom},fps=10,{L},{fade},{tail}"),
}
fr = {k: frame(*v) for k,v in cases.items()}
base = fr['tagged file']
for k,v in fr.items():
    print(f"{k:28} vs tagged file: PSNR {psnr(v,base)[0]:5.1f} dB  max {psnr(v,base)[1]:3d}")
