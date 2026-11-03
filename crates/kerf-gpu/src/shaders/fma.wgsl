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

// RN(a * b + c) for the bit patterns of non-negative, finite, normal (or zero) floats whose result is normal: one rounding, to
// nearest even.
fn fma_pos(a: u32, b: u32, c: u32) -> u32 {
    let ea = (a >> 23u) & 0xffu;
    let eb = (b >> 23u) & 0xffu;
    let ec = (c >> 23u) & 0xffu;
    // A zero factor leaves the addend as it is.
    if (ea == 0u || eb == 0u) {
        return c;
    }
    let prod = mul24((a & 0x7fffffu) | 0x800000u, (b & 0x7fffffu) | 0x800000u);
    // The weight of the product's top bit: 2^lp (the value is prod * 2^(ea + eb - 300)).
    let ptop = 63 - i32(clz64(prod));
    let lp = ptop + i32(ea) + i32(eb) - 300;

    var big = prod;
    var big_top = ptop;
    var big_lp = lp;
    var small = vec2<u32>(0u, 0u);
    var small_top = 0;
    var small_lp = 0;
    var has_small = false;
    if (ec != 0u) {
        let addend = vec2<u32>((c & 0x7fffffu) | 0x800000u, 0u);
        let lc = i32(ec) - 127;
        if (lc > lp) {
            big = addend;
            big_top = 23;
            big_lp = lc;
            small = prod;
            small_top = ptop;
            small_lp = lp;
        } else {
            small = addend;
            small_top = 23;
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

    // Round to 24 bits: keep one guard bit, and what is below it is a sticky bit.
    var top = 63 - i32(clz64(sum));
    let kept = shr64(sum, u32(top - 24));
    let guard = kept.x & 1u;
    var m = kept.x >> 1u;
    if (guard == 1u && (kept.z != 0u || (m & 1u) == 1u)) {
        m = m + 1u;
        if (m == 0x1000000u) {
            m = 0x800000u;
            top = top + 1;
        }
    }
    let e = big_lp + top - 62 + 127;
    return (u32(e) << 23u) | (m & 0x7fffffu);
}

fn fma_f(a: f32, b: f32, c: f32) -> f32 {
    return bitcast<f32>(fma_pos(bitcast<u32>(a), bitcast<u32>(b), bitcast<u32>(c)));
}

// RN(a * b).
fn mul_f(a: f32, b: f32) -> f32 {
    return bitcast<f32>(fma_pos(bitcast<u32>(a), bitcast<u32>(b), 0u));
}

