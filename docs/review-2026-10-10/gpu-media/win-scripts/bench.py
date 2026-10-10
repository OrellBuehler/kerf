import subprocess, sys, time, statistics, json
def run(argv, n=7, warm=1, cwd=None):
    walls=[]; sizes=[]
    for i in range(warm+n):
        t0=time.perf_counter()
        p=subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=cwd)
        t1=time.perf_counter()
        if p.returncode!=0: return {"error": p.stderr.decode(errors='replace')[-300:]}
        if i<warm: continue
        walls.append(t1-t0); sizes.append(len(p.stdout))
    return {"med_ms": round(statistics.median(walls)*1000,1), "min_ms": round(min(walls)*1000,1), "max_ms": round(max(walls)*1000,1), "out_bytes": sizes[0]}
