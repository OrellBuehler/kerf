import sys, subprocess, os, math, glob
import numpy as np
from PIL import Image
out = sys.argv[1]; ff = sys.argv[2]; fp = sys.argv[3]
def raw(mp4, frame=5):
    r = subprocess.run([ff,'-hide_banner','-loglevel','error','-i',mp4,'-vf',f'select=eq(n\\,{frame})','-frames:v','1','-f','rawvideo','-pix_fmt','yuv420p','pipe:1'],capture_output=True)
    return np.frombuffer(r.stdout, dtype=np.uint8)
def tag(mp4):
    r = subprocess.run([fp,'-v','error','-select_streams','v:0','-show_entries','stream=color_space','-of','csv=p=0',mp4],capture_output=True,text=True)
    return r.stdout.strip()
def psnr(a,b):
    d=a.astype(np.int32)-b.astype(np.int32); mse=(d*d).mean()
    return (99.0 if mse==0 else 10*math.log10(255*255/mse)), int(np.abs(d).max())
names = sorted({os.path.basename(p).rsplit('-',1)[0] for p in glob.glob(out+'/*-U.png')})
print(f"{'case':26} {'still png U vs T':>20} {'export yuv U vs T':>22}  tags(U,T)")
for n in names:
    pu=np.asarray(Image.open(f'{out}/{n}-U.png').convert('RGB')); pt=np.asarray(Image.open(f'{out}/{n}-T.png').convert('RGB'))
    s=psnr(pu,pt)
    yu=raw(f'{out}/{n}-U.mp4'); yt=raw(f'{out}/{n}-T.mp4')
    e=psnr(yu,yt) if yu.size==yt.size and yu.size else (0,-1)
    print(f"{n:26} {s[0]:8.1f} dB max {s[1]:3d}   {e[0]:8.1f} dB max {e[1]:3d}   {tag(f'{out}/{n}-U.mp4') or '-'},{tag(f'{out}/{n}-T.mp4') or '-'}")
