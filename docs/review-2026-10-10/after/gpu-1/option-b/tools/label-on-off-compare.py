import numpy as np, math, sys
W,H=640,360
FS=W*H*3//2
def psnr(a,b):
    d=a.astype(int)-b.astype(int); mse=(d*d).mean()
    return 99.0 if mse==0 else 10*math.log10(255*255/mse)
def load(p,dt=np.uint8): return np.fromfile(p,dtype=dt)
for v,name in (('9','FFmpeg 9.0.2'),('6','FFmpeg 6.1.1')):
    print("==",name)
    for sc in ('dip-into-hd','dip-out-of-hd','opaque','translucent'):
        a=load(f'meas/{v}-on/{sc}-export.yuv'); b=load(f'meas/{v}-off/{sc}-export.yuv')
        n=len(a)//FS
        fa=a.reshape(n,FS).astype(int); fb=b.reshape(n,FS).astype(int)
        worst=[int(np.abs(fa[i]-fb[i]).max()) for i in range(n)]
        worsty=[int(np.abs(fa[i,:W*H]-fb[i,:W*H]).max()) for i in range(n)]
        pf=[psnr(fa[i],fb[i]) for i in range(n)]
        print(f"{sc:14} export {n} frames: all frames {psnr(fa,fb):5.1f} dB, worst {max(worst)} (Y {max(worsty)}); worst per frame {worst}")
        print(f"{'':14}   per-frame dB {[round(x,1) for x in pf]}")
    for sc in ('opaque','translucent'):
        for bld in ('still','stream','scrub'):
            a=load(f'meas/{v}-on/{sc}-{bld}.rgb'); b=load(f'meas/{v}-off/{sc}-{bld}.rgb')
            if a.shape!=b.shape: print(sc,bld,'shape',a.shape,b.shape); continue
            d=np.abs(a.astype(int)-b.astype(int))
            print(f"{sc:14} {bld:6}: {psnr(a,b):5.1f} dB, worst {int(d.max())}")
