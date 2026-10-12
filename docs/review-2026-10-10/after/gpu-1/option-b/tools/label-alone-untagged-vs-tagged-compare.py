import numpy as np, math
W,H=640,360
FS=W*H*3//2
def psnr(a,b):
    d=a.astype(int)-b.astype(int); mse=(d*d).mean()
    return 99.0 if mse==0 else 10*math.log10(255*255/mse)
def load(p): return np.fromfile(p,dtype=np.uint8)
for v,name in (('9','FFmpeg 9.0.2'),('6','FFmpeg 6.1.1')):
    print("==",name,"(the HD clip alone: untagged against the same footage tagged BT.709 = the label)")
    for sc in ('alone-translucent','alone-text','alone-dip'):
        a=load(f'meas/{v}-slice/{sc}-u-export.yuv'); b=load(f'meas/{v}-slice/{sc}-t-export.yuv')
        n=len(a)//FS
        fa=a.reshape(n,FS).astype(int); fb=b.reshape(n,FS).astype(int)
        worst=[int(np.abs(fa[i]-fb[i]).max()) for i in range(n)]
        print(f"{sc:18} export {n} frames: {psnr(fa,fb):5.1f} dB, worst {max(worst)}   per frame {worst}")
        if sc!='alone-dip':
            for bld in ('still','stream','scrub'):
                a=load(f'meas/{v}-slice/{sc}-u-{bld}.rgb'); b=load(f'meas/{v}-slice/{sc}-t-{bld}.rgb')
                d=np.abs(a.astype(int)-b.astype(int))
                print(f"{'':18} {bld:6}: {psnr(a,b):5.1f} dB, worst {int(d.max())}")
