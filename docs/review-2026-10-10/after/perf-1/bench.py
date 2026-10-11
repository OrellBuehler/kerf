import hashlib, json, os, statistics, subprocess, sys, time

FF = "/home/user/kerf/crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu"

def run_once(argv):
    t0 = time.perf_counter()
    p = subprocess.Popen([FF] + argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    out = p.stdout.read()
    err = p.stderr.read()
    _, status, ru = os.wait4(p.pid, 0)
    wall = (time.perf_counter() - t0) * 1000
    cpu = (ru.ru_utime + ru.ru_stime) * 1000
    return wall, cpu, hashlib.md5(out).hexdigest(), len(out), status, err.decode(errors="replace").strip()

def main():
    n = int(sys.argv[1])
    cases = sys.argv[2:]
    argvs = {c: json.load(open(c)) for c in cases}
    for c in cases:
        w, cpu, h, size, status, err = run_once(argvs[c])
        if status != 0 or size == 0:
            print("WARMUP FAILED", c, status, err[-300:])
    res = {c: ([], [], set()) for c in cases}
    for i in range(n):
        for c in (cases if i % 2 == 0 else cases[::-1]):
            w, cpu, h, size, status, err = run_once(argvs[c])
            res[c][0].append(w); res[c][1].append(cpu); res[c][2].add(h)
    for c in cases:
        w, cpu, hs = res[c]
        w.sort(); cpu.sort()
        q = lambda a, f: a[min(len(a) - 1, int(f * len(a)))]
        print(f"{os.path.basename(c):28s} wall ms median {statistics.median(w):7.1f} p10 {q(w,.1):7.1f} p90 {q(w,.9):7.1f} | cpu ms median {statistics.median(cpu):7.1f} | md5 {','.join(sorted(h[:8] for h in hs))}")

main()
