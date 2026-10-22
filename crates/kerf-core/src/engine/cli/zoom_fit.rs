//! A clip's zoom taken from the source in the same resample as its fit ([`FitOrder`]).
//!
//! Fitting the picture to the frame first and enlarging the fitted picture afterwards keeps a
//! fraction of the source's detail (a 4096x2160 clip zoomed 3.37x in a 1080x1920 frame was
//! scaled down to 1080x570 and up again), and under `Cover` the crop to the frame ran before the
//! transform, so a position revealed black where the picture goes on. The chains here are
//! the string-level checks; the `#[ignore]`d tests at the end render them with the real `ffmpeg`
//! (`cargo test -p kerf-core --no-default-features -- --ignored zoom_fit`).

use super::*;
use crate::engine::test_support::{make_clip, test_asset, video_stream};
use crate::model::{Fit, FitOrder, Hdr, Keyframe};

fn format(fit: Fit) -> ExportFormat {
    ExportFormat {
        width: 1080,
        height: 1920,
        fit,
        ..ExportFormat::default()
    }
}

fn clip(scale: f64) -> Clip {
    let asset = test_asset(vec![video_stream(4096, 2160, 30.0)]);
    let mut clip = make_clip(asset.id, 0.0, 4.0, 0.0);
    clip.transform.scale = scale;
    clip
}

/// The footage `clip()` is cut from: wide, so that under `Cover` in the vertical frame it hangs over.
const WIDE: (u32, u32) = (4096, 2160);

fn chain_with(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx) -> String {
    video_clip_chain_of(clip, fmt, fx, false, Some(WIDE), "c0")
}

fn chain(clip: &Clip, fit: Fit) -> String {
    chain_with(clip, &format(fit), &ClipFx::default())
}

/// The chain of a clip cut from footage of the size `picture` (`None`: not known).
fn chain_of(clip: &Clip, fit: Fit, picture: Option<(u32, u32)>) -> String {
    video_clip_chain_of(clip, &format(fit), &ClipFx::default(), false, picture, "c0")
}

fn key(time: f64, scale: f64, pos_x: f64) -> Keyframe {
    Keyframe {
        time,
        scale,
        pos_x,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    }
}

fn scales(chain: &str) -> usize {
    chain.split(',').filter(|f| f.starts_with("scale=")).count()
}

#[test]
fn the_fit_scale_of_a_zoom_is_the_frame_times_the_zoom() {
    assert_eq!(
        fit_scale(1080, 1920, 3.3713, "decrease", false, ""),
        "scale=1080*3.3713:1920*3.3713:force_original_aspect_ratio=decrease"
    );
    assert_eq!(
        fit_scale(1080, 1920, 2.0, "increase", false, ":flags=lanczos"),
        "scale=1080*2:1920*2:force_original_aspect_ratio=increase:flags=lanczos"
    );
    // No zoom is the frame's own box.
    assert_eq!(
        fit_scale(1080, 1920, 1.0, "increase", false, ""),
        "scale=1080:1920:force_original_aspect_ratio=increase"
    );
    // Ahead of a tone-map the box is made even, and the result too.
    assert_eq!(
        fit_scale(1080, 1920, 1.5, "decrease", true, ""),
        "scale=w='max(2,2*trunc((1080*1.5)/2))':h='max(2,2*trunc((1920*1.5)/2))':force_original_aspect_ratio=decrease:force_divisible_by=2"
    );
    assert_eq!(
        fit_scale(1080, 1920, 1.0, "increase", true, ""),
        "scale=1080:1920:force_original_aspect_ratio=increase:force_divisible_by=2"
    );
    // A zoom out to a fraction of a pixel keeps a pixel; the sizes would be 0, which `scale` reads as "unset".
    assert_eq!(
        fit_scale(1080, 1920, 0.0004, "increase", false, ""),
        "scale=w='max(1,1080*0.0004)':h='max(1,1920*0.0004)':force_original_aspect_ratio=increase"
    );
}

#[test]
fn a_zoom_in_under_contain_is_one_scale_from_the_source() {
    let zoomed = clip(3.3713);
    assert_eq!(zoomed.fit_order(Fit::Contain, true), FitOrder::Folded);
    let c = chain(&zoomed, Fit::Contain);
    assert!(
        c.contains("scale=1080*3.3713:1920*3.3713:force_original_aspect_ratio=decrease,setsar=1"),
        "{c}"
    );
    assert_eq!(scales(&c), 1, "the picture is resampled once: {c}");
    assert!(!c.contains("iw*3.3713"), "{c}");
    assert!(!c.contains("pad="), "a transformed clip is centred by the overlay: {c}");
    // The delivery's scaler rides on the one scale.
    let lanczos = ExportFormat {
        scaler: Some("lanczos".into()),
        ..format(Fit::Contain)
    };
    let c = chain_with(&zoomed, &lanczos, &ClipFx::default());
    assert!(
        c.contains("scale=1080*3.3713:1920*3.3713:force_original_aspect_ratio=decrease:flags=lanczos,setsar=1"),
        "{c}"
    );
    // A blurred fill is Contain's chain for a clip that is not the plain fit.
    assert_eq!(zoomed.fit_order(Fit::Blur, true), FitOrder::Folded);
    let c = chain(&zoomed, Fit::Blur);
    assert!(
        c.contains("scale=1080*3.3713:1920*3.3713:force_original_aspect_ratio=decrease,setsar=1"),
        "{c}"
    );
}

#[test]
fn a_zoom_in_ahead_of_a_tone_map_is_even_in_the_same_scale() {
    let hlg = ClipFx {
        hdr: Some(Hdr::Hlg),
        ..ClipFx::default()
    };
    let c = chain_with(&clip(1.5), &format(Fit::Contain), &hlg);
    assert!(
        c.contains("scale=w='max(2,2*trunc((1080*1.5)/2))':h='max(2,2*trunc((1920*1.5)/2))':force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1"),
        "{c}"
    );
    assert_eq!(scales(&c), 1, "{c}");
    let tone = c.find("zscale").or_else(|| c.find("colorspace")).unwrap();
    assert!(c.find("force_divisible_by").unwrap() < tone, "{c}");
}

#[test]
fn what_is_not_a_zoom_in_keeps_the_chain_it_had() {
    // The identity transform: the fit and the pad.
    let c = chain(&clip(1.0), Fit::Contain);
    assert!(
        c.contains("scale=1080:1920:force_original_aspect_ratio=decrease,pad=1080:1920:(ow-iw)/2:(oh-ih)/2"),
        "{c}"
    );
    // A zoom out: the fit, then the shrink.
    let shrunk = clip(0.5);
    assert_eq!(shrunk.fit_order(Fit::Contain, true), FitOrder::Fitted);
    let c = chain(&shrunk, Fit::Contain);
    assert!(
        c.contains("scale=1080:1920:force_original_aspect_ratio=decrease,scale=iw*0.5:ih*0.5,setsar=1"),
        "{c}"
    );
    // A move that does not zoom: the fit alone, centred by the overlay.
    let mut moved = clip(1.0);
    moved.transform.pos_x = 0.2;
    assert_eq!(moved.fit_order(Fit::Contain, true), FitOrder::Fitted);
    let c = chain(&moved, Fit::Contain);
    assert!(
        c.contains("scale=1080:1920:force_original_aspect_ratio=decrease,setsar=1"),
        "{c}"
    );
    // A zoom that moves is the chain's last stage, at the fit size.
    let mut moving = clip(1.0);
    moving.keyframes = vec![key(0.0, 0.5, 0.0), key(2.0, 3.0, 0.0)];
    assert_eq!(moving.fit_order(Fit::Contain, true), FitOrder::Fitted);
    let c = chain(&moving, Fit::Contain);
    assert!(
        c.contains("scale=1080:1920:force_original_aspect_ratio=decrease,setsar=1"),
        "{c}"
    );
    assert!(c.contains("eval=frame"), "{c}");
}

#[test]
fn a_zoom_that_is_keyed_but_held_is_the_same_scale() {
    let mut held = clip(1.0);
    held.keyframes = vec![key(0.0, 2.0, 0.0), key(2.0, 2.0, 0.1)];
    assert_eq!(held.held_scale(), Some(2.0));
    assert_eq!(held.fit_order(Fit::Contain, true), FitOrder::Folded);
    let c = chain(&held, Fit::Contain);
    assert!(
        c.contains("scale=1080*2:1920*2:force_original_aspect_ratio=decrease,setsar=1") && !c.contains("eval=frame"),
        "{c}"
    );
    // Held at or under 1 it keeps the per-frame scale it always had.
    let mut under = clip(1.0);
    under.keyframes = vec![key(0.0, 0.8, 0.0), key(2.0, 0.8, 0.1)];
    assert_eq!(under.fit_order(Fit::Contain, true), FitOrder::Fitted);
    assert!(chain(&under, Fit::Contain).contains("eval=frame"));
}

#[test]
fn a_cover_clip_that_pans_is_scaled_to_cover_and_the_overlay_cuts_it() {
    let mut panned = clip(1.0);
    panned.transform.pos_x = 0.3;
    assert_eq!(panned.fit_order(Fit::Cover, true), FitOrder::Uncropped);
    let c = chain(&panned, Fit::Cover);
    // The cover of the frame, not cropped to it; the window kept is the half frame plus the pan on
    // each side of the centre (1080 * 0.8 * 2 = 1728) and a margin of 8 px: 1744 wide, 1936 tall.
    assert!(
        c.contains(
            "scale=1080:1920:force_original_aspect_ratio=increase,crop=w='iw-4*floor(max(0,iw-1744)/4)':h='ih-4*floor(max(0,ih-1936)/4)',setsar=1"
        ),
        "{c}"
    );
    assert!(!c.contains("crop=1080:1920"), "{c}");
    assert_eq!(scales(&c), 1, "{c}");
    // A zoom is the same scale, whatever the side of 1.
    for (scale, text) in [(2.0, "1080*2:1920*2"), (0.5, "1080*0.5:1920*0.5")] {
        let mut zoomed = clip(scale);
        zoomed.transform.pos_y = -0.1;
        assert_eq!(zoomed.fit_order(Fit::Cover, true), FitOrder::Uncropped);
        let c = chain(&zoomed, Fit::Cover);
        assert!(
            c.contains(&format!(
                "scale={text}:force_original_aspect_ratio=increase,crop=w='iw-4*floor("
            )),
            "{c}"
        );
        assert_eq!(scales(&c), 1, "{c}");
    }
    // A position that is keyed pans as far as its curve goes (0.4), and the zoom may be held keyed.
    let mut keyed = clip(1.0);
    keyed.keyframes = vec![key(0.0, 1.5, -0.4), key(2.0, 1.5, 0.2)];
    assert_eq!(keyed.fit_order(Fit::Cover, true), FitOrder::Uncropped);
    assert_eq!(keyed.pan_extent(), (0.4, 0.0));
    let c = chain(&keyed, Fit::Cover);
    assert!(
        c.contains("scale=1080*1.5:1920*1.5:force_original_aspect_ratio=increase,crop=w='iw-4*floor(max(0,iw-1960)/4)'"),
        "{c}"
    );
    assert!(!c.contains("eval=frame"), "{c}");
}

/// A slide or push brings the clip in from a frame away: the window it keeps has to hold what the
/// canvas shows of it all the way (the travel adds to the position's own reach).
#[test]
fn the_window_of_a_cover_clip_that_slides_in_holds_the_whole_travel() {
    let mut zoomed = clip(1.5);
    zoomed.transform.pos_y = 0.1;
    let sliding = ClipFx {
        // From a frame to the left, over half a second.
        move_in: Some((-1.0, 0.0, 0.5)),
        ..ClipFx::default()
    };
    let c = chain_with(&zoomed, &format(Fit::Cover), &sliding);
    // 1080 * (0.5 + 1) * 2 + 16 across; 1920 * (0.5 + 0.1) * 2 + 16 down.
    assert!(
        c.contains("crop=w='iw-4*floor(max(0,iw-3256)/4)':h='ih-4*floor(max(0,ih-2320)/4)'"),
        "{c}"
    );
    let still = chain_with(&zoomed, &format(Fit::Cover), &ClipFx::default());
    assert!(still.contains("max(0,iw-1096)"), "{still}");
}

#[test]
fn what_a_cover_clip_shows_of_the_frame_alone_is_cropped_to_it_as_it_was() {
    let crops = "scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920,setsar=1";
    // The identity.
    assert!(chain(&clip(1.0), Fit::Cover).contains(crops));
    // Neither zoom nor position: a turn, a fade of opacity, a crop of the source.
    let mut turned = clip(1.0);
    turned.transform.rotation = 15.0;
    let mut faint = clip(1.0);
    faint.transform.opacity = 0.5;
    let mut trimmed = clip(1.0);
    trimmed.transform.crop_left = 0.1;
    for c in [turned, faint, trimmed] {
        assert_eq!(c.fit_order(Fit::Cover, true), FitOrder::Fitted);
        assert!(chain(&c, Fit::Cover).contains(crops), "{}", chain(&c, Fit::Cover));
    }
    // A mask and a vignette are laid out over the frame the chain hands them: they keep cropping
    // first, a pan or a zoom or not.
    let mut masked = clip(1.0);
    masked.transform.pos_x = 0.3;
    masked.mask = Some(crate::model::Mask::default());
    let mut vignetted = clip(2.0);
    vignetted.effects = vec![crate::model::VideoEffect::Vignette];
    for c in [masked, vignetted] {
        assert_eq!(c.fit_order(Fit::Cover, true), FitOrder::Fitted);
        let c = chain(&c, Fit::Cover);
        assert!(c.contains("crop=1080:1920,") && !c.contains("floor("), "{c}");
    }
    // A zoom that moves is the chain's last stage, at the cover-and-cropped fit size.
    let mut moving = clip(1.0);
    moving.keyframes = vec![key(0.0, 0.5, 0.2), key(2.0, 2.0, 0.2)];
    assert_eq!(moving.fit_order(Fit::Cover, true), FitOrder::Fitted);
    let c = chain(&moving, Fit::Cover);
    assert!(c.contains(crops) && c.contains("eval=frame"), "{c}");
}

/// Under Cover a picture of the frame's own shape has nothing past the frame to reveal, so a move or
/// a zoom out changes nothing a crop to the frame hid: those keep the chain they had (a webcam shrunk
/// over a 16:9 cut in a 16:9 frame is the common one), and only a zoom in, which is detail, is taken
/// from the source in one scale.
#[test]
fn a_cover_clip_of_the_frames_own_shape_keeps_its_chain_unless_it_zooms_in() {
    let tall = Some((2160, 3840));
    let crops = "scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920,";
    let mut pip = clip(0.25);
    pip.transform.pos_x = 0.35;
    pip.transform.pos_y = 0.3;
    let c = chain_of(&pip, Fit::Cover, tall);
    assert!(c.contains(&format!("{crops}scale=iw*0.25:ih*0.25,setsar=1")), "{c}");
    let mut moved = clip(1.0);
    moved.transform.pos_x = 0.3;
    assert!(chain_of(&moved, Fit::Cover, tall).contains(&format!("{crops}setsar=1")));
    // ... but a footage of another shape, or of a size not known, hangs over the frame.
    for picture in [Some(WIDE), None] {
        assert!(chain_of(&pip, Fit::Cover, picture).contains("floor("), "{picture:?}");
        assert!(chain_of(&moved, Fit::Cover, picture).contains("floor("), "{picture:?}");
    }
    // A zoom in is the frame's cover times the zoom, whatever the shape, and the centred crop keeps the
    // picture to the frame it can show.
    let zoomed = clip(1.5);
    let c = chain_of(&zoomed, Fit::Cover, tall);
    assert!(
        c.contains("scale=1080*1.5:1920*1.5:force_original_aspect_ratio=increase,crop=w='iw-4*floor(max(0,iw-1096)/4)'"),
        "{c}"
    );
    // A reprojected picture comes out at the frame's size: a move of it is cropped as it was.
    let mut reframed = clip(1.0);
    reframed.transform.pos_x = 0.3;
    reframed.reframe = Some(crate::model::Reframe::new(crate::model::Projection::Equirect));
    let c = chain_of(&reframed, Fit::Cover, Some(WIDE));
    assert!(c.contains(&format!("{crops}setsar=1")), "{c}");
}

/// The keyed-zoom harness's alpha case: 4:3 footage (160x120) in a 16:9 frame (320x180) under
/// Cover, held at 0.8 and moved 0.1 right. The cover is 320x240, so a zoom out of 0.8 is 256x192:
/// not the cropped frame zoomed (256x144), which is what a crop ahead of the zoom made of it, but
/// the whole cover, which reaches the top and bottom of the frame and which the overlay cuts there.
#[test]
fn a_zoom_out_of_a_cover_clip_of_another_shape_is_the_whole_cover_zoomed() {
    let fmt = ExportFormat {
        width: 320,
        height: 180,
        fit: Fit::Cover,
        ..ExportFormat::default()
    };
    let asset = test_asset(vec![video_stream(160, 120, 30.0)]);
    let mut zoomed_out = make_clip(asset.id, 0.0, 4.0, 0.0);
    zoomed_out.transform.scale = 0.8;
    zoomed_out.transform.pos_x = 0.1;
    let chain = |clip: &Clip, picture| video_clip_chain_of(clip, &fmt, &ClipFx::default(), false, picture, "c0");
    assert!(crate::layer_geometry::cover_overflows(
        Some((160, 120)),
        (320, 180),
        &zoomed_out.transform
    ));
    assert_eq!(zoomed_out.fit_order(Fit::Cover, true), FitOrder::Uncropped);
    // One resample to the cover times the zoom, no crop to the frame; the window kept is the half
    // frame plus the pan on each side of the centre (320 * 0.6 * 2 = 384, 180 * 0.5 * 2 = 180)
    // and a margin of 8 px.
    let c = chain(&zoomed_out, Some((160, 120)));
    assert!(
        c.contains(
            "scale=320*0.8:180*0.8:force_original_aspect_ratio=increase,crop=w='iw-4*floor(max(0,iw-400)/4)':h='ih-4*floor(max(0,ih-196)/4)',setsar=1"
        ),
        "{c}"
    );
    assert!(!c.contains("crop=320:180"), "{c}");
    assert_eq!(scales(&c), 1, "{c}");
    // The same clip held by a moving zoom is the chain it always had: the cover cropped to the frame,
    // then the zoom, so the picture is the frame zoomed (256x144).
    let mut moving = zoomed_out.clone();
    moving.keyframes = vec![key(0.0, 0.8, 0.1), key(2.0, 1.2, 0.1)];
    assert_eq!(moving.fit_order(Fit::Cover, true), FitOrder::Fitted);
    let c = chain(&moving, Some((160, 120)));
    assert!(
        c.contains("scale=320:180:force_original_aspect_ratio=increase,crop=320:180,") && !c.contains("floor("),
        "{c}"
    );
    // Footage of the frame's own shape has no overhang: it is cropped to the frame as it was.
    let c = chain(&zoomed_out, Some((320, 180)));
    assert!(
        c.contains("scale=320:180:force_original_aspect_ratio=increase,crop=320:180,scale=iw*0.8:ih*0.8,setsar=1"),
        "{c}"
    );
}

#[test]
fn the_window_kept_of_an_uncropped_picture_follows_the_pan_the_turn_and_the_blur() {
    let crop = |pan, rotation, margin| bound_crop((1080, 1920), pan, rotation, margin);
    let text = |w: u32, h: u32| format!("crop=w='iw-4*floor(max(0,iw-{w})/4)':h='ih-4*floor(max(0,ih-{h})/4)'");
    // The frame and the margin on each side of the centre.
    assert_eq!(crop((0.0, 0.0), Rotated::No, 8.0), text(1096, 1936));
    // A pan adds to the reach on the side it goes from, as far as it ever goes.
    assert_eq!(crop((0.3, 0.1), Rotated::No, 8.0), text(1744, 2320));
    // A quarter turn swaps the axes; a turn that is keyed is the circle round the frame.
    assert_eq!(
        crop((0.0, 0.0), Rotated::By(std::f64::consts::FRAC_PI_2), 8.0),
        text(1936, 1096)
    );
    let circle = (540.0_f64.hypot(960.0) * 2.0 + 16.0).ceil() as u32;
    assert_eq!(crop((0.0, 0.0), Rotated::Keyed, 8.0), text(circle, circle));
    // A blur reads about three sigma past a pixel.
    assert_eq!(crop((0.0, 0.0), Rotated::No, 8.0 + 3.0 * 10.0), text(1156, 1996));
    // A pan that is no number keeps the picture whole rather than cutting it to nothing.
    assert_eq!(
        crop((f64::NAN, f64::INFINITY), Rotated::No, 8.0),
        text(1_000_000_000, 1_000_000_000)
    );
}

#[test]
fn an_uncropped_hdr_picture_is_even_where_the_tone_map_reads_it() {
    let hlg = ClipFx {
        hdr: Some(Hdr::Hlg),
        ..ClipFx::default()
    };
    let mut panned = clip(1.0);
    panned.transform.pos_x = 0.3;
    let c = chain_with(&panned, &format(Fit::Cover), &hlg);
    assert!(
        c.contains("scale=1080:1920:force_original_aspect_ratio=increase:force_divisible_by=2,crop=w='iw-4*floor("),
        "{c}"
    );
    let mut zoomed = clip(1.5);
    zoomed.transform.pos_x = 0.3;
    let c = chain_with(&zoomed, &format(Fit::Cover), &hlg);
    assert!(
        c.contains(
            "scale=w='max(2,2*trunc((1080*1.5)/2))':h='max(2,2*trunc((1920*1.5)/2))':force_original_aspect_ratio=increase:force_divisible_by=2,crop="
        ),
        "{c}"
    );
    let tone = c.find("zscale").or_else(|| c.find("colorspace")).unwrap();
    assert!(c.find("floor(").unwrap() < tone, "{c}");
}

// ---- rendered (`#[ignore]`d: they drive the real ffmpeg) --------------------------------------------

use std::path::Path;
use std::process::Stdio;

use super::rendered::scratch;
use crate::engine::test_support::{single, StatusBounded};
use crate::model::Delivery;

const FW: u32 = 1080;
const FH: u32 = 1920;

/// A 4096x2160 picture of fine detail: a grid of sines about 8 pixels across, in 4:2:0 and lossless.
/// Fitted to a 1080-wide frame first it is below the fitted picture's resolution (2 px across, past
/// what a scaler keeps), so the detail only survives a zoom that is taken from this file.
fn fine(dir: &Path) -> Asset {
    let path = dir.join("fine.mp4");
    if !path.exists() {
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(
                "nullsrc=s=4096x2160:r=5:d=0.6,format=yuv420p,\
                 geq=lum='128+100*sin(X*0.8)*sin(Y*0.8)':cb=128:cr=128",
            )
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "0", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
    }
    let mut asset = test_asset(vec![video_stream(4096, 2160, 5.0)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 0.6;
    asset
}

/// A 4096x2160 picture that changes slowly (features a few hundred pixels across): two frames of it
/// that differ by a fraction of a pixel are alike to 40 dB and more, where the grid of `fine` is not.
fn broad(dir: &Path) -> Asset {
    let path = dir.join("broad.mp4");
    if !path.exists() {
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(
                "nullsrc=s=4096x2160:r=5:d=0.6,format=yuv420p,\
                 geq=lum='128+80*sin(X*0.006)*sin(Y*0.007)+30*sin((X+Y)*0.02)':cb='128+40*sin(X*0.003)':cr='128+40*sin(Y*0.003)'",
            )
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "0", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
    }
    let mut asset = test_asset(vec![video_stream(4096, 2160, 5.0)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 0.6;
    asset
}

/// The luma-ish mean of (R, G, B) of a pixel of an `FW`x`FH` rgb24 frame.
fn luma(frame: &[u8], x: u32, y: u32) -> f64 {
    let at = ((y * FW + x) * 3) as usize;
    frame[at..at + 3].iter().map(|v| f64::from(*v)).sum::<f64>() / 3.0
}

/// The standard deviation of the luma of a window of the frame: what is left of a detail of amplitude
/// 100 that has been resampled.
fn spread(frame: &[u8], rows: std::ops::Range<u32>, cols: std::ops::Range<u32>) -> f64 {
    let values: Vec<f64> = rows
        .flat_map(|y| cols.clone().map(move |x| (x, y)))
        .map(|(x, y)| luma(frame, x, y))
        .collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / values.len() as f64).sqrt()
}

/// The first frame of the export graph of `tl` at the delivery size, its graph passed through `edit` first.
fn first_frame(tl: &Timeline, assets: &[Asset], dir: &Path, tag: &str, edit: impl Fn(&str) -> String) -> Vec<u8> {
    let opts = ExportOptions {
        resolution: Some((FW, FH)),
        fps: Some(5.0),
        ..ExportOptions::default()
    };
    let mut args = build_export_args(tl, assets, "unused.mkv", &opts).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args[graph] = edit(&args[graph]);
    args.truncate(graph + 1);
    let out = dir.join(format!("{tag}.rgb"));
    args.extend(
        [
            "-map",
            "[outv]",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-y",
        ]
        .map(String::from),
    );
    args.push(out.to_string_lossy().into_owned());
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    let raw = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    assert_eq!(raw.len(), (FW * FH * 3) as usize);
    raw
}

/// The rows (top, bottom) of the frame that hold the picture, found by the bars of black above and below.
fn picture_rows(frame: &[u8]) -> (u32, u32) {
    let lit = |y: u32| (0..FW).step_by(7).any(|x| luma(frame, x, y) > 12.0);
    ((0..FH).find(|&y| lit(y)).unwrap(), (0..FH).rev().find(|&y| lit(y)).unwrap())
}

/// The scrubbed still of `tl` at `t`, at the delivery size, as raw rgb24.
fn still_frame(tl: &Timeline, assets: &[Asset], t: f64) -> Vec<u8> {
    let opts = ExportOptions {
        resolution: Some((FW, FH)),
        fps: Some(5.0),
        ..ExportOptions::default()
    };
    let args = build_still_args(tl, assets, &opts, t, FW, None, &StillOutput::RgbPipe).unwrap();
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout.len(), (FW * FH * 3) as usize);
    run.stdout
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let d = f64::from(*x) - f64::from(*y);
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

/// A 4096x2160 clip zoomed 3x in a 1080x1920 frame under Contain keeps the source's detail: its picture
/// is the source scaled once, where fitting it to the frame first (to 1080x570) and enlarging that
/// 3x kept next to none of it. The picture is also where it was, to a few pixels.
#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn a_zoom_in_under_contain_keeps_the_detail_the_fitted_picture_lost() {
    let dir = scratch("zoom-fit-contain");
    let asset = fine(&dir);
    let assets = vec![asset.clone()];
    let mut c = make_clip(asset.id, 0.0, 0.4, 0.0);
    c.transform.scale = 3.0;
    let mut tl = single(vec![c]);
    tl.format = Some(Delivery::new(FW, FH, Fit::Contain));

    let new = first_frame(&tl, &assets, &dir, "new", |g| {
        assert!(g.contains("scale=1080*3:1920*3:force_original_aspect_ratio=decrease"), "{g}");
        g.to_string()
    });
    // The order the graph had: fit to the frame, then enlarge the fitted picture.
    let old = first_frame(&tl, &assets, &dir, "old", |g| {
        g.replace(
            "scale=1080*3:1920*3:force_original_aspect_ratio=decrease",
            "scale=1080:1920:force_original_aspect_ratio=decrease,scale=iw*3:ih*3",
        )
    });
    // The picture the source gives at the zoom, taken once and laid out by hand: 3240x1708, its middle
    // 1080 columns, 104 rows down.
    let reference = first_frame(&tl, &assets, &dir, "reference", |_| {
        "color=c=black:s=1080x1920:r=5:d=1,format=yuv420p[vbase];\
         [0:v]trim=start=0:end=0.4,setpts=PTS-STARTPTS,scale=3240:1709,crop=1080:1709:1080:0:exact=1,\
         setsar=1,fps=5,format=yuv420p[v0];[vbase][v0]overlay=x=0:y=104[outv]"
            .to_string()
    });

    let (top, bottom) = picture_rows(&new);
    let (old_top, old_bottom) = picture_rows(&old);
    eprintln!("picture rows: new {top}..{bottom}, old {old_top}..{old_bottom}");
    assert!(
        top.abs_diff(old_top) <= 2 && bottom.abs_diff(old_bottom) <= 2,
        "the picture is where it was"
    );
    assert!(top.abs_diff(104) <= 2 && bottom.abs_diff(1812) <= 2, "{top}..{bottom}");

    let window = (400..1500, 100..980);
    let (kept, lost, truth) = (
        spread(&new, window.0.clone(), window.1.clone()),
        spread(&old, window.0.clone(), window.1.clone()),
        spread(&reference, window.0, window.1),
    );
    eprintln!("detail (std-dev of luma): one resample {kept:.1}, the source once {truth:.1}, fitted then enlarged {lost:.1}");
    assert!(truth > 30.0, "the pattern survives a single resample: {truth}");
    assert!(kept > 0.9 * truth, "the zoom is the source's detail: {kept} of {truth}");
    assert!(lost < 0.3 * truth, "fitting first lost it: {lost} of {truth}");
    let (to_new, to_old) = (psnr(&new, &reference), psnr(&old, &reference));
    eprintln!("PSNR against the source scaled once: new {to_new:.1} dB, old {to_old:.1} dB");
    assert!(to_new > 45.0, "the source scaled once, to the pixel: {to_new}");
    assert!(to_new > to_old + 10.0, "well above the old path: {to_new} vs {to_old}");
    // The scrubbed still of the same moment draws the same picture.
    let still = still_frame(&tl, &assets, 0.1);
    let to_still = psnr(&still, &new);
    eprintln!("PSNR of the still against the export: {to_still:.1} dB");
    assert!(to_still > 45.0, "the still is the export's frame: {to_still}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The columns (from, to) of the frame, in the rows `rows`, whose pixels are black: the bars a panned
/// picture leaves where it does not reach.
fn black_columns(frame: &[u8], rows: std::ops::Range<u32>) -> u32 {
    (0..FW).filter(|&x| rows.clone().all(|y| luma(frame, x, y) < 12.0)).count() as u32
}

/// A 16:9 clip in a vertical frame under Cover, moved sideways: the picture goes on past the frame, so
/// the frame shows more of it — where cropping to the frame first left a black bar of the move's width.
/// With a zoom as well, the frame is a window of the source scaled once, and it is the window the
/// position says (to the pixel: the same scale and crop done by hand).
#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn a_cover_clip_that_pans_shows_the_picture_past_the_frame_not_black() {
    let dir = scratch("zoom-fit-cover");
    let asset = fine(&dir);
    let assets = vec![asset.clone()];
    let cut = |scale: f64, pos: (f64, f64)| {
        let mut c = make_clip(asset.id, 0.0, 0.4, 0.0);
        c.transform.scale = scale;
        c.transform.pos_x = pos.0;
        c.transform.pos_y = pos.1;
        let mut tl = single(vec![c]);
        tl.format = Some(Delivery::new(FW, FH, Fit::Cover));
        tl
    };
    // The picture by hand: the source scaled to `w`x`h` once, the window of the frame cut from it at
    // (x, y), black where it does not reach (nowhere in these cases).
    let by_hand = |w: u32, h: u32, x: u32, y: u32| {
        format!(
            "color=c=black:s=1080x1920:r=5:d=1,format=yuv420p[vbase];\
             [0:v]trim=start=0:end=0.4,setpts=PTS-STARTPTS,scale={w}:{h},crop=1080:1920:{x}:{y},\
             setsar=1,fps=5,format=yuv420p[v0];[vbase][v0]overlay=x=0:y=0[outv]"
        )
    };

    // A move of 0.3 of the frame to the right, no zoom: the cover is 3641x1920, centred at -1280.5 and
    // moved 324, so the frame's left edge is the picture's column 956.
    let tl = cut(1.0, (0.3, 0.0));
    let new = first_frame(&tl, &assets, &dir, "pan", |g| {
        assert!(g.contains("force_original_aspect_ratio=increase,crop=w='iw-4*floor("), "{g}");
        g.to_string()
    });
    let old = first_frame(&tl, &assets, &dir, "pan-old", |g| {
        let at = g
            .find("scale=1080:1920:force_original_aspect_ratio=increase,crop=w='")
            .unwrap();
        let to = g[at..].find(",setsar=1").unwrap() + at;
        format!(
            "{}scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920{}",
            &g[..at],
            &g[to..]
        )
    });
    let truth = first_frame(&tl, &assets, &dir, "pan-reference", |_| by_hand(3641, 1920, 956, 0));
    let (black_new, black_old) = (black_columns(&new, 0..FH), black_columns(&old, 0..FH));
    eprintln!("black columns: {black_new} (picture across the frame), {black_old} (cropped to the frame first)");
    assert_eq!(black_new, 0, "the picture is there to the edge");
    assert!(black_old >= 320, "the bug: a bar as wide as the move: {black_old}");
    let (to_new, to_old) = (psnr(&new, &truth), psnr(&old, &truth));
    eprintln!("PSNR against the picture by hand: new {to_new:.1} dB, old {to_old:.1} dB");
    assert!(to_new > 45.0 && to_new > to_old + 10.0, "{to_new} vs {to_old}");

    let first_pan = new;

    // Zoomed 2x and moved right and up: the cover of the frame times 2 is 7282x3840 (centred, then moved
    // 216 right and 192 up), cut to the window the move leaves at (2886, 1152).
    let tl = cut(2.0, (0.2, -0.1));
    let new = first_frame(&tl, &assets, &dir, "zoom-pan", str::to_string);
    let truth = first_frame(&tl, &assets, &dir, "zoom-pan-reference", |_| by_hand(7282, 3840, 2886, 1152));
    let old = first_frame(&tl, &assets, &dir, "zoom-pan-old", |g| {
        let at = g
            .find("scale=1080*2:1920*2:force_original_aspect_ratio=increase,crop=w='")
            .unwrap();
        let to = g[at..].find(",setsar=1").unwrap() + at;
        format!(
            "{}scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920,scale=iw*2:ih*2{}",
            &g[..at],
            &g[to..]
        )
    });
    let (to_new, to_old) = (psnr(&new, &truth), psnr(&old, &truth));
    eprintln!("zoomed and moved: PSNR against the picture by hand: new {to_new:.1} dB, old {to_old:.1} dB");
    assert_eq!(black_columns(&new, 0..FH), 0);
    assert!(to_new > 45.0 && to_new > to_old + 10.0, "{to_new} vs {to_old}");
    // The scrubbed still of the same moment, for both poses, is the export's frame.
    for (tag, scale, pos, export) in [("pan", 1.0, (0.3, 0.0), &first_pan), ("zoom and pan", 2.0, (0.2, -0.1), &new)] {
        let still = still_frame(&cut(scale, pos), &assets, 0.1);
        let to_export = psnr(&still, export);
        eprintln!("PSNR of the still against the export, {tag}: {to_export:.1} dB");
        assert!(to_export > 45.0, "{tag}: the still is the export's frame: {to_export}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `graph` with the export's centred bounding `crop` taken out: the cover of the frame times the zoom,
/// whole, as the overlay would be handed it without the bound.
fn without_the_bound(graph: &str) -> String {
    let at = graph.find(",crop=w='iw-4*floor(").expect("a bounded picture");
    let end = at + graph[at..].find(")/4)'").unwrap() + 5;
    let end = end + graph[end..].find(")/4)'").unwrap() + 5;
    format!("{}{}", &graph[..at], &graph[end..])
}

/// Whether the corners of the frame (a block of 8x8 pixels at each) are picture, not black.
fn corners_are_picture(frame: &[u8]) -> bool {
    [(0, 0), (FW - 8, 0), (0, FH - 8), (FW - 8, FH - 8)]
        .iter()
        .all(|&(x0, y0)| (0..8).all(|dy| (0..8).all(|dx| luma(frame, x0 + dx, y0 + dy) > 12.0)))
}

/// The centred window the export keeps of an uncropped picture is what any frame can show, **turned
/// with the picture**: a Cover clip that zooms, moves and turns (a static angle, and the keyed one's
/// circle) draws the same frame as the whole cover of the frame would, to the pixel, and the frame is
/// picture to its corners.
#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn the_bounded_picture_of_a_turning_cover_clip_draws_what_the_whole_cover_would() {
    let dir = scratch("zoom-fit-turn");
    let asset = broad(&dir);
    let assets = vec![asset.clone()];
    let turned = |edit: &dyn Fn(&mut Clip)| {
        let mut c = make_clip(asset.id, 0.0, 0.4, 0.0);
        c.transform.scale = 1.2;
        c.transform.pos_x = 0.1;
        c.transform.pos_y = -0.02;
        edit(&mut c);
        let mut tl = single(vec![c]);
        tl.format = Some(Delivery::new(FW, FH, Fit::Cover));
        tl
    };
    let key = |time: f64, rotation: f64| Keyframe {
        time,
        scale: 1.2,
        pos_x: 0.1,
        pos_y: -0.02,
        rotation,
        opacity: 1.0,
        easing: Default::default(),
    };
    for (name, tl) in [
        ("static 8 degrees", turned(&|c| c.transform.rotation = 8.0)),
        ("static 90 degrees", turned(&|c| c.transform.rotation = 90.0)),
        ("keyed", turned(&|c| c.keyframes = vec![key(0.0, 0.0), key(0.4, 10.0)])),
    ] {
        let bounded = first_frame(&tl, &assets, &dir, name, str::to_string);
        let whole = first_frame(&tl, &assets, &dir, &format!("{name}-whole"), without_the_bound);
        // The two differ by the half pixel the rotated box's parity moves the centre by (`rotw` of a
        // picture that is not the same size), which a slowly changing picture shows at 40 dB and more.
        let same = psnr(&bounded, &whole);
        eprintln!(
            "{name}: bounded against whole: PSNR {same:.1} dB; corners are picture: {}",
            corners_are_picture(&whole)
        );
        assert!(same > 40.0, "{name}: the bound cost more than a pixel of the frame: {same}");
        if name == "static 8 degrees" {
            assert!(corners_are_picture(&bounded), "{name}: the corners are the picture's");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
