import subprocess, sys, math
import numpy as np
ff=sys.argv[1]
W,H=640,360
geom=f"scale={W}:{H}:force_original_aspect_ratio=decrease,pad={W}:{H}:(ow-iw)/2:(oh-ih)/2,setsar=1"
fade="fade=t=in:st=0.0:d=0.8:c=white"
def frames(inp, chain):
    g=f"[0:v]{chain}[v]"
    r=subprocess.run([ff,'-hide_banner','-loglevel','error','-i',inp,'-filter_complex',g,'-map','[v]','-f','rawvideo','-pix_fmt','yuv420p','pipe:1'],capture_output=True)
    if r.returncode: print(r.stderr.decode()[:300])
    return np.frombuffer(r.stdout,dtype=np.uint8)
def psnr(a,b):
    n=min(len(a),len(b)); d=a[:n].astype(int)-b[:n].astype(int); mse=(d*d).mean()
    return (99.0 if mse==0 else 10*math.log10(255*255/mse), int(abs(d).max()))
tag=frames('media/t.mp4', f"{geom},fps=10,{fade},fps=10,format=yuv420p")
unt=frames('media/u.mp4', f"{geom},fps=10,{fade},fps=10,format=yuv420p")
print("untagged vs tagged, all frames of the clip: PSNR %.1f dB, worst %d" % psnr(unt,tag))
