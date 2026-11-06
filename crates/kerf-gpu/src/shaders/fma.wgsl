// The fused multiply-add of a `float`, in integers (included ahead of `blur.wgsl` and `blur_out.wgsl`; see `blur.wgsl`).

// ---- 64-bit integers as (lo, hi) -----------------------------------------------------------------------------------------

fn shl64(v: vec2<u32>, s: u32) -> vec2<u32> {
    if (s == 0u) {
        return v;
    }
    if (s >= 64u) {
        return vec2<u32>(0u, 0u);
    }
    if (s >= 32u) {
        return vec2<u32>(0u, v.x << (s - 32u));
    }
    return vec2<u32>(v.x << s, (v.y << s) | (v.x >> (32u - s)));
}

// `v >> s` in .xy and 1 in .z where a bit that was shifted out is set.
fn shr64(v: vec2<u32>, s: u32) -> vec3<u32> {
    if (s == 0u) {
        return vec3<u32>(v.x, v.y, 0u);
    }
    if (s >= 64u) {
        return vec3<u32>(0u, 0u, select(0u, 1u, (v.x | v.y) != 0u));
    }
    if (s >= 32u) {
        let t = s - 32u;
        let dropped = v.x | (v.y & ((1u << t) - 1u));
        return vec3<u32>(v.y >> t, 0u, select(0u, 1u, dropped != 0u));
    }
    let dropped = v.x & ((1u << s) - 1u);
    return vec3<u32>((v.x >> s) | (v.y << (32u - s)), v.y >> s, select(0u, 1u, dropped != 0u));
}

fn add64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.x + b.x;
    let carry = select(0u, 1u, lo < a.x);
    return vec2<u32>(lo, a.y + b.y + carry);
}

fn clz64(v: vec2<u32>) -> u32 {
    if (v.y != 0u) {
        return countLeadingZeros(v.y);
    }
    return 32u + countLeadingZeros(v.x);
}

// The exact 48-bit product of two 24-bit significands.
fn mul24(a: u32, b: u32) -> vec2<u32> {
    let al = a & 0xfffu;
    let ah = a >> 12u;
    let bl = b & 0xfffu;
    let bh = b >> 12u;
    var p = vec2<u32>(al * bl, 0u);
    p = add64(p, shl64(vec2<u32>(al * bh + ah * bl, 0u), 12u));
    p = add64(p, shl64(vec2<u32>(ah * bh, 0u), 24u));
    return p;
}

// ---- the fused multiply-add ----------------------------------------------------------------------------------------------

// RN(a * b + c) for the bit patterns of non-negative finite floats (zero, subnormals and normals), with one rounding, to nearest even,
// and the result **subnormal, underflowing to zero or overflowing to infinity as IEEE 754 has it** (`f32::mul_add`, `vfmadd231ps` without
// flush-to-zero: the recursion of a bright dot on a field of zeros decays through 2^-126 and into the subnormals, and FFmpeg's
// answer there is the subnormal, not zero). Everything is on the bit patterns: no float arithmetic touches a value, so no backend's
// handling of denormals (flush on load, on store, in a multiply) can change one. A NaN, an infinity or a negative operand is not
// in the domain (`kerf_core::gblur::Gaussian::in_shader_domain` keeps them out).
fn fma_pos(a: u32, b: u32, c: u32) -> u32 {
    let ea = (a >> 23u) & 0xffu;
    let eb = (b >> 23u) & 0xffu;
    let ec = (c >> 23u) & 0xffu;
    // A float is m * 2^(e - 150) with the significand m (the hidden bit included for a normal) and e = max(exponent field, 1).
    let ma = (a & 0x7fffffu) | select(0x800000u, 0u, ea == 0u);
    let mb = (b & 0x7fffffu) | select(0x800000u, 0u, eb == 0u);
    let mc = (c & 0x7fffffu) | select(0x800000u, 0u, ec == 0u);
    // A zero factor leaves the addend as it is.
    if (ma == 0u || mb == 0u) {
        return c;
    }
    let prod = mul24(ma, mb);
    // The weight of the product's top bit: 2^lp (the value is prod * 2^(ea + eb - 300), the exponents of a subnormal read as 1).
    let ptop = 63 - i32(clz64(prod));
    let lp = ptop + i32(max(ea, 1u)) + i32(max(eb, 1u)) - 300;

    var big = prod;
    var big_top = ptop;
    var big_lp = lp;
    var small = vec2<u32>(0u, 0u);
    var small_top = 0;
    var small_lp = 0;
    var has_small = false;
    if (mc != 0u) {
        let addend = vec2<u32>(mc, 0u);
        let ctop = 31 - i32(countLeadingZeros(mc));
        let lc = ctop + i32(max(ec, 1u)) - 150;
        if (lc > lp) {
            big = addend;
            big_top = ctop;
            big_lp = lc;
            small = prod;
            small_top = ptop;
            small_lp = lp;
        } else {
            small = addend;
            small_top = ctop;
            small_lp = lc;
        }
        has_small = true;
    }

    // A 64-bit window with the larger operand's top bit at bit 62.
    let wide = shl64(big, u32(62 - big_top));
    var narrow = vec2<u32>(0u, 0u);
    var sticky = 0u;
    if (has_small) {
        let pos = 62 - (big_lp - small_lp);
        let s = pos - small_top;
        if (s >= 0) {
            narrow = shl64(small, u32(s));
        } else {
            let r = shr64(small, u32(-s));
            narrow = r.xy;
            sticky = r.z;
        }
    }
    var sum = add64(wide, narrow);
    // (A bit lost off the bottom of the window is far below the rounding position: it only says "not a tie".)
    sum.x = sum.x | sticky;

    // Round to the quantum of the result: 24 significant bits for a normal one (the bit below them is the guard bit, and what is
    // below that a sticky bit), and the fixed quantum 2^-149 where the result is subnormal. Bit i of the window weighs 2^(big_lp - 62 + i).
    let top = 63 - i32(clz64(sum));
    let shift = max(top - 23, -149 - (big_lp - 62));
    let kept = shr64(sum, u32(shift - 1));
    let guard = kept.x & 1u;
    var m = kept.x >> 1u;
    if (guard == 1u && (kept.z != 0u || (m & 1u) == 1u)) {
        m = m + 1u;
    }
    // m is the result in quanta of 2^q (q = big_lp - 62 + shift): a subnormal when q is -149, a normal with its hidden bit otherwise,
    // and a carry out of the rounding (2^24 for a normal, 2^23 for a subnormal) lands in the next exponent by itself.
    let q = big_lp - 62 + shift;
    let field = q + 149;
    if (field + i32(m >> 23u) >= 255) {
        return 0x7f800000u;
    }
    return (u32(field) << 23u) + m;
}

// RN(a * b), on bit patterns.
fn mul_pos(a: u32, b: u32) -> u32 {
    return fma_pos(a, b, 0u);
}
