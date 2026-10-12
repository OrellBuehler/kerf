import sys
# usage: argv-diff.py logA logB outdirA outdirB   -> per case: identical | what differs (after normalising the output directory)
def load(path, out):
    d = {}; key = None
    for line in open(path, errors='replace'):
        line = line.rstrip('\n')
        if line.startswith('#CASE '):
            _, n, k = line.split(' ', 2); key = (n, k); d.setdefault(key, []); continue
        if key is None: continue
        d[key].append(line.replace(out, 'OUT'))
    return d
a = load(sys.argv[1], sys.argv[3]); b = load(sys.argv[2], sys.argv[4])
same = diff = 0
for key in sorted(a):
    la, lb = a[key], b.get(key, [])
    # the policy probe and proxy encodes are not part of a case's argv: compare the runs that write to OUT or to a pipe (stream, scrub)
    keep = lambda x: 'OUT' in x or 'image2pipe' in x
    ra = [x for x in la if keep(x)]; rb = [x for x in lb if keep(x)]
    LABEL = ',setparams=colorspace=bt709'
    if ra != rb and [y.replace(LABEL, '') for y in rb] == ra:
        diff += 1; print(f"{key[0]:28} {key[1]:5} label only ({sum(LABEL in y for y in rb)} of {len(rb)} runs carry it)"); continue
    if ra == rb:
        same += 1; print(f"{key[0]:28} {key[1]:5} identical argv ({len(ra)} runs)")
    else:
        diff += 1
        tail = ''
        for x, y in zip(ra, rb):
            if x != y:
                xs, ys = x.split('\x1f'), y.split('\x1f')
                dif = [(i, p, q) for i, (p, q) in enumerate(zip(xs, ys)) if p != q]
                tail = ' | '.join(f"arg {i}: ...{p[-60:]!r} -> ...{q[-60:]!r}" if len(p) > 120 else f"{p!r} -> {q!r}" for i, p, q in dif[:1])
                if len(xs) != len(ys): tail += f" (argc {len(xs)} -> {len(ys)})"
                break
        print(f"{key[0]:28} {key[1]:5} DIFFERENT ({len(ra)} runs) {tail[:300]}")
print(f"identical {same}, different {diff}")
