#!/usr/bin/env python3
"""Run a command N times under /usr/bin/time; print min CPU (user+sys) and min wall per frame, and bytes written to stdout file."""
import subprocess, sys, os, re
n_frames = int(sys.argv[1]); reps = int(sys.argv[2]); label = sys.argv[3]; cmd = sys.argv[4:]
best_cpu = best_wall = None; out_bytes = 0
for _ in range(reps):
    out = open('/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/gpu-compositor/ff.out', 'wb')
    r = subprocess.run(['/usr/bin/time', '-f', '%U %S %e'] + cmd, stdout=out, stderr=subprocess.PIPE, stdin=subprocess.DEVNULL)
    out.close()
    last = r.stderr.decode().strip().splitlines()[-1].split()
    u, s, e = map(float, last)
    out_bytes = os.path.getsize('/tmp/claude-1000/-home-orell-github-kerf/3a822707-e08c-4465-adf8-e4864e1d4032/scratchpad/review/gpu-compositor/ff.out')
    cpu = (u + s) * 1000 / n_frames; wall = e * 1000 / n_frames
    best_cpu = cpu if best_cpu is None else min(best_cpu, cpu)
    best_wall = wall if best_wall is None else min(best_wall, wall)
print(f"{label:48s} cpu {best_cpu:7.2f} ms/frame  wall {best_wall:7.2f} ms/frame  out {out_bytes//n_frames} B/frame")
