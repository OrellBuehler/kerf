import sys, subprocess, os, math, glob
import numpy as np
from PIL import Image
# usage: cmp.py ffmpeg dirA dirB [suffixA suffixB]
ff = sys.argv[1]; A = sys.argv[2]; B = sys.argv[3]
sa = sys.argv[4] if len(sys.argv) > 4 else 'orig'
sb = sys.argv[5] if len(sys.argv) > 5 else 'orig'
def raw(mp4, frame=5):
    r = subprocess.run([ff,'-hide_banner','-loglevel','error','-i',mp4,'-vf',f'select=eq(n\\,{frame})','-frames:v','1','-f','rawvideo','-pix_fmt','yuv420p','pipe:1'],capture_output=True)
    return np.frombuffer(r.stdout, dtype=np.uint8)
def jpgrgb(p):
    return np.asarray(Image.open(p).convert('RGB'))
def psnr(a,b):
    if a.shape != b.shape or a.size == 0: return (0.0, -1)
    d=a.astype(np.int32)-b.astype(np.int32); mse=(d*d).mean()
    return (99.0 if mse==0 else 10*math.log10(255*255/mse)), int(np.abs(d).max())
names = sorted({os.path.basename(p).rsplit('-',1)[0] for p in glob.glob(A+f'/*-{sa}.png')})
print(f"{'case':26} {'still png':>16} {'export yuv':>16} {'stream jpg':>16} {'scrub jpg':>16}")
for n in names:
    try:
        s=psnr(np.asarray(Image.open(f'{A}/{n}-{sa}.png').convert('RGB')), np.asarray(Image.open(f'{B}/{n}-{sb}.png').convert('RGB')))
        e=psnr(raw(f'{A}/{n}-{sa}.mp4'), raw(f'{B}/{n}-{sb}.mp4'))
        pa=f'{A}/{n}-{sa}.stream.jpg'; pb=f'{B}/{n}-{sb}.stream.jpg'
        st=psnr(jpgrgb(pa), jpgrgb(pb)) if os.path.exists(pa) and os.path.exists(pb) else (0,-1)
        ja=f'{A}/{n}-{sa}.jpg'; jb=f'{B}/{n}-{sb}.jpg'
        sj=psnr(jpgrgb(ja), jpgrgb(jb))
    except Exception as ex:
        print(n, 'ERR', ex); continue
    f=lambda x: f"{x[0]:6.1f} / {x[1]:3d}"
    print(f"{n:26} {f(s):>16} {f(e):>16} {f(st):>16} {f(sj):>16}")
