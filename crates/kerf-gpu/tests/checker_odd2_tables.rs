//! The independent checker's guard on the odd round trip's filter tables: the shaders of an odd picture (`fs_chroma_h_scaled`, `fs_wide`,
//! `fs_tall`, `fs_chroma_up`, the vertical pass of the way back) read `sws::bicubic` -- the table of the C build (`filterAlign` 1) -- where a SIMD
//! build of swscale has the horizontal tables aligned to 4 taps and the vertical ones to 2 (`sws_init_context`, `X86_MMX(cpu_flags)`), whose padding is
//! not zeros (`sws::table_aligned`). The two are the same table for every size from 11 samples on; under that (a 4:7 enlargement is two taps) they
//! are not, which is the plan's reason to refuse a side under 16 (`TRANSLUCENT_ODD_MIN_SIDE`).
//!
//! No FFmpeg and no GPU: the tables themselves. `#[ignore]`d for its run time in a debug build (about 40 s); local only.

#![allow(clippy::print_stderr)]

use kerf_gpu::sws::{self, Scaler};

/// The sizes under 4097 at which the SIMD build's aligned table is another than the C one.
fn differing(one: i32, align: usize, pairs: &dyn Fn(u32) -> (u32, u32)) -> Vec<u32> {
    (4..4097u32)
        .filter(|n| {
            let (s, d) = pairs(*n);
            sws::table_aligned_uncached(Scaler::Bicubic, s, d, one, 1)
                != sws::table_aligned_uncached(Scaler::Bicubic, s, d, one, align)
        })
        .collect()
}

#[test]
#[ignore = "about 40 s in a debug build"]
fn checker_odd2_tables_aligned_and_unaligned_are_one_table_from_11_samples_on() {
    let mut worst = 0u32;
    for (name, one, align, pairs) in [
        (
            "h shrink w -> ceil(w / 2) (the way back of an odd width, 9.0)",
            sws::ONE_HORIZONTAL,
            sws::SIMD_ALIGN_HORIZONTAL,
            &(|w: u32| (w, w.div_ceil(2))) as &dyn Fn(u32) -> (u32, u32),
        ),
        (
            "v shrink h -> ceil(h / 2) (the way back)",
            sws::ONE_VERTICAL,
            sws::SIMD_ALIGN_VERTICAL,
            &|h: u32| (h, h.div_ceil(2)),
        ),
        (
            "h grow ceil(w / 2) -> w (the way out of an odd height and width)",
            sws::ONE_HORIZONTAL,
            sws::SIMD_ALIGN_HORIZONTAL,
            &|w: u32| (w.div_ceil(2), w),
        ),
        (
            "v grow ceil(h / 2) -> h (the way out of an odd height)",
            sws::ONE_VERTICAL,
            sws::SIMD_ALIGN_VERTICAL,
            &|h: u32| (h.div_ceil(2), h),
        ),
        (
            "h shrink 2w -> w",
            sws::ONE_HORIZONTAL,
            sws::SIMD_ALIGN_HORIZONTAL,
            &|w: u32| (w * 2, w),
        ),
        ("v shrink 2h -> h", sws::ONE_VERTICAL, sws::SIMD_ALIGN_VERTICAL, &|h: u32| {
            (h * 2, h)
        }),
    ] {
        let diff = differing(one, align, pairs);
        eprintln!("{name}: aligned tables differ at {diff:?}");
        worst = worst.max(diff.last().copied().unwrap_or(0));
    }
    assert!(worst <= 10, "an aligned table differs from the C one at {worst} samples");
}
