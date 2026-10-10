import sys
from PIL import Image
import numpy as np
name=sys.argv[1]; step=int(sys.argv[2]) if len(sys.argv)>2 else 640
i=Image.open(f'{name}.png').convert('RGB')
a=np.array(i).astype(int)
bg=a[a.shape[0]-3, 8]
rows=np.where((abs(a-bg).sum(axis=2)>25).any(axis=1))[0]
H=min(a.shape[0], rows.max()+20)
n=0
for y in range(0,H,step):
    i.crop((0,y,i.width,min(H,y+step))).save(f'{name}-{n}.png'); n+=1
print(n, H)
