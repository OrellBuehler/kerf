import subprocess, sys, time, resource, statistics, json, os
def run(argv, n=5, warm=1, stdout_bytes=False):
    walls, cpus, sizes = [], [], []
    for i in range(warm + n):
        r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
        t0 = time.perf_counter()
        p = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        t1 = time.perf_counter()
        r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
        if p.returncode != 0:
            return {"error": p.stderr.decode()[-400:]}
        if i < warm: continue
        walls.append(t1 - t0)
        cpus.append((r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime))
        sizes.append(len(p.stdout))
    return {"wall_med": round(statistics.median(walls), 4), "wall_min": round(min(walls), 4),
            "cpu_med": round(statistics.median(cpus), 4), "out_bytes": sizes[0]}
if __name__ == "__main__":
    spec = json.loads(sys.argv[1])
    print(json.dumps(run(spec["argv"], spec.get("n", 5), spec.get("warm", 1))))
