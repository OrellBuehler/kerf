use super::*;

fn picture(w: u32, h: u32, pix_fmt: &str, tag: Option<&str>) -> StreamInfo {
    StreamInfo {
        index: 0,
        kind: StreamKind::Video,
        codec: "h264".into(),
        width: Some(w),
        height: Some(h),
        fps: Some(30.0),
        sample_rate: None,
        channels: None,
        image: false,
        projection: None,
        rotation: 0,
        color_transfer: None,
        color_primaries: None,
        pix_fmt: Some(pix_fmt.into()),
        color_space: tag.map(Into::into),
        color_range: None,
        lead: 0.0,
        time_base: None,
    }
}

#[test]
fn a_tag_states_a_matrix_unless_it_says_nothing() {
    for tag in [
        "bt709",
        "smpte170m",
        "bt470bg",
        "bt2020nc",
        "bt2020c",
        "smpte240m",
        "fcc",
        "ycgco",
        "gbr",
    ] {
        assert!(matrix_tag_is_stated(Some(tag)), "{tag}");
    }
    // `reserved` is a value no matrix has: FFmpeg reads it as it reads no tag.
    for tag in ["", "unknown", "unspecified", "reserved", " reserved "] {
        assert!(!matrix_tag_is_stated(Some(tag)), "{tag:?}");
    }
    assert!(!matrix_tag_is_stated(None));
}

#[test]
fn untagged_hd_is_hd_ycbcr_that_names_nothing_and_is_not_hdr() {
    let hd = |tag, fmt, w, h| untagged_hd_yuv(tag, fmt, w, h, false);
    // mpv's rule: wide enough, or tall enough.
    assert!(hd(None, "yuv420p", 1280, 720));
    assert!(hd(None, "yuv420p", 1920, 1080));
    assert!(hd(None, "yuv420p", 1024, 768));
    assert!(hd(None, "yuv420p", 720, 1280), "a portrait phone clip is taller than 576");
    assert!(hd(Some("unknown"), "yuv420p10le", 3840, 2160));
    assert!(hd(Some("reserved"), "yuv422p", 1920, 1080));
    assert!(hd(None, "nv12", 1920, 1080));
    assert!(!hd(None, "yuv420p", 720, 576), "PAL is SD");
    assert!(!hd(None, "yuv420p", 1279, 576));
    assert!(!hd(None, "yuv420p", 854, 480));
    assert!(hd(None, "yuv420p", 1280, 100), "wide enough");
    // A tag of any kind is the file's word.
    for tag in ["bt709", "smpte170m", "bt470bg", "bt2020nc"] {
        assert!(!hd(Some(tag), "yuv420p", 1920, 1080), "{tag}");
    }
    // JFIF and a webcam's MJPEG are BT.601 by definition.
    assert!(!hd(None, "yuvj420p", 1920, 1080));
    assert!(!hd(None, "yuvj444p", 1920, 1080));
    // RGB, grey, alpha, packed and unknown formats are not YCbCr on the list.
    for fmt in [
        "bgr0",
        "rgb24",
        "gbrp",
        "gray",
        "gray10le",
        "yuva420p",
        "rgba",
        "yuva444p10le",
        "pal8",
        "",
        "nonsense",
    ] {
        assert!(!hd(None, fmt, 1920, 1080), "{fmt:?}");
    }
    // HDR is tone-mapped to a tagged BT.709 picture.
    assert!(!untagged_hd_yuv(None, "yuv420p10le", 3840, 2160, true));
    // No size recorded is not HD.
    assert!(!hd(None, "yuv420p", 0, 0));
}

#[test]
fn a_stream_names_a_matrix_by_tag_or_hdr_and_is_untagged_hd_only_as_a_moving_picture() {
    assert!(picture(640, 360, "yuv420p", Some("bt709")).names_a_matrix());
    assert!(picture(1920, 1080, "yuv420p", Some("smpte170m")).names_a_matrix());
    assert!(!picture(1920, 1080, "yuv420p", None).names_a_matrix());
    assert!(!picture(1920, 1080, "yuv420p", Some("reserved")).names_a_matrix());
    let mut hlg = picture(1920, 1080, "yuv420p10le", Some("bt2020nc"));
    hlg.color_transfer = Some("arib-std-b67".into());
    assert!(hlg.names_a_matrix() && !hlg.is_untagged_hd_yuv());
    // HDR names a matrix even when the file's tag is missing: the graph tone-maps it to BT.709.
    hlg.color_space = None;
    assert!(hlg.names_a_matrix() && !hlg.is_untagged_hd_yuv());
    // An RGB picture has no YCbCr matrix to name, whatever the container writes.
    assert!(!picture(1920, 1080, "gbrp", Some("gbr")).names_a_matrix());
    assert!(!picture(1920, 1080, "bgr0", Some("gbr")).names_a_matrix());
    // …and `gbr` is nobody's matrix on any layout: a PNG with alpha or a grey one carries it too.
    assert!(!picture(1920, 1080, "rgba", Some("gbr")).names_a_matrix());
    assert!(!picture(1920, 1080, "gray", Some("gbr")).names_a_matrix());
    assert!(!picture(1920, 1080, "yuv420p", Some("gbr")).names_a_matrix());
    // The tags decoders write on their own (probed): a JPEG's, a WebP's and MJPEG video's `bt470bg`,
    // a PNG's `gbr`, RGB video's `gbr`. None names a matrix, so nothing beside them is labelled.
    let hd = picture(1920, 1080, "yuv420p", None);
    let phone = picture(1920, 1080, "yuv420p", Some("bt709"));
    for (what, shape) in crate::engine::test_support::decoder_default_pictures(1920, 1080) {
        assert!(!shape.names_a_matrix(), "{what}");
        assert_eq!(bt709_labels(&[Some(&hd), Some(&shape)]), [false, false], "{what}");
        assert_eq!(bt709_labels(&[Some(&shape), Some(&hd)]), [false, false], "{what}");
        assert_eq!(
            bt709_labels(&[Some(&phone), Some(&shape), Some(&hd)]),
            [false, false, true],
            "{what}"
        );
    }
    // The same tag on footage that is not the decoder's default is the file's word: an H.264 or
    // ProRes clip tagged `bt470bg` names it (and so does SD tagged `smpte170m`).
    assert!(picture(1920, 1080, "yuv420p", Some("bt470bg")).names_a_matrix());
    assert!(picture(1920, 1080, "yuv422p10le", Some("bt470bg")).names_a_matrix());
    // …but not on a still image, nor from a decoder that tags everything it decodes (`DEFAULT_TAG_CODECS`),
    // whatever the pixel format: `yuv422p` MJPEG is no more the file's word than `yuvj420p`, and a
    // JPEG-LS grey picture (not RGB, so the layout rule does not catch it) is not either.
    let mut jpeg = picture(1920, 1080, "yuv420p", Some("bt470bg"));
    jpeg.image = true;
    assert!(!jpeg.names_a_matrix());
    for (codec, fmt) in [
        ("mjpeg", "yuv422p"),
        ("mjpeg", "yuvj422p"),
        ("amv", "yuvj420p"),
        ("speedhq", "yuv422p"),
        ("jpegls", "gray"),
        ("jpegls", "gray16le"),
        ("jpegls", "yuv420p"),
        ("mjpegb", "yuvj420p"),
        ("sp5x", "yuvj420p"),
        ("thp", "yuvj420p"),
        ("smvjpeg", "yuvj420p"),
        ("mxpeg", "yuvj420p"),
    ] {
        let mut s = picture(1920, 1080, fmt, Some("bt470bg"));
        s.codec = codec.into();
        assert!(!s.names_a_matrix(), "{codec} {fmt}");
        s.codec = codec.to_ascii_uppercase();
        assert!(!s.names_a_matrix(), "{codec} {fmt}: the codec is matched whatever its case");
    }
    // The pixel format is not the decoder: FFmpeg's h264 decoder reports every full-range stream as
    // `yuvj420p`, and a camera's full-range BT.709 (libx264 `-color_range pc -colorspace bt709`
    // probes `h264 yuvj420p pc bt709`) states a matrix, which labels the untagged HD beside it.
    let full_range = picture(1920, 1080, "yuvj420p", Some("bt709"));
    assert!(full_range.names_a_matrix());
    assert_eq!(bt709_labels(&[Some(&hd), Some(&full_range)]), [true, false]);
    // …while an untagged full-range clip is still never labelled itself.
    let untagged_full_range = picture(1920, 1080, "yuvj420p", None);
    assert!(!untagged_full_range.is_untagged_hd_yuv() && !untagged_full_range.names_a_matrix());
    assert_eq!(bt709_labels(&[Some(&phone), Some(&untagged_full_range)]), [false, false]);
    // An RGB picture has no YCbCr matrix to name even when its tag is not `gbr`, from a codec the
    // list above does not hold (the layout rule alone: `jpegls` is on the list).
    let mut rgb = picture(1920, 1080, "rgb24", Some("bt470bg"));
    rgb.codec = "ffv1".into();
    assert!(!rgb.names_a_matrix());
    rgb.pix_fmt = Some("yuv420p".into());
    assert!(rgb.names_a_matrix(), "the same tag on a YCbCr picture is a matrix");
    // A grey picture is not RGB: from any other codec its tag is the file's word.
    rgb.pix_fmt = Some("gray".into());
    assert!(rgb.names_a_matrix(), "a tagged grey picture names a matrix");
    // A second video stream (cover art) does not tone-map or tag the first.
    let mut asset = crate::engine::test_support::test_asset(vec![
        picture(1920, 1080, "yuv420p", None),
        picture(640, 640, "yuvj420p", None),
    ]);
    asset.streams[1].color_transfer = Some("smpte2084".into());
    assert_eq!(asset.hdr(), None);
    asset.streams.swap(0, 1);
    assert_eq!(asset.hdr(), Some(Hdr::Pq));
    // A still image is not given a convention (a lossy WebP is BT.601 by its format).
    let mut webp = picture(1920, 1080, "yuv420p", None);
    assert!(webp.is_untagged_hd_yuv());
    webp.image = true;
    assert!(!webp.is_untagged_hd_yuv());
    // A stream that is not video names nothing and is nothing.
    let mut audio = picture(1920, 1080, "yuv420p", Some("bt709"));
    audio.kind = StreamKind::Audio;
    assert!(!audio.names_a_matrix() && !audio.is_untagged_hd_yuv());
}

#[test]
fn untagged_hd_is_labelled_only_beside_an_input_that_names_a_matrix() {
    let hd = picture(1920, 1080, "yuv420p", None);
    let hd2 = picture(1280, 720, "yuv420p", Some("unknown"));
    let phone = picture(1920, 1080, "yuv420p", Some("bt709"));
    let mut hlg = picture(3840, 2160, "yuv420p10le", Some("bt2020nc"));
    hlg.color_transfer = Some("smpte2084".into());
    let sd = picture(720, 576, "yuv420p", None);
    let sd_tagged = picture(720, 576, "yuv420p", Some("smpte170m"));
    let mjpeg = picture(1920, 1080, "yuvj420p", None);
    let rgb = picture(1920, 1080, "bgr0", None);
    let alpha = picture(1920, 1080, "yuva420p", None);
    let mut png = picture(1920, 1080, "rgb24", None);
    png.image = true;
    let labels = |inputs: &[Option<&StreamInfo>]| bt709_labels(inputs);

    // A single clip, and a graph of untagged footage, are never labelled.
    assert_eq!(labels(&[Some(&hd)]), [false]);
    assert_eq!(labels(&[Some(&hd), Some(&hd2)]), [false, false]);
    assert_eq!(
        labels(&[Some(&hd), Some(&sd), Some(&mjpeg), Some(&rgb), Some(&alpha), Some(&png)]),
        [false; 6]
    );
    assert_eq!(labels(&[]), Vec::<bool>::new());
    assert_eq!(labels(&[None, Some(&hd)]), [false, false]);
    // Beside an explicit tag or HDR the untagged HD input is labelled, and nothing else is.
    assert_eq!(labels(&[Some(&phone), Some(&hd)]), [false, true]);
    assert_eq!(labels(&[Some(&hd), Some(&phone), Some(&hd2)]), [true, false, true]);
    assert_eq!(labels(&[Some(&hlg), Some(&hd)]), [false, true]);
    assert_eq!(labels(&[Some(&sd_tagged), Some(&hd)]), [false, true]);
    // …and never SD, MJPEG, RGB, grey, alpha or a still image, whatever they sit beside.
    assert_eq!(
        labels(&[
            Some(&phone),
            Some(&sd),
            Some(&mjpeg),
            Some(&rgb),
            Some(&alpha),
            Some(&png),
            None,
            Some(&hd)
        ]),
        [false, false, false, false, false, false, false, true]
    );
    // An RGB picture tagged `gbr` does not name a matrix: it moves nothing.
    let tagged_rgb = picture(1920, 1080, "gbrp", Some("gbr"));
    assert_eq!(labels(&[Some(&tagged_rgb), Some(&hd)]), [false, false]);
    // A tagged input that is not on screen is still an input of the graph: the label is the
    // graph's, not the frame's.
    assert_eq!(labels(&[Some(&phone), Some(&hd), Some(&hd)]), [false, true, true]);
}

#[test]
fn the_chroma_grid_follows_the_pixel_format() {
    let sub = |w, h| Some(Subsampling { log2_w: w, log2_h: h });
    for (name, want) in [
        ("yuv420p", sub(1, 1)),
        ("yuvj420p", sub(1, 1)),
        ("yuv420p10le", sub(1, 1)),
        ("yuv420p16le", sub(1, 1)),
        ("nv12", sub(1, 1)),
        ("p010le", sub(1, 1)),
        ("yuv422p", sub(1, 0)),
        ("yuv422p10le", sub(1, 0)),
        ("nv16", sub(1, 0)),
        ("p210le", sub(1, 0)),
        ("yuv440p", sub(0, 1)),
        ("yuv411p", sub(2, 0)),
        ("yuv410p", sub(2, 2)),
        ("yuv444p", sub(0, 0)),
        ("yuv444p12le", sub(0, 0)),
        ("p410le", sub(0, 0)),
        ("gray", sub(0, 0)),
        ("gray10le", sub(0, 0)),
        ("gbrp", sub(0, 0)),
        ("rgb24", sub(0, 0)),
        ("bgr0", sub(0, 0)),
        ("rgb48le", sub(0, 0)),
        // Not byte-aligned, palettised, packed YCbCr, alpha, nonsense: not known.
        ("rgb565le", None),
        ("rgb4", None),
        ("pal8", None),
        ("uyvy422", None),
        ("yuva420p", None),
        ("yuv420p11le", None),
        ("", None),
    ] {
        assert_eq!(pix_fmt_subsampling(name), want, "{name}");
    }
    // The rounding itself: down to a whole chroma sample.
    assert_eq!((Subsampling::YUV420.round_w(11), Subsampling::YUV420.round_h(7)), (10, 6));
    assert_eq!((Subsampling::NONE.round_w(11), Subsampling::NONE.round_h(7)), (11, 7));
    assert_eq!(Subsampling { log2_w: 2, log2_h: 0 }.round_w(11), 8);
}

#[test]
fn the_pixel_format_allow_list_knows_opaque_formats_and_nothing_else() {
    use PixLayout::*;
    for (name, layout) in [
        ("yuv420p", Yuv420),
        ("yuvj420p", Yuv420),
        ("yuv420p10le", Yuv420),
        ("yuv420p10be", Yuv420),
        ("yuv420p9le", Yuv420),
        ("nv12", Yuv420),
        ("nv21", Yuv420),
        ("p010le", Yuv420),
        ("yuv420p12le", OtherYuv),
        ("yuv420p16le", OtherYuv),
        ("p016le", OtherYuv),
        ("yuv422p", OtherYuv),
        ("yuv444p", OtherYuv),
        ("yuvj444p", OtherYuv),
        ("yuv422p10le", OtherYuv),
        ("yuv444p16le", OtherYuv),
        ("yuv411p", OtherYuv),
        ("yuyv422", OtherYuv),
        ("gray", Gray),
        ("gray10le", Gray),
        ("gray16le", Gray),
        ("rgb24", Rgb),
        ("bgr24", Rgb),
        ("bgr0", Rgb),
        ("0rgb", Rgb),
        ("rgb48le", Rgb),
        ("gbrp", Rgb),
        ("gbrp10le", Rgb),
        ("rgb565le", Rgb),
        ("x2rgb10le", Rgb),
    ] {
        assert_eq!(pix_fmt_layout(name), Some(layout), "{name}");
        assert!(!pix_fmt_has_alpha(name), "{name} is opaque");
    }
    // Alpha-carrying, palettised and unlisted names are all `None` — including the
    // ones a deny-list forgot (`ayuv`, `vuya`, the `rgb32` aliases).
    for name in [
        "yuva420p",
        "yuva444p10le",
        "gbrap",
        "gbrap16le",
        "rgba",
        "bgra",
        "argb",
        "abgr",
        "rgba64le",
        "ya8",
        "ya16le",
        "ayuv",
        "vuya",
        "ayuv64le",
        "rgb32",
        "bgr32",
        "pal8",
        "grayf32le",
        "something_new",
        "",
    ] {
        assert_eq!(pix_fmt_layout(name), None, "{name}");
    }
    for name in [
        "yuva420p", "gbrap", "rgba", "ayuv", "vuya", "rgb32", "bgr32", "pal8", "ya8", "argb64le",
    ] {
        assert!(pix_fmt_has_alpha(name), "{name} carries alpha");
    }
    // `has_alpha` is three-valued: yes, no, and "not recorded or not known".
    let mut s = StreamInfo {
        index: 0,
        kind: StreamKind::Video,
        codec: "h264".into(),
        width: Some(1),
        height: Some(1),
        fps: None,
        sample_rate: None,
        channels: None,
        image: false,
        projection: None,
        rotation: 0,
        color_transfer: None,
        color_primaries: None,
        pix_fmt: None,
        color_space: None,
        color_range: None,
        lead: 0.0,
        time_base: None,
    };
    assert_eq!(s.has_alpha(), None);
    s.pix_fmt = Some("yuv420p".into());
    assert_eq!(s.has_alpha(), Some(false));
    s.pix_fmt = Some("ayuv".into());
    assert_eq!(s.has_alpha(), Some(true));
    s.pix_fmt = Some("something_new".into());
    assert_eq!(s.has_alpha(), None);
}

#[test]
fn a_stream_saved_before_rotation_and_colour_were_probed_still_loads_as_sdr() {
    let old = r#"{"index":0,"kind":"video","codec":"hevc","width":1920,"height":1080,"fps":30.0}"#;
    let s: StreamInfo = serde_json::from_str(old).unwrap();
    assert_eq!((s.rotation, s.color_transfer.clone(), s.hdr()), (0, None, None));
    // And an ordinary stream serializes exactly as it did.
    assert_eq!(
        serde_json::to_string(&s).unwrap(),
        r#"{"index":0,"kind":"video","codec":"hevc","width":1920,"height":1080,"fps":30.0}"#
    );
}

#[test]
fn hdr_is_a_property_of_video_streams_with_an_hdr_transfer() {
    let mut s: StreamInfo = serde_json::from_str(r#"{"index":0,"kind":"video","codec":"hevc"}"#).unwrap();
    for (tag, want) in [
        ("arib-std-b67", Some(Hdr::Hlg)),
        ("smpte2084", Some(Hdr::Pq)),
        ("bt709", None),
        ("smpte170m", None),
    ] {
        s.color_transfer = Some(tag.into());
        assert_eq!(s.hdr(), want, "{tag}");
    }
    s.color_transfer = Some("arib-std-b67".into());
    s.kind = StreamKind::Audio;
    assert_eq!(s.hdr(), None);
}

#[test]
fn a_proxied_asset_is_sdr_and_keeps_the_rest_of_its_metadata() {
    let streams = r#"[{"index":0,"kind":"video","codec":"hevc","width":1080,"height":1920,"rotation":-90,"color_transfer":"arib-std-b67","color_primaries":"bt2020"}]"#;
    let asset = Asset {
        id: Uuid::new_v4(),
        path: "/a.mov".into(),
        name: "a.mov".into(),
        duration: 3.0,
        streams: serde_json::from_str(streams).unwrap(),
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    assert_eq!(asset.hdr(), Some(Hdr::Hlg));
    let proxied = asset.as_sdr_proxy();
    assert_eq!(proxied.hdr(), None);
    assert_eq!(
        (
            proxied.streams[0].width,
            proxied.streams[0].height,
            proxied.streams[0].rotation
        ),
        (Some(1080), Some(1920), -90)
    );
    assert_eq!(asset.hdr(), Some(Hdr::Hlg), "the original is untouched");
}

fn clip_at(start: f64, dur: f64) -> Clip {
    Clip::new(Uuid::new_v4(), 0.0, dur, start)
}

fn track(kind: StreamKind, name: &str, clips: Vec<Clip>) -> Track {
    Track {
        clips,
        ..Track::new(kind, name)
    }
}

#[test]
fn a_zoom_is_animated_only_when_the_keyed_scale_moves() {
    let key = |time: f64, scale: f64, pos_x: f64| Keyframe {
        time,
        scale,
        pos_x,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    };
    let mut clip = clip_at(0.0, 4.0);
    // Nothing keyed, and one key (a held pose): the picture never changes size.
    assert!(!clip.zoom_animated());
    clip.keyframes = vec![key(0.0, 1.5, 0.0)];
    assert!(!clip.zoom_animated());
    // Position moves, scale does not: animated, but not a zoom.
    clip.keyframes = vec![key(0.0, 0.5, -0.2), key(2.0, 0.5, 0.2)];
    assert!(clip.is_animated() && !clip.zoom_animated());
    // Float noise is not a zoom (it cannot change the size by a pixel).
    clip.keyframes = vec![key(0.0, 0.5, 0.0), key(2.0, 0.5 + 1e-12, 0.0)];
    assert!(!clip.zoom_animated());
    // Any key off the first one is, wherever it sits and whatever the order.
    clip.keyframes = vec![key(0.0, 0.5, 0.0), key(1.0, 0.5, 0.0), key(2.0, 0.75, 0.0)];
    assert!(clip.zoom_animated());
    clip.keyframes = vec![key(2.0, 0.75, 0.0), key(0.0, 0.5, 0.0)];
    assert!(clip.zoom_animated());
    // Held against the first key in *time*: a stored order that puts a later key first
    // gives the same answer, at the edge of the tolerance too.
    clip.keyframes = vec![key(1.0, 0.5 + 0.8e-9, 0.0), key(0.0, 0.5, 0.0), key(2.0, 0.5 + 1.6e-9, 0.0)];
    assert!(
        clip.zoom_animated(),
        "1.6e-9 from the first key in time, though 0.8e-9 from the stored first"
    );
    clip.keyframes = vec![key(1.0, 0.5 + 0.8e-9, 0.0), key(0.0, 0.5, 0.0)];
    assert!(!clip.zoom_animated());
}

#[test]
fn a_cover_clip_zoomed_out_and_moved_over_a_cover_that_overhangs_is_uncropped() {
    let key = |time: f64, scale: f64| Keyframe {
        time,
        scale,
        pos_x: 0.1,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    };
    // The case that hangs 4:3 footage in a 16:9 frame: a static zoom out of 0.8 and a pan of 0.1.
    let mut clip = clip_at(0.0, 4.0);
    clip.transform.scale = 0.8;
    clip.transform.pos_x = 0.1;
    assert_eq!(clip.held_scale(), Some(0.8));
    assert_eq!(clip.pan_extent(), (0.1, 0.0));
    // The whole cover is zoomed and the canvas cuts it (the zoom out reveals the overhang)...
    assert_eq!(clip.fit_order(Fit::Cover, true), FitOrder::Uncropped);
    // ... but a picture of the frame's own shape has none to reveal, so it keeps its chain,
    assert_eq!(clip.fit_order(Fit::Cover, false), FitOrder::Fitted);
    // as does a zoom out under Contain,
    assert_eq!(clip.fit_order(Fit::Contain, true), FitOrder::Fitted);
    // a zoom that moves (it is the chain's last stage, at the fit size),
    clip.keyframes = vec![key(0.0, 0.8), key(2.0, 1.2)];
    assert_eq!(clip.held_scale(), None);
    assert_eq!(clip.fit_order(Fit::Cover, true), FitOrder::Fitted);
    // and a mask or a vignette, which are laid out over the frame the chain hands them.
    clip.keyframes.clear();
    clip.mask = Some(Mask::default());
    assert_eq!(clip.fit_order(Fit::Cover, true), FitOrder::Fitted);
    clip.mask = None;
    clip.effects = vec![VideoEffect::Vignette];
    assert_eq!(clip.fit_order(Fit::Cover, true), FitOrder::Fitted);
    // A keyed pan over a held zoom is the same order as the static one.
    let mut keyed = clip_at(0.0, 4.0);
    keyed.keyframes = vec![key(0.0, 0.8), key(2.0, 0.8)];
    assert_eq!(keyed.held_scale(), Some(0.8));
    assert_eq!(keyed.fit_order(Fit::Cover, true), FitOrder::Uncropped);
    // A zoom in is the cover times the zoom whatever the shape; the identity and a turn are never.
    let mut zoomed = clip_at(0.0, 4.0);
    zoomed.transform.scale = 1.5;
    assert_eq!(zoomed.fit_order(Fit::Cover, false), FitOrder::Uncropped);
    assert_eq!(zoomed.fit_order(Fit::Contain, false), FitOrder::Folded);
    let mut turned = clip_at(0.0, 4.0);
    turned.transform.rotation = 15.0;
    assert_eq!(turned.fit_order(Fit::Cover, true), FitOrder::Fitted);
    assert_eq!(clip_at(0.0, 4.0).fit_order(Fit::Cover, true), FitOrder::Fitted);
}

#[test]
fn beat_grid_maps_source_beats_onto_the_timeline() {
    // A music clip cut from 2s into the source and placed at 10s: a beat at
    // source 3.0 is heard at 11.0, and beats outside the window never sound.
    let asset = Uuid::new_v4();
    let clip = Clip::new(asset, 2.0, 6.0, 10.0);
    let tl = Timeline {
        tracks: vec![track(StreamKind::Audio, "A1", vec![clip])],
        ..Timeline::new()
    };
    let tempos = HashMap::from([(
        asset,
        Tempo {
            downbeats: Vec::new(),
            bpm: 120.0,
            beats: vec![0.0, 1.0, 3.0, 5.0, 8.0],
            confidence: 0.5,
        },
    )]);
    assert_eq!(tl.beat_grid(&tempos), vec![11.0, 13.0]);
}

#[test]
fn beat_grid_ignores_a_low_confidence_tempo() {
    let asset = Uuid::new_v4();
    let tl = Timeline {
        tracks: vec![track(StreamKind::Audio, "A1", vec![Clip::new(asset, 0.0, 4.0, 0.0)])],
        ..Timeline::new()
    };
    let tempos = HashMap::from([(
        asset,
        Tempo {
            downbeats: Vec::new(),
            bpm: 120.0,
            beats: vec![0.5, 1.0],
            confidence: BEAT_MIN_CONFIDENCE - 0.01,
        },
    )]);
    assert!(tl.beat_grid(&tempos).is_empty());
}

#[test]
fn nearest_beat_takes_the_closest_within_tolerance() {
    let beats = [0.0, 0.5, 1.0, 1.5];
    assert_eq!(nearest_beat(&beats, 0.6, 0.25), Some(0.5));
    assert_eq!(nearest_beat(&beats, 0.9, 0.25), Some(1.0));
    assert_eq!(nearest_beat(&beats, 0.75, 0.1), None);
    assert_eq!(nearest_beat(&beats, 9.0, 0.25), None);
    assert_eq!(default_beat_tolerance(&beats), 0.25);
}

#[test]
fn align_cuts_to_beats_ripples_every_cut_onto_the_grid() {
    let asset = Uuid::new_v4();
    let beats: Vec<f64> = (0..=20).map(|i| i as f64 * 0.5).collect();
    let limits = HashMap::from([(asset, 10.0)]);
    let mut t = track(
        StreamKind::Video,
        "V1",
        vec![Clip::new(asset, 0.0, 1.1, 0.0), Clip::new(asset, 4.0, 4.9, 1.1)],
    );

    assert_eq!(t.align_cuts_to_beats(&beats, 0.25, &limits), 2);
    assert_eq!(t.clips[0].timeline_start, 0.0);
    assert_eq!(t.clips[0].source_out, 1.0, "the first cut moved back onto the beat");
    assert_eq!(t.clips[1].timeline_start, 1.0, "the next clip rippled with it");
    assert_eq!(t.clips[1].timeline_end(), 2.0, "and its own cut landed on a beat too");
    assert_eq!(t.clips[1].source_in, 4.0, "trimming happens at the outgoing edge");

    // Already aligned: nothing moves, so running it twice is a no-op.
    assert_eq!(t.align_cuts_to_beats(&beats, 0.25, &limits), 0);
}

#[test]
fn align_cuts_to_beats_keeps_gaps_and_respects_the_source() {
    let asset = Uuid::new_v4();
    let beats: Vec<f64> = (0..=20).map(|i| i as f64 * 0.5).collect();
    // Only 1.2s of footage left after source_in, so the clip cannot stretch
    // to the 1.5 beat its end is nearest.
    let limits = HashMap::from([(asset, 1.2)]);
    let mut t = track(
        StreamKind::Video,
        "V1",
        vec![Clip::new(asset, 0.0, 0.4, 0.0), Clip::new(asset, 0.0, 1.4, 0.9)],
    );

    t.align_cuts_to_beats(&beats, 0.25, &limits);
    assert_eq!(t.clips[0].timeline_end(), 0.5);
    assert_eq!(t.clips[1].timeline_start, 1.0, "the 0.5s gap survived, snapped to a beat");
    assert_eq!(t.clips[1].source_out, 1.2, "stretched only as far as there is footage");
}

#[test]
fn align_cuts_to_beats_trims_a_reversed_clip_at_its_outgoing_edge() {
    // Played backwards the timeline tail is the *start* of the source, so
    // shortening the clip must move source_in, not source_out.
    let asset = Uuid::new_v4();
    let beats: Vec<f64> = (0..=20).map(|i| i as f64 * 0.5).collect();
    let mut clip = Clip::new(asset, 1.0, 2.1, 0.0);
    clip.speed = -1.0;
    let mut t = track(StreamKind::Video, "V1", vec![clip]);

    t.align_cuts_to_beats(&beats, 0.25, &HashMap::from([(asset, 10.0)]));
    assert_eq!(t.clips[0].source_out, 2.1);
    assert!((t.clips[0].source_in - 1.1).abs() < 1e-9);
    assert!((t.clips[0].timeline_end() - 1.0).abs() < 1e-9);
}

#[test]
fn for_render_drops_muted_tracks_and_disabled_clips() {
    let mut disabled = clip_at(0.0, 2.0);
    disabled.enabled = false;
    let tl = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0), disabled]),
            Track {
                muted: true,
                ..track(StreamKind::Audio, "A1", vec![clip_at(0.0, 2.0)])
            },
        ],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let r = tl.for_render();
    // The disabled clip is gone but the enabled one stays.
    assert_eq!(r.tracks[0].clips.len(), 1);
    // A muted track keeps its row (it still carries `duck`) but loses its clips.
    assert_eq!(r.tracks.len(), 2);
    assert!(r.tracks[1].clips.is_empty());
    // Filtering never touches the original.
    assert_eq!(tl.tracks[0].clips.len(), 2);
}

#[test]
fn solo_shadows_other_tracks_of_the_same_kind_only() {
    let tl = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0)]),
            Track {
                solo: true,
                ..track(StreamKind::Video, "V2", vec![clip_at(0.0, 2.0)])
            },
            track(StreamKind::Audio, "A1", vec![clip_at(0.0, 2.0)]),
        ],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let r = tl.for_render();
    assert!(r.tracks[0].clips.is_empty(), "unsoloed video track is shadowed");
    assert_eq!(r.tracks[1].clips.len(), 1, "soloed video track renders");
    // Soloing a video track must not blank unrelated audio.
    assert_eq!(r.tracks[2].clips.len(), 1, "audio is unaffected by a video solo");
}

#[test]
fn a_muted_track_stays_muted_even_when_soloed() {
    let tl = Timeline {
        tracks: vec![Track {
            muted: true,
            solo: true,
            ..track(StreamKind::Audio, "A1", vec![clip_at(0.0, 2.0)])
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    assert!(tl.for_render().tracks[0].clips.is_empty());
}

fn framed(ratio: (u32, u32), left: f64, right: f64) -> Framing {
    Framing {
        aspect_w: ratio.0,
        aspect_h: ratio.1,
        crop_left: left,
        crop_right: right,
        crop_top: 0.0,
        crop_bottom: 0.0,
    }
}

#[test]
fn delivery_shapes_reduce_and_parse() {
    assert_eq!(Delivery::new(1080, 1920, Fit::Cover).ratio(), (9, 16));
    assert_eq!(Delivery::new(720, 1280, Fit::Cover).ratio(), (9, 16));
    assert_eq!(Delivery::new(1080, 1350, Fit::Cover).ratio_label(), "4:5");
    let v = Delivery::parse("9:16").unwrap();
    assert_eq!((v.width, v.height, v.fit), (1080, 1920, Fit::Cover));
    let l = Delivery::parse("16:9").unwrap();
    assert_eq!((l.width, l.height, l.fit), (1920, 1080, Fit::Contain));
    let custom = Delivery::parse("1440x1800").unwrap();
    assert_eq!((custom.width, custom.height, custom.fit), (1440, 1800, Fit::Cover));
    assert_eq!(Delivery::parse("3840x2160").unwrap().fit, Fit::Contain);
    assert!(Delivery::parse("wide").is_none());
    assert!(Delivery::parse("0x10").is_none());
}

#[test]
fn a_framing_replaces_its_own_shape_and_keeps_the_others() {
    let mut clip = clip_at(0.0, 2.0);
    assert!(clip.set_framing(framed((9, 16), 0.1, 0.5)));
    assert!(clip.set_framing(framed((1, 1), 0.2, 0.3)));
    assert!(!clip.set_framing(framed((9, 16), 0.1, 0.5)), "unchanged is not a change");
    assert!(clip.set_framing(framed((9, 16), 0.3, 0.3)));
    assert_eq!(clip.framings.len(), 2);
    assert_eq!(clip.framing_for((9, 16)).unwrap().crop_left, 0.3);
    assert_eq!(clip.framing_for((1, 1)).unwrap().crop_left, 0.2);
    assert!(clip.framing_for((4, 5)).is_none());
}

#[test]
fn for_delivery_swaps_in_the_crop_for_that_shape() {
    let mut clip = clip_at(0.0, 2.0);
    // The project is cut 9:16 and the clip carries that crop as its transform.
    clip.transform.crop_left = 0.1;
    clip.transform.crop_right = 0.5836;
    clip.set_framing(framed((1, 1), 0.2, 0.3));
    let tl = Timeline {
        tracks: vec![track(StreamKind::Video, "V1", vec![clip])],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Some(Delivery::new(1080, 1920, Fit::Cover)),
        master: Default::default(),
        thumbnails: Vec::new(),
    };

    let square = tl.for_delivery(Delivery::new(1080, 1080, Fit::Cover));
    let t = square.tracks[0].clips[0].transform;
    assert_eq!((t.crop_left, t.crop_right), (0.2, 0.3));
    assert_eq!(square.format.unwrap().ratio(), (1, 1));

    // No framing for 4:5: the crop it has is kept rather than thrown away.
    let portrait = tl.for_delivery(Delivery::new(1080, 1350, Fit::Cover));
    let t = portrait.tracks[0].clips[0].transform;
    assert_eq!((t.crop_left, t.crop_right), (0.1, 0.5836));

    // The project's own shape at another size changes only the size.
    let small = tl.for_delivery(Delivery::new(720, 1280, Fit::Cover));
    assert_eq!(small.tracks[0].clips[0].transform, tl.tracks[0].clips[0].transform);
    assert_eq!((small.format.unwrap().width, small.format.unwrap().height), (720, 1280));

    // Never touches the original.
    assert_eq!(tl.tracks[0].clips[0].transform.crop_left, 0.1);
}

#[test]
fn for_delivery_refits_generated_captions_only() {
    let long = "a caption line that is wide";
    let mut caption = TextOverlay::new(long, 0.0, 1.0);
    caption.size = 0.05;
    caption.generated = true;
    let mut title = TextOverlay::new(long, 0.0, 1.0);
    title.size = 0.05;
    let tl = Timeline {
        tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0)])],
        overlays: vec![caption, title],
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Some(Delivery::new(1920, 1080, Fit::Contain)),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let vertical = tl.for_delivery(Delivery::new(1080, 1920, Fit::Cover));
    let fitted = vertical.overlays[0].size;
    assert!(fitted < 0.05, "a 9:16 frame is too narrow for the line at 5%");
    assert!(
        long.chars().count() as f64 * CHAR_ADVANCE * fitted <= CAPTION_WIDTH * (1080.0 / 1920.0) + 1e-9,
        "fits across the frame"
    );
    assert_eq!(vertical.overlays[1].size, 0.05, "a typed title is not resized");
}

#[test]
fn a_new_framing_reads_in_the_diff() {
    let before = Timeline {
        tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0)])],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let mut after = before.clone();
    after.tracks[0].clips[0].set_framing(framed((9, 16), 0.1, 0.5));
    after.tracks[0].clips[0].set_framing(framed((1, 1), 0.2, 0.3));
    let diff = before.diff(&after);
    assert_eq!(diff.entries.len(), 1, "{diff:?}");
    let detail = diff.entries[0].detail.clone().unwrap_or_default();
    assert!(detail.contains("framed for 9:16, 1:1"), "{detail}");
    assert!(after.diff(&after).entries.is_empty());
}

#[test]
fn for_render_is_a_no_op_on_an_untouched_timeline() {
    let tl = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0), clip_at(2.0, 1.0)]),
            track(StreamKind::Audio, "A1", vec![clip_at(0.0, 3.0)]),
        ],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let r = tl.for_render();
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        serde_json::to_string(&tl).unwrap(),
        "an ordinary timeline must reach the graph builders unchanged"
    );
}

#[test]
fn enabled_defaults_to_true_for_clips_saved_before_the_field_existed() {
    // Old projects have no `enabled` key; they must not silently stop rendering.
    let clip: Clip = serde_json::from_str(
        r#"{"id":"00000000-0000-0000-0000-000000000001",
                "asset_id":"00000000-0000-0000-0000-000000000002",
                "source_in":0.0,"source_out":1.0,"timeline_start":0.0,"volume":1.0}"#,
    )
    .unwrap();
    assert!(clip.enabled);
    let t: Track =
        serde_json::from_str(r#"{"id":"00000000-0000-0000-0000-000000000003","kind":"video","name":"V1","clips":[]}"#).unwrap();
    assert!(!t.muted && !t.solo && !t.locked);
}

// ---- keyframe easing -----------------------------------------------------------

fn eased(time: f64, scale: f64, easing: Easing) -> Keyframe {
    Keyframe {
        easing,
        ..Keyframe::from_transform(
            time,
            &Transform {
                scale,
                ..Transform::default()
            },
        )
    }
}

#[test]
fn easing_curves_start_at_zero_end_at_one_and_never_go_back() {
    let curves = [
        Easing::Linear,
        Easing::EaseIn,
        Easing::EaseOut,
        Easing::EaseInOut,
        Easing::Bezier {
            x1: 0.2,
            y1: 0.9,
            x2: 0.3,
            y2: 0.1,
        },
    ];
    for e in curves {
        assert!(e.curve(0.0).abs() < 1e-9, "{e:?}");
        assert!((e.curve(1.0) - 1.0).abs() < 1e-9, "{e:?}");
        let mut last = 0.0;
        for i in 1..=100 {
            let v = e.curve(f64::from(i) / 100.0);
            assert!(v >= last - 1e-9 && v <= 1.0 + 1e-9, "{e:?} at {i}: {v} after {last}");
            last = v;
        }
    }
    // The named curves are CSS's: ease-in is slow at first, ease-out slow at the end.
    assert!(Easing::EaseIn.curve(0.25) < 0.25 && Easing::EaseOut.curve(0.25) > 0.25);
    assert!((Easing::EaseInOut.curve(0.5) - 0.5).abs() < 1e-6);
    // A hold stays put until the next key.
    assert_eq!(Easing::Hold.curve(0.99), 0.0);
    // Control points outside the unit square are held to it (no overshoot).
    let wild = Easing::Bezier {
        x1: 0.5,
        y1: 3.0,
        x2: 0.5,
        y2: -2.0,
    };
    assert!((0..=20).all(|i| (0.0..=1.0).contains(&wild.curve(f64::from(i) / 20.0))));
}

/// The same numbers `frontend/src/lib/easing.test.ts` pins for its mirror: the two
/// implementations do the same arithmetic in the same order, so they agree exactly.
#[test]
fn easing_curves_match_the_frontend_mirror_bit_for_bit() {
    let bezier = Easing::Bezier {
        x1: 0.2,
        y1: 0.9,
        x2: 0.3,
        y2: 0.1,
    };
    assert_eq!(Easing::EaseIn.curve(0.25), 0.093_464_650_718_456_97);
    assert_eq!(Easing::EaseOut.curve(0.25), 0.378_138_130_824_722_36);
    assert_eq!(Easing::EaseInOut.curve(0.3), 0.187_395_906_704_947_93);
    assert_eq!(bezier.curve(0.5), 0.551_329_682_020_157_3);
    assert_eq!(Easing::EaseInOut.curve(7.0 / 12.0), 0.641_173_603_406_141_9);
}

#[test]
fn linear_keys_are_their_own_polyline_and_eased_ones_are_twelve_pieces() {
    let lin = [(0.0, 1.0, Easing::Linear), (2.0, 3.0, Easing::Linear)];
    assert_eq!(eased_points(&lin), vec![(0.0, 1.0), (2.0, 3.0)]);
    let ease = [(0.0, 1.0, Easing::EaseInOut), (2.0, 3.0, Easing::Linear)];
    let pts = eased_points(&ease);
    assert_eq!(pts.len(), EASE_STEPS + 1);
    assert_eq!(pts.first(), Some(&(0.0, 1.0)));
    assert_eq!(pts.last(), Some(&(2.0, 3.0)));
    // A hold is a step at the next key.
    let hold = [(0.0, 1.0, Easing::Hold), (2.0, 3.0, Easing::Linear)];
    assert_eq!(eased_points(&hold), vec![(0.0, 1.0), (2.0, 1.0), (2.0, 3.0)]);
    assert_eq!(interpolate(&eased_points(&hold), 1.999), Some(1.0));
    assert_eq!(interpolate(&eased_points(&hold), 2.0), Some(3.0));
}

#[test]
fn transform_at_follows_each_keys_outgoing_easing() {
    let mut clip = clip_at(0.0, 10.0);
    clip.keyframes = vec![
        eased(0.0, 1.0, Easing::EaseIn),
        eased(2.0, 3.0, Easing::Hold),
        eased(4.0, 5.0, Easing::Linear),
        eased(6.0, 1.0, Easing::Linear),
    ];
    // Slow out of the first key, at the key values on the keys.
    assert!(clip.transform_at(0.5).scale < 1.5);
    assert!((clip.transform_at(2.0).scale - 3.0).abs() < 1e-12);
    // Held from 2 to 4, then the jump.
    assert!((clip.transform_at(3.9).scale - 3.0).abs() < 1e-12);
    assert!((clip.transform_at(4.0).scale - 5.0).abs() < 1e-12);
    // A linear segment is exactly linear.
    assert!((clip.transform_at(5.0).scale - 3.0).abs() < 1e-12);
}

#[test]
fn keys_saved_before_easing_existed_are_linear_and_linear_is_not_written() {
    let k: Keyframe = serde_json::from_str(r#"{"time":1.0,"scale":2.0}"#).unwrap();
    assert_eq!(k.easing, Easing::Linear);
    assert!(!serde_json::to_string(&k).unwrap().contains("easing"));
    let b = eased(
        0.0,
        1.0,
        Easing::Bezier {
            x1: 0.1,
            y1: 0.2,
            x2: 0.3,
            y2: 0.4,
        },
    );
    let json = serde_json::to_string(&b).unwrap();
    assert!(
        json.contains(r#""easing":{"bezier":{"x1":0.1,"y1":0.2,"x2":0.3,"y2":0.4}}"#),
        "{json}"
    );
    assert_eq!(serde_json::from_str::<Keyframe>(&json).unwrap(), b);
    let hold = serde_json::to_string(&eased(0.0, 1.0, Easing::EaseInOut)).unwrap();
    assert!(hold.contains(r#""easing":"ease_in_out""#), "{hold}");
}

#[test]
fn cutting_into_an_eased_segment_keeps_the_curve_exactly() {
    for easing in [Easing::EaseInOut, Easing::Hold, Easing::EaseOut] {
        let mut original = clip_at(0.0, 8.0);
        original.keyframes = vec![
            eased(0.0, 1.0, easing),
            eased(3.0, 4.0, Easing::EaseIn),
            eased(6.0, 2.0, Easing::Linear),
        ];
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![original.clone()])],
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
            thumbnails: Vec::new(),
        };
        // A cut inside the first (eased) segment, not on one of its pieces.
        let from = 1.1;
        let sliced = tl.slice(from, 8.0);
        let c = &sliced.tracks[0].clips[0];
        for i in 0..=120 {
            let t = f64::from(i) * 0.05;
            let (a, b) = (c.transform_at(t).scale, original.transform_at(from + t).scale);
            assert!((a - b).abs() < 1e-9, "{easing:?} at {t}: sliced {a}, original {b}");
        }
    }
}

/// A head cut that lands exactly on a key used to read as "before the segment": the key's
/// outgoing easing was dropped and a hold became a ramp from its value to the next key's.
#[test]
fn cutting_exactly_on_a_key_keeps_the_easing_that_leaves_it() {
    let bezier = Easing::Bezier {
        x1: 0.2,
        y1: 0.9,
        x2: 0.3,
        y2: 0.1,
    };
    for easing in [Easing::EaseInOut, Easing::Hold, bezier, Easing::EaseOut] {
        let mut original = clip_at(0.0, 8.0);
        original.keyframes = vec![
            eased(0.0, 1.0, Easing::Linear),
            eased(3.0, 4.0, easing),
            eased(6.0, 2.0, Easing::Linear),
        ];
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![original.clone()])],
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
            thumbnails: Vec::new(),
        };
        let id = original.id;
        // The range cut, and the edit that trims the head and moves the clip: same result.
        let sliced = tl.slice(3.0, 8.0).tracks[0].clips[0].clone();
        let mut trimmed = tl;
        let trimmed_clip = trimmed.split_remove(id, 3.0, SplitSide::Left).unwrap();
        for c in [sliced, trimmed_clip] {
            // Between the pieces of a curve and a hold's step, never on one.
            for i in 0..60 {
                let t = (f64::from(i) + 0.5) * 0.05;
                let (a, b) = (c.transform_at(t).scale, original.transform_at(3.0 + t).scale);
                assert!((a - b).abs() < 1e-9, "{easing:?} at {t}: cut {a}, original {b}");
            }
            if easing == Easing::Hold {
                assert_eq!(c.keyframes[0].easing, Easing::Hold);
                assert_eq!(c.transform_at(2.9).scale, 4.0);
            }
        }
    }
}

/// The curve `easing` draws over `[0, 1]`, as the two halves of a split at `u` draw it.
fn split_deviation(easing: Easing, u: f64) -> f64 {
    let (before, after) = easing.split(u);
    let v = easing.curve(u);
    let mut worst: f64 = 0.0;
    for i in 0..=20 {
        let w = f64::from(i) / 20.0;
        // The first part stretched back to its size in the whole, then the second.
        worst = worst.max((v * before.curve(w) - easing.curve(u * w)).abs());
        worst = worst.max((v + (1.0 - v) * after.curve(w) - easing.curve(u + (1.0 - u) * w)).abs());
    }
    worst
}

#[test]
fn splitting_an_easing_draws_the_same_curve_in_two_pieces() {
    let at = [0.05, 0.3, 0.5, 0.7, 0.95];
    // Every preset the editor offers, exactly...
    let named = [
        Easing::EaseIn,
        Easing::EaseOut,
        Easing::EaseInOut,
        Easing::Bezier {
            x1: 0.2,
            y1: 0.9,
            x2: 0.3,
            y2: 1.0,
        },
        Easing::Bezier {
            x1: 0.45,
            y1: 0.05,
            x2: 0.55,
            y2: 0.95,
        },
        Easing::Bezier {
            x1: 0.1,
            y1: 0.6,
            x2: 0.4,
            y2: 1.0,
        },
    ];
    for easing in named {
        for u in at {
            assert!(split_deviation(easing, u) < 1e-9, "{easing:?} split at {u}");
        }
    }
    // ... and so is any bezier whose control points rise, over a grid of them.
    let grid = [0.0, 0.25, 0.5, 0.75, 1.0];
    for x1 in grid {
        for y1 in grid {
            for x2 in grid.into_iter().filter(|x2| *x2 >= x1) {
                for y2 in grid.into_iter().filter(|y2| *y2 >= y1) {
                    let easing = Easing::Bezier { x1, y1, x2, y2 };
                    for u in at {
                        let off = split_deviation(easing, u);
                        assert!(off < 1e-4, "{easing:?} split at {u} is {off} off");
                    }
                }
            }
        }
    }
    // An S that turns back needs a control point outside the square: re-fitted, not exact.
    let s_curve = Easing::Bezier {
        x1: 0.2,
        y1: 0.9,
        x2: 0.3,
        y2: 0.1,
    };
    let off = at.map(|u| split_deviation(s_curve, u)).into_iter().fold(0.0, f64::max);
    assert!((1e-4..0.05).contains(&off), "{off}");
    // Not curves: a line stays a line, a hold stays held on both sides.
    assert_eq!(Easing::Linear.split(0.4), (Easing::Linear, Easing::Linear));
    assert_eq!(Easing::Hold.split(0.4), (Easing::Hold, Easing::Hold));
    // A plateau at the start has nothing to draw before the new key.
    let late = Easing::Bezier {
        x1: 0.0,
        y1: 0.0,
        x2: 0.0,
        y2: 0.0,
    };
    assert_eq!(late.split(1e-12).0, Easing::Linear);
}

/// The numbers `frontend/src/lib/easing.test.ts` pins for `splitEasing`.
#[test]
fn easing_splits_match_the_frontend_mirror_bit_for_bit() {
    let bezier = |x1, y1, x2, y2| Easing::Bezier { x1, y1, x2, y2 };
    assert_eq!(
        Easing::EaseInOut.split(0.3),
        (
            bezier(0.387_469_928_603_703_55, 0.0, 0.708_554_541_138_148_6, 0.408_751_946_428_076),
            bezier(
                0.326_400_214_186_121_75,
                0.356_304_039_690_523_16,
                0.566_058_540_830_177_7,
                1.0
            ),
        )
    );
    assert_eq!(
        Easing::EaseIn.split(0.25),
        (
            bezier(
                0.317_162_150_132_499_54,
                0.0,
                0.657_134_238_921_790_6,
                0.381_326_296_917_524_73
            ),
            bezier(0.491_095_208_371_804_6, 0.274_086_177_201_424_27, 1.0, 1.0),
        )
    );
}

#[test]
fn a_key_added_inside_a_segment_splits_it_and_leaves_the_motion_where_it_was() {
    let bezier = Easing::Bezier {
        x1: 0.2,
        y1: 0.9,
        x2: 0.3,
        y2: 1.0,
    };
    for easing in [Easing::EaseInOut, Easing::EaseIn, bezier, Easing::Hold] {
        let mut clip = clip_at(0.0, 10.0);
        clip.keyframes = vec![eased(1.0, 1.0, easing), eased(5.0, 3.0, Easing::Linear)];
        let before = clip.clone();
        // Pin the present pose a third of the way in, as the editor does.
        let pose = Keyframe::from_transform(2.3, &clip.transform_at(2.3));
        clip.insert_keyframe(pose);
        assert_eq!(clip.keyframes.len(), 3);
        for i in 0..=80 {
            // Never on a hold's step. The two are the same curve drawn in different pieces,
            // so they differ by how a steep curve is drawn in 12 of them (about 0.014 of a
            // range of 2 for the steepest here); a linear key in the middle is 0.3 off.
            let t = (f64::from(i) + 0.5) * 0.05;
            let (a, b) = (clip.transform_at(t).scale, before.transform_at(t).scale);
            assert!((a - b).abs() < 0.03, "{easing:?} at {t}: now {a}, was {b}");
        }
        if easing == Easing::Hold {
            assert_eq!(
                (clip.keyframes[0].easing, clip.keyframes[1].easing),
                (Easing::Hold, Easing::Hold)
            );
            assert_eq!(clip.transform_at(4.9).scale, 1.0);
        } else {
            assert!(matches!(clip.keyframes[0].easing, Easing::Bezier { .. }));
            assert!(matches!(clip.keyframes[1].easing, Easing::Bezier { .. }));
        }
        assert_eq!(clip.keyframes[2].easing, Easing::Linear);
    }
}

#[test]
fn a_key_outside_every_segment_or_on_a_key_changes_no_neighbours_shape() {
    let mut clip = clip_at(0.0, 10.0);
    clip.keyframes = vec![eased(1.0, 1.0, Easing::EaseIn), eased(5.0, 3.0, Easing::Hold)];
    // Before the first and after the last there is no segment to split: the key comes as it is
    // and the ones next to it are untouched.
    clip.insert_keyframe(eased(0.0, 2.0, Easing::Hold));
    clip.insert_keyframe(eased(8.0, 2.0, Easing::EaseOut));
    let shapes: Vec<_> = clip.keyframes.iter().map(|k| (k.time, k.easing)).collect();
    assert_eq!(
        shapes,
        vec![
            (0.0, Easing::Hold),
            (1.0, Easing::EaseIn),
            (5.0, Easing::Hold),
            (8.0, Easing::EaseOut)
        ]
    );
    // On a key: the pose moves, the way it leaves does not.
    clip.insert_keyframe(eased(1.0000004, 9.0, Easing::Linear));
    assert_eq!((clip.keyframes[1].scale, clip.keyframes[1].easing), (9.0, Easing::EaseIn));
    assert_eq!(clip.keyframes.len(), 4);
}

#[test]
fn export_span_clamps_to_the_cut_and_falls_back_to_everything_when_empty() {
    let tl = Timeline {
        tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 60.0)])],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    assert_eq!(tl.export_span(None), None);
    assert_eq!(tl.export_span(Some((10.0, 30.0))), Some((10.0, 30.0)));
    assert_eq!(tl.export_span(Some((-5.0, 20.0))), Some((0.0, 20.0)));
    assert_eq!(tl.export_span(Some((40.0, 500.0))), Some((40.0, 60.0)));
    assert_eq!(tl.export_span(Some((10.0, 10.0))), None);
    assert_eq!(tl.export_span(Some((30.0, 10.0))), None);
    assert_eq!(tl.export_span(Some((90.0, 120.0))), None);
}

#[test]
fn slice_shifts_markers_into_the_window_and_drops_the_rest() {
    let mk = |t: f64, n: &str| Marker {
        id: Uuid::new_v4(),
        time: t,
        name: n.into(),
        ..Default::default()
    };
    let tl = Timeline {
        tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 20.0)])],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: vec![mk(1.0, "before"), mk(4.0, "inside"), mk(9.0, "after")],
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let s = tl.slice(3.0, 7.0);
    let names: Vec<_> = s.markers.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["inside"], "only markers within the window survive");
    // Shifted with everything else — otherwise a range export desyncs them.
    assert!((s.markers[0].time - 1.0).abs() < 1e-9, "{}", s.markers[0].time);
}

#[test]
fn slice_carries_track_flags_through() {
    let tl = Timeline {
        tracks: vec![Track {
            muted: true,
            locked: true,
            duck: true,
            ..track(StreamKind::Audio, "A1", vec![clip_at(0.0, 10.0)])
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let s = tl.slice(2.0, 6.0);
    assert!(s.tracks[0].muted && s.tracks[0].locked && s.tracks[0].duck);
}

#[test]
fn diff_says_when_only_the_easing_of_keys_changed() {
    let mut clip = clip_at(0.0, 4.0);
    clip.keyframes = vec![
        eased(0.0, 1.0, Easing::Linear),
        eased(2.0, 2.0, Easing::Linear),
        eased(3.0, 3.0, Easing::Linear),
    ];
    let tl = Timeline {
        tracks: vec![track(StreamKind::Video, "V1", vec![clip])],
        ..Timeline::new()
    };
    let summary = |after: &Timeline| tl.diff(after).entries[0].detail.clone().unwrap_or_default();
    let mut one = tl.clone();
    one.tracks[0].clips[0].keyframes[0].easing = Easing::Hold;
    assert!(summary(&one).contains("easing changed on 1 keyframe"), "{}", summary(&one));
    assert!(!summary(&one).contains("retimed"), "{}", summary(&one));
    let mut two = one.clone();
    two.tracks[0].clips[0].keyframes[1].easing = Easing::EaseOut;
    assert!(summary(&two).contains("easing changed on 2 keyframes"), "{}", summary(&two));
    // A key that moved is still a retime, and both are said when both happened.
    let mut moved = two.clone();
    moved.tracks[0].clips[0].keyframes[2].time = 3.5;
    assert!(summary(&moved).contains("keyframes retimed"), "{}", summary(&moved));
    assert!(
        summary(&moved).contains("easing changed on 2 keyframes"),
        "{}",
        summary(&moved)
    );
    let mut only_moved = tl.clone();
    only_moved.tracks[0].clips[0].keyframes[2].time = 3.5;
    assert!(summary(&only_moved).contains("keyframes retimed"), "{}", summary(&only_moved));
    assert!(!summary(&only_moved).contains("easing"), "{}", summary(&only_moved));
}

#[test]
fn an_untouched_timeline_diffs_to_nothing() {
    let tl = Timeline {
        tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0), clip_at(4.0, 3.0)])],
        ..Timeline::new()
    };
    let diff = tl.diff(&tl.clone());
    assert!(diff.is_empty());
    assert_eq!(diff.headline(), "No changes");
    assert_eq!(diff.clips_before, 2);
}

#[test]
fn diff_names_the_move_the_trim_and_the_removal() {
    let tl = Timeline {
        tracks: vec![track(
            StreamKind::Video,
            "V1",
            vec![clip_at(0.0, 4.0), clip_at(4.0, 3.0), clip_at(7.0, 2.0)],
        )],
        ..Timeline::new()
    };
    let mut after = tl.clone();
    {
        let clips = &mut after.tracks[0].clips;
        clips[0].source_out = 2.5; // retrim
        clips[1].timeline_start = 5.0; // move
        clips.remove(2); // cut
    }

    let diff = tl.diff(&after);
    let kinds: Vec<DiffKind> = diff.entries.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![DiffKind::ClipRemoved, DiffKind::ClipRetrimmed, DiffKind::ClipMoved]
    );
    assert_eq!(diff.clips_before, 3);
    assert_eq!(diff.clips_after, 2);
    // The retrim reports the source window, and the entry carries the clip
    // so the reviewer can jump to it.
    assert!(
        diff.entries[1].summary.contains("4.0s → 2.5s (-1.5s)"),
        "{}",
        diff.entries[1].summary
    );
    assert_eq!(diff.entries[1].clip_id, Some(after.tracks[0].clips[0].id));
    assert!(
        diff.entries[2].summary.contains("0:04.0 → 0:05.0"),
        "{}",
        diff.entries[2].summary
    );
    // The headline leads with what the edit did to the runtime.
    assert_eq!(diff.headline(), "3 changes · 0:09.0 → 0:08.0 (-1.0s) · 3 → 2 clips");
}

#[test]
fn a_removed_track_is_one_change_not_one_per_clip() {
    let tl = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)]),
            track(StreamKind::Audio, "A1", vec![clip_at(0.0, 4.0), clip_at(4.0, 4.0)]),
        ],
        ..Timeline::new()
    };
    let mut after = tl.clone();
    after.tracks.remove(1);

    let diff = tl.diff(&after);
    assert_eq!(diff.entries.len(), 1);
    assert_eq!(diff.entries[0].kind, DiffKind::TrackRemoved);
    assert!(
        diff.entries[0].summary.contains("A1 (2 clips)"),
        "{}",
        diff.entries[0].summary
    );
}

#[test]
fn a_clip_dragged_to_another_track_reads_as_one_move() {
    let mut tl = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)]),
            track(StreamKind::Video, "V2", vec![]),
        ],
        ..Timeline::new()
    };
    tl.tracks[1].kind = StreamKind::Video;
    let mut after = tl.clone();
    let clip = after.tracks[0].clips.remove(0);
    after.tracks[1].clips.push(clip);

    let diff = tl.diff(&after);
    assert_eq!(diff.entries.len(), 1);
    assert_eq!(diff.entries[0].kind, DiffKind::ClipMoved);
    assert!(
        diff.entries[0].summary.contains("from V1 to V2"),
        "{}",
        diff.entries[0].summary
    );
}

#[test]
fn diff_details_what_changed_on_a_clip() {
    let tl = Timeline {
        tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)])],
        ..Timeline::new()
    };
    let mut after = tl.clone();
    {
        let c = &mut after.tracks[0].clips[0];
        c.volume = 0.4;
        c.speed = 2.0;
        c.effects.push(VideoEffect::Vignette);
        c.enabled = false;
    }

    let diff = tl.diff(&after);
    assert_eq!(diff.entries.len(), 1);
    assert_eq!(diff.entries[0].kind, DiffKind::ClipChanged);
    let detail = diff.entries[0].detail.clone().unwrap();
    assert!(detail.contains("volume 100% → 40%"), "{detail}");
    assert!(detail.contains("speed 1.00× → 2.00×"), "{detail}");
    assert!(detail.contains("disabled"), "{detail}");
    assert!(detail.contains("video effects none → vignette"), "{detail}");
}

#[test]
fn diff_sees_a_mask_and_the_track_mix() {
    // An agent proposal that only masks a clip or rides a fader must not
    // diff as empty — apply_staged discards an empty proposal.
    let tl = Timeline {
        tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)])],
        ..Timeline::new()
    };
    let mut after = tl.clone();
    after.tracks[0].clips[0].mask = Some(Mask::default());
    let diff = tl.diff(&after);
    assert_eq!(diff.entries.len(), 1, "{diff:?}");
    assert!(diff.entries[0].detail.as_deref().unwrap().contains("masked (rect)"));

    let mut after = tl.clone();
    after.tracks[0].volume = 0.5;
    after.tracks[0].pan = -0.3;
    let diff = tl.diff(&after);
    assert_eq!(diff.entries.len(), 1, "{diff:?}");
    let detail = diff.entries[0].detail.clone().unwrap();
    assert!(detail.contains("level 100% → 50%"), "{detail}");
    assert!(detail.contains("pan 0.00 → -0.30"), "{detail}");
}

#[test]
fn diff_sees_a_blend_mode() {
    let tl = Timeline {
        tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)])],
        ..Timeline::new()
    };
    let mut after = tl.clone();
    after.tracks[0].clips[0].blend_mode = BlendMode::HardLight;
    let diff = tl.diff(&after);
    assert_eq!(diff.entries.len(), 1, "{diff:?}");
    let detail = diff.entries[0].detail.clone().unwrap();
    assert!(detail.contains("blend normal → hard light"), "{detail}");
}

#[test]
fn blend_modes_name_themselves_one_way_and_reach_ffmpeg_swapped_where_its_names_are() {
    for mode in BlendMode::ALL {
        let json = serde_json::to_value(mode).unwrap();
        assert_eq!(json, mode.as_str(), "the stored name is the one as_str gives");
        assert_eq!(BlendMode::parse(mode.as_str()), Some(mode));
        assert_eq!(mode.ffmpeg().is_none(), mode.is_normal());
    }
    assert_eq!(BlendMode::parse("Linear Dodge"), Some(BlendMode::Add));
    assert_eq!(BlendMode::parse("hard-light"), Some(BlendMode::HardLight));
    assert_eq!(BlendMode::parse("soft_light"), None, "FFmpeg's soft light is another formula");
    // FFmpeg's `overlay` conditions on its top input (the clip): the textbook hard light.
    assert_eq!(BlendMode::Overlay.ffmpeg(), Some("hardlight"));
    assert_eq!(BlendMode::HardLight.ffmpeg(), Some("overlay"));
    // Overlay is hard light with the roles swapped; both keep mid-grey of the clip neutral or not.
    let (dark, light) = (0.2, 0.9);
    assert_eq!(BlendMode::Overlay.apply(dark, light), BlendMode::HardLight.apply(light, dark));
    assert!((BlendMode::Multiply.apply(0.5, 0.5) - 0.25).abs() < 1e-12);
    assert!((BlendMode::Screen.apply(0.5, 0.5) - 0.75).abs() < 1e-12);
    assert_eq!(BlendMode::Add.apply(0.7, 0.6), 1.0);
    // A clip without the field is normal, and normal writes nothing.
    let mut clip = clip_at(0.0, 4.0);
    assert!(!serde_json::to_string(&clip).unwrap().contains("blend_mode"));
    clip.blend_mode = BlendMode::Screen;
    let json = serde_json::to_string(&clip).unwrap();
    assert!(json.contains("\"blend_mode\":\"screen\""), "{json}");
    let unknown = json.replace("\"screen\"", "\"soft_light\"");
    assert!(serde_json::from_str::<Clip>(&unknown).is_err(), "only the listed modes read");
}

#[test]
fn diff_covers_overlays_markers_and_the_delivery_frame() {
    let tl = Timeline::new();
    let mut after = tl.clone();
    after.overlays.push(TextOverlay::new("Hello", 1.0, 3.0));
    after.markers.push(Marker {
        id: Uuid::new_v4(),
        time: 72.0,
        name: "the laugh".to_string(),
        ..Default::default()
    });
    after.format = Some(Delivery::new(1080, 1920, Fit::Cover));

    let diff = tl.diff(&after);
    let kinds: Vec<DiffKind> = diff.entries.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![DiffKind::OverlayAdded, DiffKind::MarkerAdded, DiffKind::FormatChanged]
    );
    assert!(diff.entries[1].summary.contains("1:12.0"), "{}", diff.entries[1].summary);
    assert!(
        diff.entries[2].summary.contains("1080x1920 (cover)"),
        "{}",
        diff.entries[2].summary
    );
    // Every entry lands in the rendered summary the agent reads back.
    assert_eq!(diff.summary().lines().count(), 4);
}

#[test]
fn diff_names_a_frame_rate_that_is_the_only_change_to_the_delivery() {
    let mut before = Timeline::new();
    before.format = Some(Delivery::new(1080, 1920, Fit::Cover));
    let mut after = before.clone();
    after.format = before.format.map(|d| d.with_fps(Some(29.97)).unwrap());
    let diff = before.diff(&after);
    assert_eq!(diff.entries.len(), 1, "{diff:?}");
    assert_eq!(diff.entries[0].kind, DiffKind::FormatChanged);
    assert_eq!(
        diff.entries[0].summary,
        "Delivery frame 1080x1920 (cover) → 1080x1920 (cover, 29.97 fps)"
    );
}

#[test]
fn delivery_frame_rates_are_held_to_a_sane_range_and_ntsc_is_made_exact() {
    for ok in [1.0, 12.0, 24.0, 25.0, 30.0, 50.0, 60.0, 120.0, 240.0, 33.5] {
        assert_eq!(check_delivery_fps(ok).unwrap(), ok, "{ok}");
    }
    for bad in [
        0.0,
        0.99,
        -24.0,
        240.5,
        1000.0,
        1e9,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        assert!(check_delivery_fps(bad).is_err(), "{bad} is not a delivery rate");
    }
    // The short decimals people write are the exact NTSC rates...
    for (typed, exact) in [
        (23.976, 24_000.0 / 1001.0),
        (23.98, 24_000.0 / 1001.0),
        (29.97, 30_000.0 / 1001.0),
        (47.952, 48_000.0 / 1001.0),
        (59.94, 60_000.0 / 1001.0),
        (119.88, 120_000.0 / 1001.0),
        (239.76, 240_000.0 / 1001.0),
        (30_000.0 / 1001.0, 30_000.0 / 1001.0),
    ] {
        assert_eq!(check_delivery_fps(typed).unwrap(), exact, "{typed}");
    }
    // ...and nothing near a whole rate is dragged onto an NTSC one.
    for plain in [24.0, 29.0, 29.9, 30.0, 59.9, 60.0, 23.9, 25.0] {
        assert_eq!(check_delivery_fps(plain).unwrap(), plain, "{plain}");
    }
}

#[test]
fn a_delivery_with_no_rate_stores_exactly_what_it_always_did() {
    let plain = Delivery::new(1080, 1920, Fit::Cover);
    assert_eq!(
        serde_json::to_string(&plain).unwrap(),
        r#"{"width":1080,"height":1920,"fit":"cover"}"#
    );
    let rated = plain.with_fps(Some(50.0)).unwrap();
    assert_eq!(
        serde_json::to_string(&rated).unwrap(),
        r#"{"width":1080,"height":1920,"fit":"cover","fps":50.0}"#
    );
    // Both read back, and one stored before frame rates existed follows the footage.
    assert_eq!(
        serde_json::from_str::<Delivery>(r#"{"width":1080,"height":1920,"fit":"cover"}"#).unwrap(),
        plain
    );
    assert_eq!(
        serde_json::from_str::<Delivery>(&serde_json::to_string(&rated).unwrap()).unwrap(),
        rated
    );
    assert_eq!(plain.rate(), None);
    assert_eq!(rated.rate(), Some(50.0));
    assert!(plain.with_fps(Some(0.0)).is_err());
    assert_eq!(rated.with_fps(None).unwrap(), plain);
}

#[test]
fn diff_overlay_entries_name_the_overlay() {
    let mut before = Timeline::new();
    let kept = TextOverlay::new("Kept", 0.0, 2.0);
    let removed = TextOverlay::new("Gone", 5.0, 7.0);
    before.overlays.push(kept.clone());
    before.overlays.push(removed.clone());

    let mut after = Timeline::new();
    let mut changed = kept.clone();
    changed.start = 0.5;
    let added = TextOverlay::new("New", 3.0, 4.0);
    after.overlays.push(changed);
    after.overlays.push(added.clone());

    let diff = before.diff(&after);
    let id_of = |kind: DiffKind| diff.entries.iter().find(|e| e.kind == kind).and_then(|e| e.overlay_id);
    assert_eq!(id_of(DiffKind::OverlayChanged), Some(kept.id));
    assert_eq!(id_of(DiffKind::OverlayAdded), Some(added.id));
    assert_eq!(id_of(DiffKind::OverlayRemoved), Some(removed.id));
    // It rides the JSON the surfaces send, and is left out of an entry that has none.
    let entry = diff.entries.iter().find(|e| e.kind == DiffKind::OverlayAdded).unwrap();
    let json = serde_json::to_value(entry).unwrap();
    assert_eq!(json["overlay_id"], serde_json::json!(added.id.to_string()), "{json}");
    let marker = DiffEntry::new(DiffKind::MarkerAdded, "m".to_string());
    assert!(serde_json::to_value(&marker).unwrap().get("overlay_id").is_none());
}

// ---- smart crop ---------------------------------------------------------

/// A map whose salience sits in one horizontal band of `cols`, so a test can
/// say "the subject is on the left third" and nothing else.
fn map_with_column_band(cols: usize, from: usize, to: usize) -> SalienceMap {
    let rows = 4;
    let mut cells = vec![0.01f32; cols * rows];
    for r in 0..rows {
        for c in from..to {
            cells[r * cols + c] = 1.0;
        }
    }
    SalienceMap::new(cols, rows, cells)
}

#[test]
fn a_matching_aspect_needs_no_crop() {
    let map = map_with_column_band(32, 0, 32);
    // 1080x1920 delivered at 9:16, and 1920x1080 at a 1280x720 frame.
    assert!(map.crop_for(1080, 1920, 1080.0 / 1920.0).is_none());
    assert!(map.crop_for(1920, 1080, 1280.0 / 720.0).is_none());
}

#[test]
fn a_vertical_delivery_crops_width_toward_the_subject() {
    // 16:9 footage into a 9:16 frame with the subject in the left third.
    let map = map_with_column_band(48, 4, 16);
    let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
    assert_eq!((crop.top, crop.bottom), (0.0, 0.0));
    // 9:16 of 16:9 keeps 0.3164 of the width; the rest is cut.
    assert!((crop.left + crop.right - (1.0 - 1080.0 * 1080.0 / (1920.0 * 1920.0))).abs() < 1e-6);
    // The kept window contains the band, which a centre crop would miss.
    assert!(crop.left < 4.0 / 48.0 && 1.0 - crop.right > 16.0 / 48.0, "{crop:?}");
    assert!(!crop.is_centered());
    assert!(crop.offset < 0.0, "a left-hand subject pulls the window left");
}

#[test]
fn flat_salience_falls_back_to_the_centre_crop() {
    let map = map_with_column_band(48, 0, 48);
    let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
    assert!((crop.left - crop.right).abs() < 1e-9, "{crop:?}");
    assert!(crop.is_centered());
}

#[test]
fn an_off_centre_subject_still_loses_to_a_hard_pull_it_cannot_earn() {
    // Salience a hair off centre: the centre bias should hold the window put
    // rather than pan for a rounding difference.
    let map = map_with_column_band(48, 23, 27);
    let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
    assert!(crop.is_centered(), "{crop:?}");
}

#[test]
fn a_landscape_delivery_crops_height_of_vertical_footage() {
    // 9:16 footage into 16:9, subject in the top rows.
    let cols = 4;
    let rows = 32;
    let mut cells = vec![0.01f32; cols * rows];
    for r in 2..8 {
        for c in 0..cols {
            cells[r * cols + c] = 1.0;
        }
    }
    let map = SalienceMap::new(cols, rows, cells);
    let crop = map.crop_for(1080, 1920, 1920.0 / 1080.0).expect("crops");
    assert_eq!((crop.left, crop.right), (0.0, 0.0));
    // The search grid is finer than a bucket but not exact, so allow the top
    // edge to land a hair inside the band it is framing.
    assert!(crop.top <= 2.0 / 32.0 + 0.01 && 1.0 - crop.bottom > 8.0 / 32.0, "{crop:?}");
}

#[test]
fn a_map_with_nothing_in_it_still_yields_the_centre_crop() {
    for map in [
        SalienceMap::default(),
        SalienceMap::new(8, 2, vec![0.0; 16]),
        SalienceMap::new(8, 2, vec![0.0; 3]),
    ] {
        let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
        assert!(crop.is_centered(), "{crop:?}");
        assert!((crop.left - crop.right).abs() < 1e-9);
    }
}

#[test]
fn a_degenerate_aspect_is_refused_rather_than_guessed() {
    let map = map_with_column_band(8, 0, 4);
    assert!(map.crop_for(1920, 1080, 0.0).is_none());
    assert!(map.crop_for(1920, 1080, f64::NAN).is_none());
    assert!(map.crop_for(0, 1080, 1.0).is_none());
}

fn seg(start: f64, end: f64, text: &str) -> TranscriptSegment {
    TranscriptSegment {
        start,
        end,
        text: text.to_string(),
        ..Default::default()
    }
}

fn captioned(timeline: &Timeline, asset: Uuid, segments: Vec<TranscriptSegment>) -> Vec<(String, f64, f64)> {
    let mut map = HashMap::new();
    map.insert(asset, segments);
    timeline
        .captions(&map, CaptionOptions::default())
        .into_iter()
        .map(|o| (o.text, (o.start * 100.0).round() / 100.0, (o.end * 100.0).round() / 100.0))
        .collect()
}

fn one_clip(clip: Clip) -> Timeline {
    let mut track = Track::new(StreamKind::Video, "V1");
    track.clips = vec![clip];
    Timeline {
        tracks: vec![track],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

fn timed(text: &str, words: &[(&str, f64, f64)]) -> TranscriptSegment {
    let (start, end) = (words[0].1, words[words.len() - 1].2);
    TranscriptSegment {
        start,
        end: end + 1.0,
        text: text.to_string(),
        words: words
            .iter()
            .map(|(t, a, b)| Word {
                text: t.to_string(),
                start: *a,
                end: *b,
                ..Word::default()
            })
            .collect(),
        speaker: None,
    }
}

#[test]
fn word_timed_lines_follow_the_words_and_their_pauses() {
    let asset = Uuid::new_v4();
    let timeline = one_clip(Clip::new(asset, 0.0, 20.0, 0.0));
    let line = timed(
        "one two three four five six",
        &[
            ("one", 0.0, 0.3),
            ("two", 0.3, 0.6),
            ("three", 0.6, 1.0),
            ("four", 1.0, 1.4),
            ("five", 4.0, 4.4),
            ("six", 4.4, 5.0),
        ],
    );
    assert_eq!(
        captioned(&timeline, asset, vec![line]),
        vec![
            ("one two three four".to_string(), 0.0, 1.9),
            ("five six".to_string(), 4.0, 5.5)
        ]
    );
}

#[test]
fn a_short_word_timed_line_merges_into_its_neighbour() {
    let asset = Uuid::new_v4();
    let timeline = one_clip(Clip::new(asset, 0.0, 20.0, 0.0));
    let opts = CaptionOptions {
        style: CaptionStyle::Lines,
        max_words: Some(1),
        ..CaptionOptions::default()
    };
    let line = timed("a long", &[("a", 0.0, 0.1), ("long", 0.1, 1.0)]);
    let got: Vec<(String, f64, f64)> = timeline
        .captions(&HashMap::from([(asset, vec![line])]), opts)
        .into_iter()
        .map(|o| (o.text, o.start, o.end))
        .collect();
    assert_eq!(got, vec![("a long".to_string(), 0.0, 1.5)]);
}

#[test]
fn word_timed_lines_project_through_trim_and_speed() {
    let asset = Uuid::new_v4();
    let mut clip = Clip::new(asset, 2.0, 10.0, 5.0);
    clip.speed = 2.0;
    let timeline = one_clip(clip);
    let line = timed("cut kept", &[("cut", 1.0, 1.9), ("kept", 4.0, 5.0)]);
    // `cut` is spoken before the in-point (its hold reaching past it shows nothing);
    // `kept` lands at 5 + (4 - 2) / 2 = 6.
    let got: Vec<(String, f64, f64)> = timeline
        .captions(
            &HashMap::from([(asset, vec![line])]),
            CaptionOptions::styled(CaptionStyle::WordPunch),
        )
        .into_iter()
        .map(|o| (o.text, o.start, o.end))
        .collect();
    assert_eq!(got, vec![("kept".to_string(), 6.0, 6.75)]);
}

#[test]
fn source_span_maps_through_trim_speed_and_reverse() {
    let asset = Uuid::new_v4();
    // Trimmed: the asset's 10s starts at the clip's in-point, placed at 4s.
    let mut clip = Clip::new(asset, 10.0, 20.0, 4.0);
    let r = clip.source_span_to_timeline(12.0, 14.0);
    assert!((r.start - 6.0).abs() < 1e-9, "{r:?}");
    assert!((r.end - 8.0).abs() < 1e-9, "{r:?}");

    // Double speed halves the distance from the in-point.
    clip.speed = 2.0;
    let r = clip.source_span_to_timeline(12.0, 14.0);
    assert!((r.start - 5.0).abs() < 1e-9, "{r:?}");
    assert!((r.end - 6.0).abs() < 1e-9, "{r:?}");

    // Reversed: the source's tail is heard first, and the range stays ordered.
    clip.speed = -1.0;
    let r = clip.source_span_to_timeline(12.0, 14.0);
    assert!((r.start - 10.0).abs() < 1e-9, "{r:?}");
    assert!((r.end - 12.0).abs() < 1e-9, "{r:?}");
    assert!(r.end > r.start);
}

#[test]
fn covers_source_is_the_clips_own_window() {
    let clip = Clip::new(Uuid::new_v4(), 10.0, 20.0, 0.0);
    assert!(clip.covers_source(12.0, 14.0));
    assert!(clip.covers_source(8.0, 11.0), "straddling the in-point still shows");
    assert!(!clip.covers_source(0.0, 10.0), "ending exactly at the in-point shows nothing");
    assert!(!clip.covers_source(20.0, 25.0));
}

// ---- the master bus ---------------------------------------------------

#[test]
fn an_untouched_master_is_not_written_and_an_old_file_reads_back_neutral() {
    let tl = Timeline::new();
    let json = serde_json::to_string(&tl).unwrap();
    assert!(
        !json.contains("master"),
        "a default master must not change the saved JSON: {json}"
    );
    // A timeline saved before the master existed.
    let old: Timeline = serde_json::from_str(r#"{"tracks":[]}"#).unwrap();
    assert!(old.master.is_default());
    assert!(old.master.is_neutral());
}

#[test]
fn a_master_round_trips_and_a_partial_one_fills_from_the_defaults() {
    let mut tl = Timeline::new();
    tl.master = MasterBus {
        duck_depth_db: None,
        true_peak: false,
        volume: 0.7,
        limiter: true,
        ceiling_db: -3.0,
    };
    let back: Timeline = serde_json::from_str(&serde_json::to_string(&tl).unwrap()).unwrap();
    assert_eq!(back.master, tl.master);
    let partial: Timeline = serde_json::from_str(r#"{"tracks":[],"master":{"limiter":true}}"#).unwrap();
    assert_eq!(
        partial.master,
        MasterBus {
            duck_depth_db: None,
            true_peak: false,
            volume: 1.0,
            limiter: true,
            ceiling_db: -1.5
        }
    );
}

#[test]
fn the_master_is_neutral_until_it_changes_the_mix() {
    let mut m = MasterBus::default();
    assert!(m.is_neutral() && m.is_default());
    // A stored ceiling the limiter is not using changes nothing in the graph...
    m.ceiling_db = -6.0;
    assert!(m.is_neutral());
    // ...but is still something to save.
    assert!(!m.is_default());
    m.limiter = true;
    assert!(!m.is_neutral());
    m.limiter = false;
    m.volume = 0.5;
    assert!(!m.is_neutral());
    m.volume = 0.0;
    assert!(!m.is_neutral(), "a muted master is a master");
}

#[test]
fn a_master_from_a_hand_edited_file_is_made_safe_for_the_graph() {
    let m = MasterBus {
        duck_depth_db: None,
        true_peak: false,
        volume: f64::NAN,
        limiter: true,
        ceiling_db: f64::INFINITY,
    };
    assert_eq!(m.safe_volume(), 1.0);
    assert_eq!(m.safe_ceiling_db(), MASTER_DEFAULT_CEILING_DB);
    let m = MasterBus {
        duck_depth_db: None,
        true_peak: false,
        volume: 99.0,
        limiter: true,
        ceiling_db: -90.0,
    };
    assert_eq!(m.safe_volume(), MASTER_MAX_VOLUME);
    assert_eq!(m.safe_ceiling_db(), MASTER_MIN_CEILING_DB);
    let m = MasterBus {
        volume: -3.0,
        ..MasterBus::default()
    };
    assert_eq!(m.safe_volume(), 0.0);
    assert!((MasterBus::default().limit_linear() - 0.841_395_142).abs() < 1e-6, "-1.5 dB");
}

#[test]
fn the_master_follows_the_cut_into_every_render_of_it() {
    let master = MasterBus {
        duck_depth_db: None,
        true_peak: false,
        volume: 0.5,
        limiter: true,
        ceiling_db: -2.0,
    };
    let tl = Timeline {
        tracks: vec![track(StreamKind::Audio, "A1", vec![clip_at(0.0, 8.0)])],
        master,
        thumbnails: Vec::new(),
        ..Timeline::new()
    };
    assert_eq!(tl.for_render().master, master, "muted tracks are dropped, the mix is not");
    assert_eq!(
        tl.slice(2.0, 6.0).master,
        master,
        "a range export of a limited master is limited"
    );
    assert_eq!(
        tl.for_delivery(Delivery::new(1080, 1920, Fit::Cover)).master,
        master,
        "a variant changes the picture, not the mix"
    );
}

#[test]
fn diff_sees_the_master_bus() {
    // An agent proposal that only rides the master fader or turns the limiter
    // on must not diff as empty — apply_staged discards an empty proposal.
    let tl = Timeline::new();
    assert!(tl.diff(&tl.clone()).is_empty());

    let mut after = tl.clone();
    after.master.volume = 0.7;
    let diff = tl.diff(&after);
    assert_eq!(diff.entries.len(), 1, "{diff:?}");
    assert_eq!(diff.entries[0].kind, DiffKind::MasterChanged);
    assert_eq!(diff.entries[0].detail.as_deref(), Some("level 100% → 70%"));

    let mut limited = tl.clone();
    limited.master.limiter = true;
    let detail = tl.diff(&limited).entries[0].detail.clone().unwrap();
    assert_eq!(detail, "limiter on at -1.5 dB");
    // Moving the ceiling matters only while the limiter is on.
    let mut moved = limited.clone();
    moved.master.ceiling_db = -3.0;
    assert_eq!(
        limited.diff(&moved).entries[0].detail.as_deref(),
        Some("limiter ceiling -1.5 → -3.0 dB")
    );
    let mut gated = tl.clone();
    gated.master.duck_depth_db = Some(-12.0);
    assert_eq!(
        tl.diff(&gated).entries[0].detail.as_deref(),
        Some("duck compressor → speech gate -12 dB")
    );
    let mut deeper = gated.clone();
    deeper.master.duck_depth_db = Some(-18.0);
    assert_eq!(
        gated.diff(&deeper).entries[0].detail.as_deref(),
        Some("duck speech gate -12 dB → speech gate -18 dB")
    );
    let mut parked = tl.clone();
    parked.master.ceiling_db = -3.0;
    assert!(
        tl.diff(&parked).is_empty(),
        "a ceiling nothing is using is not a change to review"
    );
    // Switching off reads as that, whatever the ceiling.
    assert_eq!(limited.diff(&tl).entries[0].detail.as_deref(), Some("limiter off"));
}

// ---- levels -----------------------------------------------------------

fn reading(i: Option<f64>, tp: Option<f64>) -> LevelReading {
    LevelReading {
        integrated_lufs: i,
        true_peak_dbtp: tp,
        ..LevelReading::default()
    }
}

fn strip(name: &str, peak: f64) -> TrackLevels {
    TrackLevels {
        track_id: Uuid::new_v4(),
        name: name.into(),
        kind: StreamKind::Audio,
        ducked: false,
        heard: true,
        level: Some(LevelReading {
            peak_dbfs: Some(peak),
            ..LevelReading::default()
        }),
    }
}

#[test]
fn the_levels_notes_judge_the_mix_against_the_streaming_target() {
    let notes = |i, tp| Levels::new(10.0, Some(reading(i, tp)), Vec::new(), false, &MasterBus::default()).notes;
    assert!(notes(Some(-14.0), Some(-3.0))[0].contains("close to"), "on target");
    assert!(notes(Some(-13.2), Some(-3.0))[0].contains("close to"), "within a LU of it");
    assert!(notes(Some(-9.0), Some(-3.0))[0].contains("5.0 LU over"), "too loud");
    assert!(notes(Some(-19.0), Some(-3.0))[0].contains("5.0 LU under"), "too quiet");
    assert!(
        notes(Some(-16.5), Some(-3.0))[0].contains("close to"),
        "a little under is fine"
    );
    assert!(notes(None, None)[0].contains("silent"));
    // Over the true-peak ceiling is its own note, and names the fix.
    let hot = notes(Some(-14.0), Some(0.4));
    assert_eq!(hot.len(), 2, "{hot:?}");
    assert!(
        hot[1].contains("0.4 dBTP") && hot[1].contains("set_master_limiter"),
        "{hot:?}"
    );
    assert_eq!(notes(Some(-14.0), Some(-1.0)).len(), 1, "exactly at the ceiling is allowed");
}

#[test]
fn the_levels_notes_name_a_track_over_full_scale() {
    let tracks = vec![strip("A1", -6.0), strip("Music", 1.5)];
    let bus = MasterBus::default();
    let notes = Levels::new(10.0, Some(reading(Some(-14.0), Some(-3.0))), tracks, false, &bus).notes;
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes[1].contains("Music") && notes[1].contains("+1.5 dBFS"), "{notes:?}");
    // The mix is float, so a hot strip is not a clip yet: the note must not claim one.
    assert!(!notes[1].contains("will clip"), "{notes:?}");
    // No audio at all is one plain note.
    assert_eq!(
        Levels::new(0.0, None, Vec::new(), false, &bus).notes,
        ["The cut has no audio to measure."]
    );
}

#[test]
fn a_true_peak_over_the_ceiling_with_the_limiter_on_lowers_the_ceiling() {
    // The limiter holds the sample peak, so "turn on the limiter" is advice already
    // taken: an agent following it again would loop. It is told how far to lower the
    // ceiling instead — the overshoot past -1 dBTP, and half a decibel more.
    let on = |ceiling_db| MasterBus {
        limiter: true,
        ceiling_db,
        ..MasterBus::default()
    };
    let notes = |tp, bus: &MasterBus| Levels::new(10.0, Some(reading(Some(-14.0), Some(tp))), Vec::new(), false, bus).notes;

    // 11 kHz at a -1 dBFS ceiling, as measured: -0.2 dBTP is 0.8 over, so -2.3.
    let n = notes(-0.2, &on(-1.0));
    assert_eq!(n.len(), 2, "{n:?}");
    assert!(!n[1].contains("Turn on the master limiter"), "{n:?}");
    assert!(
        n[1].contains("-0.2 dBTP") && n[1].contains("-1.0 dBFS") && n[1].contains("about -2.3 dBFS"),
        "{n:?}"
    );
    assert!(n[1].contains("set_master_limiter"), "{n:?}");
    // Word for word the string in the frontend's `levels.test.ts` (the harness mirrors this).
    assert_eq!(
        n[1],
        "True peak -0.2 dBTP is over the -1 dBTP platforms ask for and can clip when re-encoded. The master limiter is already \
             on, but it holds the sample peak at -1.0 dBFS and the peak between samples runs above that. Turn on true-peak \
             limiting (set_master_true_peak), or lower its ceiling to about -2.3 dBFS (set_master_limiter)."
    );
    // Already limiting the true peak: only the ceiling is left to lower.
    let tp_on = |ceiling_db| MasterBus {
        true_peak: true,
        ..on(ceiling_db)
    };
    assert_eq!(
        notes(-0.2, &tp_on(-1.0))[1],
        "True peak -0.2 dBTP is over the -1 dBTP platforms ask for and can clip when re-encoded. The master limiter is already \
             on and holds the true peak at -1.0 dBFS. Lower its ceiling to about -2.3 dBFS (set_master_limiter)."
    );
    let floor_tp = notes(0.4, &tp_on(MASTER_MIN_CEILING_DB));
    assert!(
        floor_tp[1].contains("holding the true peak at its lowest ceiling"),
        "{floor_tp:?}"
    );
    assert!(!floor_tp[1].contains("set_master_true_peak"), "{floor_tp:?}");
    // From the default ceiling: 0.4 dBTP is 1.4 over.
    assert!(
        notes(0.4, &on(MASTER_DEFAULT_CEILING_DB))[1].contains("about -3.4 dBFS"),
        "from -1.5"
    );
    // Never below what the limiter can be given; at the floor there is nothing to lower.
    assert!(notes(10.0, &on(-20.0))[1].contains("about -24.0 dBFS"));
    let floor = notes(0.4, &on(MASTER_MIN_CEILING_DB));
    assert!(
        floor[1].contains("lowest ceiling") && !floor[1].contains("about"),
        "{floor:?}"
    );
    // A ceiling a `.kerf` file stored out of range reads as the engine clamps it.
    assert!(notes(0.4, &on(f64::NAN))[1].contains("about -3.4 dBFS"));
    // With the limiter off the advice is still to turn it on, and a stored ceiling alone is not "on".
    let off = MasterBus {
        ceiling_db: -6.0,
        ..MasterBus::default()
    };
    assert_eq!(
        notes(0.4, &MasterBus::default())[1],
        "True peak 0.4 dBTP is over the -1 dBTP platforms ask for and can clip when re-encoded. Turn on the master limiter \
             (set_master_limiter) with true-peak limiting (set_master_true_peak), or lower the master."
    );
    assert!(notes(0.4, &off)[1].contains("Turn on the master limiter"));
    // Under the line: no note, limiter or not.
    assert_eq!(notes(-1.0, &on(-1.5)).len(), 1);
}

#[test]
fn a_track_pan_is_a_balance_and_centre_is_exactly_unity() {
    let mut t = Track::new(StreamKind::Audio, "A1");
    assert_eq!(t.pan_gains(), (1.0, 1.0), "an untouched track must not be touched");
    t.pan = -1.0;
    assert_eq!(t.pan_gains(), (1.0, 0.0), "hard left keeps the left at unity");
    t.pan = 1.0;
    assert_eq!(t.pan_gains(), (0.0, 1.0));
    t.pan = -0.5;
    assert_eq!(t.pan_gains(), (1.0, 0.5));
    // Never a boost: leaning a finished stereo track must not make it louder.
    for p in [-1.0, -0.5, 0.0, 0.25, 1.0] {
        t.pan = p;
        let (l, r) = t.pan_gains();
        assert!(l <= 1.0 && r <= 1.0, "pan {p} boosted to ({l}, {r})");
    }
    // Out of range is clamped rather than inverted.
    t.pan = 9.0;
    assert_eq!(t.pan_gains(), (0.0, 1.0));
}

#[test]
fn every_transition_kind_round_trips_and_knows_its_family() {
    for k in TransitionKind::ALL {
        assert_eq!(TransitionKind::parse(k.as_str()), Some(k), "{k:?} must survive the wire");
        assert!(
            TransitionKind::wire_names().contains(k.as_str()),
            "{k:?} must be listed for a caller"
        );
        // Exactly one family each: a dip has a colour and never moves, a
        // motion transition moves and never dips, a dissolve does neither.
        assert!(
            !(k.dip_color().is_some() && k.slide_from().is_some()),
            "{k:?} cannot both dip and travel"
        );
        assert_eq!(k.dip_color().is_some(), !k.overlaps(), "{k:?}: only a dip skips the overlap");
        assert!(!k.pushes() || k.slide_from().is_some(), "{k:?}: a push must have a direction");
    }
    // A slide and its push travel the same way; the difference is what
    // happens to the outgoing clip, not where the incoming one comes from.
    assert_eq!(TransitionKind::SlideLeft.slide_from(), TransitionKind::PushLeft.slide_from());
    assert!(!TransitionKind::SlideLeft.pushes() && TransitionKind::PushLeft.pushes());
    assert_eq!(TransitionKind::parse("nonsense"), None);
}

#[test]
fn captions_follow_a_trimmed_and_moved_clip() {
    let asset = Uuid::new_v4();
    // The interesting case: the transcript says 30s, the cut says 0s.
    let timeline = one_clip(Clip::new(asset, 30.0, 34.0, 0.0));
    let lines = captioned(&timeline, asset, vec![seg(30.0, 34.0, "one two three four")]);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].0, "one two three four");
    // Timeline time, not the transcript's 30.0.
    assert!(lines[0].1.abs() < 1e-9, "{lines:?}");
    assert!((lines[0].2 - 4.0).abs() < 1e-9, "{lines:?}");
}

#[test]
fn a_sentence_cut_in_half_only_captions_what_survived() {
    let asset = Uuid::new_v4();
    // A four-word line spoken over 0..4s, but the cut keeps only 0..2s.
    let timeline = one_clip(Clip::new(asset, 0.0, 2.0, 0.0));
    let lines = captioned(
        &timeline,
        asset,
        vec![seg(0.0, 4.0, "alpha bravo charlie delta echo foxtrot")],
    );
    assert!(!lines.is_empty());
    // Nothing runs past the end of the clip that carries it.
    for (text, start, end) in &lines {
        assert!(*end <= 2.0 + 1e-9, "{text:?} runs to {end} past the clip");
        assert!(*start >= -1e-9);
    }
    // The words spoken in the discarded half are gone.
    assert!(!lines.iter().any(|(t, _, _)| t.contains("foxtrot")), "{lines:?}");
}

#[test]
fn long_segments_split_into_readable_lines() {
    let asset = Uuid::new_v4();
    let timeline = one_clip(Clip::new(asset, 0.0, 8.0, 0.0));
    let lines = captioned(
        &timeline,
        asset,
        vec![seg(0.0, 8.0, "Today we are talking about non-destructive editing in Kerf")],
    );
    assert!(lines.len() > 1, "a ten-word sentence should not be one caption: {lines:?}");
    for (text, _, _) in &lines {
        assert!(text.split_whitespace().count() <= 4, "{text:?} is too many words");
    }
    // The lines are contiguous, in order, and cover the segment.
    assert!(lines[0].1.abs() < 1e-9);
    assert!((lines.last().unwrap().2 - 8.0).abs() < 1e-9);
    for pair in lines.windows(2) {
        assert!(pair[1].1 >= pair[0].1);
    }
    // Rejoining the lines gives the sentence back, word for word.
    let rejoined = lines.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>().join(" ");
    assert_eq!(rejoined, "Today we are talking about non-destructive editing in Kerf");
}

#[test]
fn fast_speech_merges_rather_than_flickering() {
    let asset = Uuid::new_v4();
    // Eight words in 0.9s: split four ways each line would last ~0.22s.
    let timeline = one_clip(Clip::new(asset, 0.0, 0.9, 0.0));
    let lines = captioned(&timeline, asset, vec![seg(0.0, 0.9, "a b c d e f g h")]);
    for (text, start, end) in &lines {
        assert!(
            end - start >= MIN_CAPTION - 1e-6 || lines.len() == 1,
            "{text:?} flashes for {}s",
            end - start
        );
    }
    // No words were lost to the merging.
    let rejoined = lines.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>().join(" ");
    assert_eq!(rejoined, "a b c d e f g h");
}

#[test]
fn captions_are_ordered_by_the_cut_not_by_the_source() {
    let asset = Uuid::new_v4();
    // The second half of the source is cut to play first.
    let mut track = Track::new(StreamKind::Video, "V1");
    track.clips = vec![Clip::new(asset, 10.0, 12.0, 0.0), Clip::new(asset, 0.0, 2.0, 2.0)];
    let timeline = Timeline {
        tracks: vec![track],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let lines = captioned(&timeline, asset, vec![seg(0.0, 2.0, "first"), seg(10.0, 12.0, "second")]);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0].0, "second", "the reordered cut leads with the later words");
    assert_eq!(lines[1].0, "first");
    assert!(lines[0].1.abs() < 1e-9);
    assert!((lines[1].1 - 2.0).abs() < 1e-9);
}

#[test]
fn two_captions_never_share_the_screen() {
    let asset = Uuid::new_v4();
    // The same footage twice in the cut at different offsets — a callback
    // shot, or a full source parked under the edit. Both would caption the
    // same words on top of each other.
    let mut track = Track::new(StreamKind::Video, "V1");
    track.clips = vec![Clip::new(asset, 3.0, 12.0, 0.0), Clip::new(asset, 0.0, 12.0, 0.0)];
    let timeline = Timeline {
        tracks: vec![track],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let lines = captioned(
        &timeline,
        asset,
        vec![seg(0.0, 6.0, "alpha bravo charlie"), seg(6.0, 12.0, "delta echo foxtrot")],
    );
    assert!(lines.len() > 1, "{lines:?}");
    for pair in lines.windows(2) {
        assert!(
            pair[1].1 >= pair[0].2 - 1e-6,
            "{:?} starts before {:?} is off screen",
            pair[1],
            pair[0]
        );
    }
}

#[test]
fn word_punch_puts_one_word_on_screen_at_a_time() {
    let asset = Uuid::new_v4();
    let timeline = one_clip(Clip::new(asset, 0.0, 5.0, 0.0));
    let mut map = HashMap::new();
    map.insert(asset, vec![seg(0.0, 5.0, "alpha bravo charlie delta echo")]);
    let punched = timeline.captions(&map, CaptionOptions::styled(CaptionStyle::WordPunch));
    assert_eq!(
        punched.iter().map(|o| o.text.as_str()).collect::<Vec<_>>(),
        ["alpha", "bravo", "charlie", "delta", "echo"]
    );
    // The whole look, not just the word count: the line style would leave
    // one word at subtitle size on the bottom edge.
    let layout = CaptionStyle::WordPunch.layout();
    assert!(punched
        .iter()
        .all(|o| o.bold && o.size == layout.size && o.pos_y == layout.pos_y));
    // Each word hands the screen to the next with no gap and no overlap.
    for pair in punched.windows(2) {
        assert!((pair[1].start - pair[0].end).abs() < 1e-9, "{:?}", (&pair[0], &pair[1]));
    }
    // The default style is untouched by any of this.
    let lines = timeline.captions(&map, CaptionOptions::default());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|o| !o.bold));
}

#[test]
fn a_word_too_short_to_read_joins_its_neighbour() {
    let asset = Uuid::new_v4();
    let timeline = one_clip(Clip::new(asset, 0.0, 2.0, 0.0));
    let mut map = HashMap::new();
    // "a" is one character of thirty, so its character share is ~0.07s —
    // two frames, which is a flicker rather than a word.
    map.insert(asset, vec![seg(0.0, 2.0, "a fairly quickly spoken sentence")]);
    let punched = timeline.captions(&map, CaptionOptions::styled(CaptionStyle::WordPunch));
    assert!(
        punched.iter().all(|o| o.end - o.start >= MIN_WORD_CAPTION - 1e-6),
        "{:?}",
        punched.iter().map(|o| (&o.text, o.end - o.start)).collect::<Vec<_>>()
    );
    assert_eq!(punched[0].text, "a fairly", "the flicker merges instead of being dropped");
}

#[test]
fn an_override_moves_one_number_and_leaves_the_style_alone() {
    let asset = Uuid::new_v4();
    let timeline = one_clip(Clip::new(asset, 0.0, 5.0, 0.0));
    let mut map = HashMap::new();
    map.insert(asset, vec![seg(0.0, 5.0, "alpha bravo charlie delta echo")]);
    let opts = CaptionOptions {
        size: Some(0.2),
        ..CaptionOptions::styled(CaptionStyle::WordPunch)
    };
    let punched = timeline.captions(&map, opts);
    assert_eq!(punched.len(), 5, "still one word each");
    assert!(punched.iter().all(|o| o.size == 0.2 && o.bold));
    assert!(punched.iter().all(|o| o.pos_y == CaptionStyle::WordPunch.layout().pos_y));
    // An unusable override falls back to the style rather than through it.
    let junk = CaptionOptions {
        size: Some(f64::NAN),
        pos_y: Some(9.0),
        ..CaptionOptions::styled(CaptionStyle::WordPunch)
    };
    let layout = junk.resolve();
    assert_eq!(layout.size, CaptionStyle::WordPunch.layout().size);
    assert_eq!(layout.pos_y, 1.0);
}

#[test]
fn a_long_word_is_shrunk_to_fit_a_vertical_frame() {
    let asset = Uuid::new_v4();
    let mut timeline = one_clip(Clip::new(asset, 0.0, 4.0, 0.0));
    let mut map = HashMap::new();
    map.insert(asset, vec![seg(0.0, 4.0, "non-destructive editing")]);
    let opts = CaptionOptions::styled(CaptionStyle::WordPunch);
    let full = CaptionStyle::WordPunch.layout().size;

    // Unframed, so 16:9 — wide enough that nothing is shrunk, which is what
    // keeps every project that never picked a frame captioned as it was.
    let wide = timeline.captions(&map, opts);
    assert!(wide.iter().all(|o| o.size == full), "{wide:?}");

    // 9:16 is barely half as wide as it is tall, and `drawtext` neither
    // wraps nor scales: the long word would be drawn off both edges.
    timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));
    let tall = timeline.captions(&map, opts);
    let long = tall.iter().find(|o| o.text == "non-destructive").expect("the long word");
    let short = tall.iter().find(|o| o.text == "editing").expect("the short word");
    assert!(long.size < full, "the long word shrinks: {}", long.size);
    assert_eq!(short.size, full, "a word that already fits is left alone");
    let aspect = 1080.0 / 1920.0;
    assert!(
        long.text.chars().count() as f64 * CHAR_ADVANCE * long.size <= CAPTION_WIDTH * aspect + 1e-9,
        "still overflows: {}",
        long.size
    );
}

#[test]
fn a_muted_track_is_not_captioned() {
    let asset = Uuid::new_v4();
    let mut timeline = one_clip(Clip::new(asset, 0.0, 4.0, 0.0));
    assert!(!captioned(&timeline, asset, vec![seg(0.0, 4.0, "heard")]).is_empty());
    timeline.tracks[0].muted = true;
    assert!(captioned(&timeline, asset, vec![seg(0.0, 4.0, "heard")]).is_empty());
}

#[test]
fn the_same_words_on_two_tracks_are_captioned_once() {
    let asset = Uuid::new_v4();
    // What `extract_audio` leaves behind: picture and detached audio, both
    // referencing the same asset over the same source window.
    let mut video = Track::new(StreamKind::Video, "V1");
    video.clips = vec![Clip::new(asset, 0.0, 3.0, 0.0)];
    let mut audio = Track::new(StreamKind::Audio, "A1");
    audio.clips = vec![Clip::new(asset, 0.0, 3.0, 0.0)];
    let timeline = Timeline {
        tracks: vec![video, audio],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let lines = captioned(&timeline, asset, vec![seg(0.0, 3.0, "only once")]);
    assert_eq!(lines.len(), 1, "{lines:?}");
}

// ---- imported captions ----------------------------------------------------

/// A cut with one `len`-second clip of `asset` from its start.
fn cut_of(asset: Uuid, len: f64) -> Timeline {
    one_clip(Clip::new(asset, 0.0, len, 0.0))
}

fn rounded(overlays: &[TextOverlay]) -> Vec<(String, f64, f64)> {
    overlays
        .iter()
        .map(|o| {
            (
                o.text.clone(),
                (o.start * 100.0).round() / 100.0,
                (o.end * 100.0).round() / 100.0,
            )
        })
        .collect()
}

/// Every cue offered is in exactly one bucket — the invariant the summary's
/// numbers are built on.
fn accounted(p: &CaptionPlacement, offered: usize) {
    assert_eq!(
        p.placed + p.dropped_outside + p.dropped_short + p.dropped_overlap,
        offered,
        "{p:?}"
    );
}

#[test]
fn cues_in_timeline_time_land_where_the_file_says() {
    let timeline = cut_of(Uuid::new_v4(), 10.0);
    let cues = [seg(1.0, 3.0, "Hello there"), seg(4.0, 6.0, "General Kenobi")];
    let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
    assert_eq!(
        rounded(&p.overlays),
        [("Hello there".into(), 1.0, 3.0), ("General Kenobi".into(), 4.0, 6.0)]
    );
    accounted(&p, 2);
    assert_eq!((p.placed, p.dropped_outside, p.dropped_overlap), (2, 0, 0));
    // Written as generated captions in the style's own look, so Recaption,
    // Clear and the delivery re-fit all treat them as captions.
    let layout = CaptionStyle::Lines.layout();
    assert!(p
        .overlays
        .iter()
        .all(|o| o.generated && o.pos_y == layout.pos_y && o.bg.is_some()));
}

#[test]
fn an_imported_cue_is_chunked_exactly_like_a_transcript_segment() {
    // The refactor's whole point: one chunking / timing / fitting path. A
    // cue in timeline time over an identity clip must come out the same as
    // the same words as a transcript segment of that clip — in both styles,
    // and on a tall frame where the fit binds.
    let asset = Uuid::new_v4();
    let mut timeline = cut_of(asset, 12.0);
    timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));
    let line = seg(
        0.5,
        11.0,
        "Today we are talking about non-destructive editing in Kerf, and a few extraordinarily long words",
    );
    let mut map = HashMap::new();
    map.insert(asset, vec![line.clone()]);
    for style in [CaptionStyle::Lines, CaptionStyle::WordPunch] {
        let opts = CaptionOptions::styled(style);
        let from_transcript = timeline.captions(&map, opts);
        let from_cues = timeline
            .place_cues(std::slice::from_ref(&line), CaptionTimeBase::Timeline, opts)
            .overlays;
        assert!(from_cues.len() > 1, "{style:?} should split a long cue");
        assert_eq!(from_cues.len(), from_transcript.len(), "{style:?}");
        for (a, b) in from_cues.iter().zip(&from_transcript) {
            assert_eq!(
                (&a.text, a.start, a.end, a.size, a.pos_y, a.bold, &a.bg, a.generated),
                (&b.text, b.start, b.end, b.size, b.pos_y, b.bold, &b.bg, b.generated),
                "{style:?}"
            );
        }
    }
    // And the same asset-based placement is `captions` itself.
    let via_source = timeline.place_cues(&[line], CaptionTimeBase::Source(asset), CaptionOptions::default());
    assert_eq!(
        rounded(&via_source.overlays),
        rounded(&timeline.captions(&map, CaptionOptions::default()))
    );
}

#[test]
fn imported_captions_shrink_to_fit_a_vertical_frame_and_take_the_style() {
    let mut timeline = cut_of(Uuid::new_v4(), 6.0);
    timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));
    let cues = [seg(0.0, 6.0, "non-destructive editing")];
    let p = timeline.place_cues(
        &cues,
        CaptionTimeBase::Timeline,
        CaptionOptions::styled(CaptionStyle::WordPunch),
    );
    let layout = CaptionStyle::WordPunch.layout();
    let long = p
        .overlays
        .iter()
        .find(|o| o.text == "non-destructive")
        .expect("the long word");
    let short = p.overlays.iter().find(|o| o.text == "editing").expect("the short word");
    assert!(long.size < layout.size, "the long word shrinks to fit 9:16: {}", long.size);
    assert_eq!(short.size, layout.size);
    assert!(p.overlays.iter().all(|o| o.bold && o.pos_y == layout.pos_y));
    // An override still moves one number.
    let opts = CaptionOptions {
        pos_y: Some(0.5),
        ..CaptionOptions::styled(CaptionStyle::WordPunch)
    };
    let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, opts);
    assert!(p.overlays.iter().all(|o| o.pos_y == 0.5 && o.bold));
}

#[test]
fn timeline_cues_past_the_end_of_the_cut_are_dropped_and_the_straddler_clipped() {
    let timeline = cut_of(Uuid::new_v4(), 10.0);
    let cues = [
        seg(2.0, 4.0, "inside"),
        seg(9.0, 12.0, "straddles the end"),
        seg(10.0, 12.0, "starts at the end"),
        seg(20.0, 25.0, "long gone"),
    ];
    let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
    accounted(&p, 4);
    assert_eq!((p.placed, p.dropped_outside), (2, 2));
    let last = p.overlays.last().unwrap();
    assert!(last.text.contains("end") && (last.end - 10.0).abs() < 1e-9, "{last:?}");
    assert!(p.overlays.iter().all(|o| o.end <= 10.0 + 1e-9));
    // A sliver under the readable floor at the very end is not kept either.
    let sliver = timeline.place_cues(
        &[seg(9.95, 12.0, "barely")],
        CaptionTimeBase::Timeline,
        CaptionOptions::default(),
    );
    assert!(sliver.overlays.is_empty());
    // It met the cut, for too short a moment to read: not "outside" it.
    assert_eq!((sliver.dropped_outside, sliver.dropped_short), (0, 1));
    accounted(&sliver, 1);
}

#[test]
fn timeline_cues_on_an_empty_timeline_have_no_end_to_run_past() {
    let timeline = Timeline::new();
    let cues = [seg(0.0, 2.0, "first"), seg(600.0, 602.0, "much later")];
    let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
    assert_eq!((p.placed, p.dropped_outside), (2, 0));
}

#[test]
fn a_muted_track_silences_source_cues_but_not_timeline_cues() {
    let asset = Uuid::new_v4();
    let mut timeline = cut_of(asset, 6.0);
    let mut music = Track::new(StreamKind::Audio, "A1");
    music.clips = vec![Clip::new(Uuid::new_v4(), 0.0, 20.0, 0.0)];
    timeline.tracks.push(music);
    let cues = [seg(1.0, 3.0, "heard")];
    // Source time is a claim about footage: a muted clip shows nothing.
    timeline.tracks[0].muted = true;
    let src = timeline.place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default());
    assert!(src.overlays.is_empty());
    assert_eq!(src.dropped_outside, 1);
    // Timeline time is a claim about the finished cut. With the picture muted
    // the cut renders no further than its remaining content — here, the
    // music — and the cue is still inside it.
    let tl = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
    assert_eq!(tl.placed, 1);
    // A disabled clip is as absent as a muted track.
    timeline.tracks[0].muted = false;
    timeline.tracks[0].clips[0].enabled = false;
    assert!(timeline
        .place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default())
        .overlays
        .is_empty());
}

#[test]
fn source_cues_follow_trim_move_speed_and_reverse() {
    let asset = Uuid::new_v4();
    let cues = [seg(12.0, 14.0, "alpha beta")];
    let place = |clip: Clip| {
        one_clip(clip)
            .place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default())
            .overlays
    };
    // Trimmed to 10..20 and moved to 4s: the cue's 12..14 is 6..8.
    let trimmed = Clip::new(asset, 10.0, 20.0, 4.0);
    assert_eq!(rounded(&place(trimmed.clone())), [("alpha beta".into(), 6.0, 8.0)]);
    // Double speed halves the distance from the in-point.
    let fast = Clip {
        speed: 2.0,
        ..trimmed.clone()
    };
    assert_eq!(rounded(&place(fast)), [("alpha beta".into(), 5.0, 6.0)]);
    // Reversed: the source's tail is heard first and the range stays ordered.
    let reversed = Clip {
        speed: -1.0,
        ..trimmed.clone()
    };
    assert_eq!(rounded(&place(reversed)), [("alpha beta".into(), 10.0, 12.0)]);
    // A cue entirely before the in-point shows nothing.
    let early = one_clip(trimmed).place_cues(
        &[seg(0.0, 5.0, "trimmed away")],
        CaptionTimeBase::Source(asset),
        CaptionOptions::default(),
    );
    assert!(early.overlays.is_empty());
    assert_eq!(early.dropped_outside, 1);
}

#[test]
fn a_source_cue_cut_in_half_only_captions_what_survived() {
    let asset = Uuid::new_v4();
    let timeline = one_clip(Clip::new(asset, 0.0, 2.0, 0.0));
    let cues = [seg(0.0, 4.0, "alpha bravo charlie delta echo foxtrot")];
    let p = timeline.place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default());
    assert!(p.overlays.iter().all(|o| o.end <= 2.0 + 1e-9), "{:?}", p.overlays);
    assert!(!p.overlays.iter().any(|o| o.text.contains("foxtrot")), "{:?}", p.overlays);
    assert_eq!(rounded(&p.overlays).len(), 1);
    assert_eq!(p.placed, 1);

    // Several lines survive when more of the cue does: placed counts cues,
    // not the lines a long one becomes.
    let longer = one_clip(Clip::new(asset, 0.0, 6.0, 0.0));
    let cues = [seg(
        0.0,
        8.0,
        "one two three four five six seven eight nine ten eleven twelve",
    )];
    let p = longer.place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default());
    assert!(p.overlays.len() > 1, "{:?}", p.overlays);
    assert_eq!(p.placed, 1, "one cue, however many lines it became");
    accounted(&p, 1);
}

#[test]
fn source_cues_reach_every_clip_of_the_asset_and_only_that_asset() {
    let asset = Uuid::new_v4();
    let other = Uuid::new_v4();
    let mut track = Track::new(StreamKind::Video, "V1");
    // The same footage twice (a callback shot) with someone else's in between.
    track.clips = vec![
        Clip::new(asset, 0.0, 4.0, 0.0),
        Clip::new(other, 0.0, 4.0, 4.0),
        Clip::new(asset, 0.0, 4.0, 8.0),
    ];
    let timeline = Timeline {
        tracks: vec![track],
        ..Timeline::new()
    };
    let p = timeline.place_cues(
        &[seg(1.0, 3.0, "said twice")],
        CaptionTimeBase::Source(asset),
        CaptionOptions::default(),
    );
    assert_eq!(
        rounded(&p.overlays),
        [("said twice".into(), 1.0, 3.0), ("said twice".into(), 9.0, 11.0)]
    );
    assert_eq!(p.placed, 1, "still one cue");
    accounted(&p, 1);
}

#[test]
fn imported_cues_never_share_the_screen() {
    let timeline = cut_of(Uuid::new_v4(), 20.0);
    let cues = [
        seg(0.0, 4.0, "alpha beta"),
        // Starts under the first: waits for it to end.
        seg(2.0, 6.0, "gamma delta"),
        // Wholly under what is already on screen: nothing readable is left.
        seg(3.0, 3.4, "buried"),
        // The same words at the same moment as an earlier cue.
        seg(8.0, 10.0, "twice over"),
        seg(8.0, 10.0, "twice over"),
    ];
    let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
    accounted(&p, 5);
    assert_eq!((p.placed, p.dropped_outside, p.dropped_overlap), (3, 0, 2), "{p:?}");
    assert_eq!(
        rounded(&p.overlays),
        [
            ("alpha beta".into(), 0.0, 4.0),
            ("gamma delta".into(), 4.0, 6.0),
            ("twice over".into(), 8.0, 10.0)
        ]
    );
    for pair in p.overlays.windows(2) {
        assert!(pair[1].start >= pair[0].end - 1e-9);
    }
}

#[test]
fn unusable_cues_count_as_not_placed_rather_than_vanishing() {
    let asset = Uuid::new_v4();
    let timeline = cut_of(asset, 10.0);
    let cues = [
        seg(1.0, 2.0, "fine"),
        seg(3.0, 3.0, "zero length"),
        seg(5.0, 4.0, "backwards"),
        seg(6.0, 7.0, "   "),
        seg(f64::NAN, 8.0, "not a time"),
    ];
    for base in [CaptionTimeBase::Timeline, CaptionTimeBase::Source(asset)] {
        let p = timeline.place_cues(&cues, base, CaptionOptions::default());
        accounted(&p, 5);
        assert_eq!((p.placed, p.dropped_outside), (1, 4), "{base:?}");
    }
}

#[test]
fn a_cue_that_meets_the_cut_too_briefly_is_short_not_outside() {
    let asset = Uuid::new_v4();
    let timeline = one_clip(Clip::new(asset, 10.0, 20.0, 0.0));
    let cues = [
        seg(11.0, 13.0, "readable"),
        // Wholly on the footage, but a blink long.
        seg(14.0, 14.05, "a blink"),
        // Starts before the kept footage and leaves a sliver of it.
        seg(9.0, 10.05, "just inside"),
        // Not on the kept footage at all.
        seg(30.0, 32.0, "elsewhere"),
    ];
    let p = timeline.place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default());
    accounted(&p, 4);
    assert_eq!(
        (p.placed, p.dropped_short, p.dropped_outside, p.dropped_overlap),
        (1, 2, 1, 0),
        "{p:?}"
    );
    // On the timeline clock the same split: before 0 and after the end are
    // outside; a blink or a sliver at an edge is short.
    let cut = cut_of(asset, 10.0);
    let cues = [
        seg(1.0, 3.0, "readable"),
        seg(4.0, 4.05, "a blink"),
        seg(-5.0, 0.05, "starts early"),
        seg(-5.0, -1.0, "before the start"),
        seg(9.96, 14.0, "at the end"),
        seg(10.0, 12.0, "after the end"),
    ];
    let p = cut.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
    accounted(&p, 6);
    assert_eq!((p.placed, p.dropped_short, p.dropped_outside), (1, 3, 2), "{p:?}");
}

#[test]
fn simultaneous_imported_cues_keep_the_order_of_the_file() {
    let asset = Uuid::new_v4();
    let timeline = cut_of(asset, 10.0);
    // Two cues at one timecode: the one written first holds the slot.
    let cues = [seg(1.0, 4.0, "zebra"), seg(1.0, 4.0, "apple")];
    let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
    assert_eq!(rounded(&p.overlays), [("zebra".into(), 1.0, 4.0)]);
    accounted(&p, 2);
    assert_eq!(p.dropped_overlap, 1);
    // …through a clip as well…
    let p = timeline.place_cues(&cues, CaptionTimeBase::Source(asset), CaptionOptions::default());
    assert_eq!(rounded(&p.overlays), [("zebra".into(), 1.0, 4.0)]);
    // …and an identical pair is still collapsed even with another between.
    let cues = [seg(1.0, 3.0, "same"), seg(1.0, 3.0, "other"), seg(1.0, 3.0, "same")];
    let p = timeline.place_cues(&cues, CaptionTimeBase::Timeline, CaptionOptions::default());
    accounted(&p, 3);
    assert_eq!(p.placed, 1, "{p:?}");
    // A transcript has no order of its own and still sorts alphabetically —
    // the behavior every existing cut was captioned with.
    let mut map = HashMap::new();
    map.insert(asset, cues[..1].to_vec());
    map.insert(asset, vec![seg(1.0, 4.0, "zebra"), seg(1.0, 4.0, "apple")]);
    let from_transcript = timeline.captions(&map, CaptionOptions::default());
    assert_eq!(rounded(&from_transcript), [("apple".into(), 1.0, 4.0)]);
}

/// The line timer exactly as it was before it was made cheaper: everything
/// recomputed — and every chunk cloned — on every pass.
fn reference_time_chunks(chunks: Vec<String>, span: TimeRange, min: f64) -> Vec<(TimeRange, String)> {
    let mut chunks = chunks;
    let duration = (span.end - span.start).max(0.0);
    loop {
        let weights: Vec<f64> = chunks.iter().map(|c| c.chars().count().max(1) as f64).collect();
        let total: f64 = weights.iter().sum();
        let mut timed: Vec<(TimeRange, String)> = Vec::with_capacity(chunks.len());
        let mut at = span.start;
        for (i, text) in chunks.iter().enumerate() {
            let share = if total > 0.0 { weights[i] / total } else { 1.0 };
            let end = if i + 1 == chunks.len() {
                span.end
            } else {
                at + duration * share
            };
            timed.push((TimeRange { start: at, end }, text.clone()));
            at = end;
        }
        if chunks.len() < 2 {
            return timed;
        }
        let short = timed.iter().position(|(r, _)| r.end - r.start < min);
        let Some(i) = short else { return timed };
        let merge_back = i > 0 && (i + 1 == chunks.len() || chunks[i - 1].chars().count() <= chunks[i + 1].chars().count());
        let into = if merge_back { i - 1 } else { i };
        let moved = chunks.remove(into + 1);
        chunks[into] = format!("{}{}{}", chunks[into], ' ', moved);
    }
}

#[test]
fn the_cheaper_line_timer_gives_exactly_the_old_answers() {
    // A deterministic pseudo-random sweep: chunk counts from none to dozens,
    // chunk widths from one letter to a long word, spans from nothing to
    // minutes and starting anywhere, every floor in use. Bit-for-bit equal,
    // since it decides where every transcript caption starts.
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    for case in 0..6_000 {
        let count = next(60) as usize;
        let chunks: Vec<String> = (0..count)
            .map(|_| {
                let width = if next(5) == 0 { 30 } else { 8 };
                "x".repeat(1 + next(width) as usize)
            })
            .collect();
        let start = (next(100_000) as f64) / 100.0 - 50.0;
        let len = match next(4) {
            0 => 0.0,
            1 => (next(100) as f64) / 1000.0,
            2 => (next(3000) as f64) / 100.0,
            _ => (next(100_000) as f64) / 10.0,
        };
        let span = TimeRange { start, end: start + len };
        let min = [MIN_CAPTION, MIN_WORD_CAPTION, 0.0, 5.0][next(4) as usize];
        let old = reference_time_chunks(chunks.clone(), span, min);
        let new = time_chunks(chunks, span, min);
        assert_eq!(old.len(), new.len(), "case {case}");
        for (a, b) in old.iter().zip(&new) {
            assert_eq!(
                (a.0.start.to_bits(), a.0.end.to_bits(), &a.1),
                (b.0.start.to_bits(), b.0.end.to_bits(), &b.1),
                "case {case}: {span:?} min {min}"
            );
        }
    }
}

#[test]
fn a_thousand_word_cue_in_word_punch_places_quickly() {
    // The worst import the caps allow: `MAX_CAPTION_WORDS` one-letter words in
    // cues as long as `MAX_CUE_CHARS` lets them be, each over a few seconds,
    // so every word is a flicker that has to be merged. Merging within a cue
    // is still quadratic in its word count (4x the words costs ~16x), which is
    // why a cue and an import are capped: the worst case is bounded, not the
    // algorithm. It runs in well under half a second unoptimized; the limit is
    // ~20x that because the suite runs in parallel on a busy machine — it has
    // to catch a lost cap or a much slower merge, never the load. (The old
    // timer rebuilt and cloned every chunk per merge: about 3 s for this.)
    use crate::captions_import::{MAX_CAPTION_WORDS, MAX_CUE_CHARS};
    let words_per_cue = MAX_CUE_CHARS.div_ceil(2);
    let cue_count = MAX_CAPTION_WORDS / words_per_cue;
    let asset = Uuid::new_v4();
    let timeline = cut_of(asset, cue_count as f64 * 10.0);
    let text = vec!["a"; words_per_cue].join(" ");
    assert!(text.chars().count() <= MAX_CUE_CHARS);
    let cues: Vec<TranscriptSegment> = (0..cue_count)
        .map(|i| seg(i as f64 * 10.0, i as f64 * 10.0 + 6.0, &text))
        .collect();
    let started = std::time::Instant::now();
    let p = timeline.place_cues(
        &cues,
        CaptionTimeBase::Timeline,
        CaptionOptions::styled(CaptionStyle::WordPunch),
    );
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(8), "placing took {took:?}");
    accounted(&p, cue_count);
    assert_eq!(p.placed, cue_count);
    // Nothing was lost to the merging: every word is still there.
    let words: usize = p.overlays.iter().map(|o| o.text.split_whitespace().count()).sum();
    assert_eq!(words, cue_count * words_per_cue);
}

#[test]
fn placement_leaves_the_timeline_it_reads_alone() {
    let asset = Uuid::new_v4();
    let timeline = cut_of(asset, 10.0);
    let before = serde_json::to_string(&timeline).unwrap();
    timeline.place_cues(
        &[seg(0.0, 3.0, "x y z")],
        CaptionTimeBase::Timeline,
        CaptionOptions::default(),
    );
    assert_eq!(serde_json::to_string(&timeline).unwrap(), before);
}

#[test]
fn a_time_base_resolves_from_loose_arguments_and_refuses_contradictions() {
    let asset = Uuid::new_v4();
    let resolve = CaptionTimeBase::resolve;
    assert_eq!(resolve(None, None).unwrap(), CaptionTimeBase::Timeline);
    assert_eq!(resolve(Some("timeline"), None).unwrap(), CaptionTimeBase::Timeline);
    assert_eq!(resolve(Some(" Timeline "), None).unwrap(), CaptionTimeBase::Timeline);
    assert_eq!(resolve(Some(""), None).unwrap(), CaptionTimeBase::Timeline);
    assert_eq!(
        resolve(None, Some(asset)).unwrap(),
        CaptionTimeBase::Source(asset),
        "an asset alone implies source"
    );
    assert_eq!(resolve(Some("SOURCE"), Some(asset)).unwrap(), CaptionTimeBase::Source(asset));
    assert!(resolve(Some("source"), None).unwrap_err().to_string().contains("asset_id"));
    assert!(resolve(Some("timeline"), Some(asset))
        .unwrap_err()
        .to_string()
        .contains("only applies"));
    let err = resolve(Some("sideways"), None).unwrap_err().to_string();
    assert!(err.contains("\"timeline\" or \"source\""), "{err}");
}

// ---- ripple ---------------------------------------------------------------

/// A clip of `dur` seconds placed at `start`, cut from `[0, dur)` of nothing
/// in particular.
fn rclip(start: f64, dur: f64) -> Clip {
    Clip::new(Uuid::nil(), 0.0, dur, start)
}

/// One video track `V1` holding `clips`.
fn one_lane(clips: Vec<Clip>) -> Timeline {
    Timeline {
        tracks: vec![track(StreamKind::Video, "V1", clips)],
        ..Timeline::new()
    }
}

/// Each clip of track `ti` as `(start, end)`, in lane order.
fn spans_of(timeline: &Timeline, ti: usize) -> Vec<(f64, f64)> {
    timeline.tracks[ti]
        .clips
        .iter()
        .map(|c| (c.timeline_start, c.timeline_end()))
        .collect()
}

fn same_cut(a: &Timeline, b: &Timeline) -> bool {
    serde_json::to_string(a).unwrap() == serde_json::to_string(b).unwrap()
}

/// Run `edit` on a copy of `before` and ripple the result.
fn rippled(before: &Timeline, edit: impl FnOnce(&mut Timeline)) -> (Timeline, Timeline) {
    let mut after = before.clone();
    edit(&mut after);
    let out = after.ripple_from(before);
    (after, out)
}

/// `a [0,4)  b [5,8)  c [10,12)` — a gap of 1 before `b` and 2 before `c`.
fn gapped() -> (Timeline, [Uuid; 3]) {
    let (a, b, c) = (rclip(0.0, 4.0), rclip(5.0, 3.0), rclip(10.0, 2.0));
    let ids = [a.id, b.id, c.id];
    (one_lane(vec![a, b, c]), ids)
}

fn clip_mut(t: &mut Timeline, id: Uuid) -> &mut Clip {
    let (ti, ci) = t.locate(id).unwrap();
    &mut t.tracks[ti].clips[ci]
}

#[test]
fn a_right_trim_that_shortens_pulls_later_clips_left_and_keeps_every_gap() {
    let (before, [a, ..]) = gapped();
    let (_, out) = rippled(&before, |t| clip_mut(t, a).source_out = 3.0);
    assert_eq!(spans_of(&out, 0), vec![(0.0, 3.0), (4.0, 7.0), (9.0, 11.0)]);
}

#[test]
fn a_right_trim_that_lengthens_pushes_later_clips_right() {
    let (before, [a, ..]) = gapped();
    let (after, out) = rippled(&before, |t| clip_mut(t, a).source_out = 6.0);
    // Un-rippled, the lengthened clip would sit on top of `b`.
    assert_eq!(spans_of(&after, 0)[1], (5.0, 8.0));
    assert_eq!(spans_of(&out, 0), vec![(0.0, 6.0), (7.0, 10.0), (12.0, 14.0)]);
}

#[test]
fn a_left_trim_keeps_the_clips_start_whether_or_not_the_right_edge_was_held() {
    let (before, [_, b, _]) = gapped();
    // The GUI's left-edge trim: later in-point, and a later start so the right
    // edge (8.0) stays put. Ripple keeps the *start* and pulls the rest in.
    let (after, held) = rippled(&before, |t| {
        let c = clip_mut(t, b);
        c.source_in = 1.0;
        c.timeline_start = 6.0;
    });
    assert_eq!(spans_of(&after, 0)[1], (6.0, 8.0));
    assert_eq!(spans_of(&held, 0), vec![(0.0, 4.0), (5.0, 7.0), (9.0, 11.0)]);

    // The same trim without moving the start gives the same cut.
    let (_, unmoved) = rippled(&before, |t| clip_mut(t, b).source_in = 1.0);
    assert!(same_cut(&held, &unmoved));
}

#[test]
fn a_left_trim_that_lengthens_also_keeps_the_start() {
    // `b` is cut from [2, 5) of its source, so it has a second of handle on the left.
    let (a, b, c) = (rclip(0.0, 4.0), Clip::new(Uuid::nil(), 2.0, 5.0, 5.0), rclip(10.0, 2.0));
    let b_id = b.id;
    let before = one_lane(vec![a, b, c]);
    let (after, out) = rippled(&before, |t| {
        let c = clip_mut(t, b_id);
        c.source_in = 1.0;
        c.timeline_start = 4.0; // right edge held at 8.0
    });
    assert_eq!(spans_of(&after, 0)[1], (4.0, 8.0), "un-rippled it would sit on `a`");
    assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (5.0, 9.0), (11.0, 13.0)]);
}

#[test]
fn a_speed_change_is_a_length_change() {
    let (before, [_, b, _]) = gapped();
    let (_, out) = rippled(&before, |t| clip_mut(t, b).speed = 2.0); // 3.0s -> 1.5s
    assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (5.0, 6.5), (8.5, 10.5)]);
}

#[test]
fn removing_a_clip_closes_its_span_and_keeps_the_gaps_either_side() {
    let (before, [_, b, _]) = gapped();
    let (_, out) = rippled(&before, |t| t.tracks[0].clips.retain(|c| c.id != b));
    // 1s of gap before `b` and 2s after it: 3s between `a` and `c`, as there was.
    assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (7.0, 9.0)]);
}

#[test]
fn removing_several_clips_closes_each_span_once() {
    let clips = vec![rclip(0.0, 2.0), rclip(2.0, 2.0), rclip(6.0, 2.0), rclip(10.0, 2.0)];
    let (a, c) = (clips[0].id, clips[2].id);
    let before = one_lane(clips);
    let (_, out) = rippled(&before, |t| t.tracks[0].clips.retain(|k| k.id != a && k.id != c));
    assert_eq!(spans_of(&out, 0), vec![(0.0, 2.0), (6.0, 8.0)]);
}

#[test]
fn a_clip_added_onto_footage_pushes_it_and_everything_after_right() {
    let clips = vec![rclip(0.0, 4.0), rclip(4.0, 4.0), rclip(8.0, 4.0)];
    let before = one_lane(clips);
    // Dropped at the cut between `a` and `b`: a 2s insert.
    let (after, out) = rippled(&before, |t| t.tracks[0].clips.push(rclip(4.0, 2.0)));
    assert_eq!(after.tracks[0].clips[1].timeline_start, 4.0, "un-rippled it overlaps `b`");
    assert_eq!(
        spans_of(&out, 0),
        vec![(0.0, 4.0), (4.0, 6.0), (6.0, 10.0), (10.0, 14.0)],
        "kept in lane order, nothing overlapping"
    );
}

#[test]
fn a_clip_added_over_the_head_of_a_clip_in_a_gap_pushes_by_its_whole_length() {
    let before = one_lane(vec![rclip(0.0, 2.0), rclip(10.0, 2.0), rclip(14.0, 2.0)]);
    // 5s at 8.0 runs into the head of the clip at 10.0.
    let (_, out) = rippled(&before, |t| t.tracks[0].clips.push(rclip(8.0, 5.0)));
    assert_eq!(spans_of(&out, 0), vec![(0.0, 2.0), (8.0, 13.0), (15.0, 17.0), (19.0, 21.0)]);
}

#[test]
fn a_clip_that_fits_moves_nothing() {
    let before = one_lane(vec![rclip(0.0, 2.0), rclip(10.0, 2.0)]);
    // Into the gap, and onto the end of the track.
    let (after, out) = rippled(&before, |t| {
        t.tracks[0].clips.push(rclip(4.0, 2.0));
        t.tracks[0].clips.push(rclip(12.0, 3.0));
    });
    assert!(same_cut(&after, &out));
}

#[test]
fn a_clip_added_inside_another_clip_is_left_as_the_edit_made_it() {
    // Resolving it would take a split, which ripple never does.
    let before = one_lane(vec![rclip(0.0, 10.0), rclip(10.0, 4.0)]);
    let (after, out) = rippled(&before, |t| t.tracks[0].clips.push(rclip(4.0, 2.0)));
    assert!(same_cut(&after, &out), "no ripple for the track — not half of one");
}

#[test]
fn adjacent_adds_push_once_by_their_combined_length() {
    let before = one_lane(vec![rclip(0.0, 4.0), rclip(4.0, 4.0)]);
    let (_, out) = rippled(&before, |t| {
        t.tracks[0].clips.push(rclip(4.0, 2.0));
        t.tracks[0].clips.push(rclip(6.0, 3.0));
    });
    assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (4.0, 6.0), (6.0, 9.0), (9.0, 13.0)]);
}

#[test]
fn replacing_a_clip_in_place_moves_nothing() {
    let (before, [_, b, _]) = gapped();
    let (after, out) = rippled(&before, |t| {
        t.tracks[0].clips.retain(|c| c.id != b);
        t.tracks[0].clips.insert(1, rclip(5.0, 3.0));
    });
    assert!(same_cut(&after, &out), "-3s for the removal and +3s for the add cancel");
}

#[test]
fn a_split_shifts_nothing() {
    let (before, [a, ..]) = gapped();
    let (after, out) = rippled(&before, |t| {
        // What `Project::split_at` does at 1.5.
        let left = clip_mut(t, a);
        let mut right = left.clone();
        right.id = Uuid::new_v4();
        right.timeline_start = 1.5;
        right.source_in = 1.5;
        left.source_out = 1.5;
        t.tracks[0].clips.insert(1, right);
    });
    assert_eq!(after.tracks[0].clips.len(), 4);
    assert!(same_cut(&after, &out));
}

#[test]
fn a_move_does_not_ripple_within_a_track_or_across_tracks() {
    let (mut before, [a, b, _]) = gapped();
    before
        .tracks
        .insert(1, track(StreamKind::Video, "V2", vec![rclip(20.0, 2.0)]));

    // Within the track: `a` slides into the gap.
    let (after, out) = rippled(&before, |t| clip_mut(t, a).timeline_start = 0.5);
    assert!(same_cut(&after, &out));

    // Across tracks: `b` goes up to V2. V1 does not close behind it.
    let (after, out) = rippled(&before, |t| {
        let (ti, ci) = t.locate(b).unwrap();
        let moved = t.tracks[ti].clips.remove(ci);
        t.tracks[1].clips.insert(0, moved);
    });
    assert!(same_cut(&after, &out));
}

#[test]
fn tracks_ripple_independently() {
    let (a, b, c) = (rclip(0.0, 4.0), rclip(5.0, 3.0), rclip(10.0, 2.0));
    let a_id = a.id;
    let audio = vec![rclip(0.0, 4.0), rclip(5.0, 3.0), rclip(10.0, 2.0)];
    let before = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![a, b, c]),
            track(StreamKind::Audio, "A1", audio),
        ],
        ..Timeline::new()
    };
    let (_, out) = rippled(&before, |t| clip_mut(t, a_id).source_out = 3.0);
    assert_eq!(spans_of(&out, 0), vec![(0.0, 3.0), (4.0, 7.0), (9.0, 11.0)]);
    assert_eq!(spans_of(&out, 1), spans_of(&before, 1), "no sync lock: the audio stays put");
}

#[test]
fn a_locked_track_never_moves() {
    let (mut before, [a, ..]) = gapped();
    before.tracks[0].locked = true;
    let (after, out) = rippled(&before, |t| clip_mut(t, a).source_out = 3.0);
    assert!(same_cut(&after, &out));
}

#[test]
fn an_edit_that_already_rippled_is_not_rippled_twice() {
    let (before, [_, b, _]) = gapped();
    // `Project::ripple_delete` of `b`: removed, and `c` closed up by 3.
    let (after, out) = rippled(&before, |t| {
        t.tracks[0].clips.retain(|c| c.id != b);
        t.tracks[0].clips[1].timeline_start -= 3.0;
    });
    assert_eq!(spans_of(&after, 0), vec![(0.0, 4.0), (7.0, 9.0)]);
    assert!(same_cut(&after, &out));

    // `Project::cut_clip_range` of the middle second of `a`: the head keeps
    // the id, the tail is a new clip, and everything later is moved left 1s.
    let (before, [a, ..]) = gapped();
    let (after, out) = rippled(&before, |t| {
        let head = clip_mut(t, a);
        let mut tail = head.clone();
        head.source_out = 1.0;
        tail.id = Uuid::new_v4();
        tail.source_in = 2.0;
        tail.timeline_start = 1.0;
        t.tracks[0].clips.insert(1, tail);
        for c in t.tracks[0].clips.iter_mut().skip(2) {
            c.timeline_start -= 1.0;
        }
    });
    assert_eq!(spans_of(&after, 0), vec![(0.0, 1.0), (1.0, 3.0), (4.0, 7.0), (9.0, 11.0)]);
    assert!(same_cut(&after, &out));
}

#[test]
fn two_trims_in_one_edit_accumulate_down_the_track() {
    let (before, [a, b, _]) = gapped();
    let (_, out) = rippled(&before, |t| {
        clip_mut(t, a).source_out = 3.0; // -1s
        clip_mut(t, b).source_out = 2.0; // -1s, and `b` itself follows `a`
    });
    assert_eq!(spans_of(&out, 0), vec![(0.0, 3.0), (4.0, 6.0), (8.0, 10.0)]);
}

#[test]
fn a_ripple_that_would_overlap_is_not_applied() {
    // V1: a [0,4)  r [4,6)  f [6,8).  The edit deletes `r` and drops a clip
    // from V2 into its place. Closing `f` up behind `r` would land it on top.
    let (a, r, f) = (rclip(0.0, 4.0), rclip(4.0, 2.0), rclip(6.0, 2.0));
    let (r_id, m) = (r.id, rclip(0.0, 2.0));
    let m_id = m.id;
    let before = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![a, r, f]),
            track(StreamKind::Video, "V2", vec![m]),
        ],
        ..Timeline::new()
    };
    let (after, out) = rippled(&before, |t| {
        t.tracks[0].clips.retain(|c| c.id != r_id);
        let mut moved = t.tracks[1].clips.remove(0);
        assert_eq!(moved.id, m_id);
        moved.timeline_start = 4.0;
        t.tracks[0].clips.insert(1, moved);
    });
    assert!(same_cut(&after, &out), "left exactly as the edit made it");
    assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (4.0, 6.0), (6.0, 8.0)]);
}

#[test]
fn a_ripple_that_would_start_a_clip_before_zero_is_not_applied() {
    // An old project with overlapping clips: deleting both `r1` and `r2` would
    // close `f` up by 8s, which is more than there is room for.
    let (r1, r2, f) = (rclip(0.0, 4.0), rclip(1.0, 4.0), rclip(5.0, 2.0));
    let (r1_id, r2_id) = (r1.id, r2.id);
    let before = one_lane(vec![r1, r2, f]);
    let (after, out) = rippled(&before, |t| t.tracks[0].clips.retain(|c| c.id != r1_id && c.id != r2_id));
    assert!(same_cut(&after, &out));
    assert_eq!(spans_of(&out, 0), vec![(5.0, 7.0)]);
}

#[test]
fn an_old_overlap_elsewhere_in_the_track_blocks_nothing() {
    // `a` and `a2` already overlap and nothing touches them.
    let (a, a2, b, c) = (rclip(0.0, 4.0), rclip(2.0, 4.0), rclip(10.0, 2.0), rclip(14.0, 2.0));
    let b_id = b.id;
    let before = one_lane(vec![a, a2, b, c]);
    let (_, out) = rippled(&before, |t| t.tracks[0].clips.retain(|k| k.id != b_id));
    assert_eq!(spans_of(&out, 0), vec![(0.0, 4.0), (2.0, 6.0), (12.0, 14.0)]);
}

#[test]
fn overlays_and_markers_stay_where_they_were() {
    let (mut before, [a, ..]) = gapped();
    before.overlays.push(TextOverlay::new("title", 6.0, 9.0));
    before.markers.push(Marker {
        id: Uuid::new_v4(),
        time: 7.0,
        name: "beat".to_string(),
        ..Default::default()
    });
    let (_, out) = rippled(&before, |t| clip_mut(t, a).source_out = 3.0);
    assert_eq!(out.overlays[0].start, 6.0);
    assert_eq!(out.markers[0].time, 7.0);
    assert_eq!(spans_of(&out, 0)[1], (4.0, 7.0), "while the clips did move");
}

#[test]
fn an_edit_that_changes_no_timing_comes_back_untouched() {
    let (before, [a, ..]) = gapped();
    let (after, out) = rippled(&before, |t| {
        clip_mut(t, a).volume = 0.4;
        t.tracks[0].muted = true;
    });
    assert!(same_cut(&after, &out));
    assert!(same_cut(&before.ripple_from(&before), &before));
}

#[test]
fn rippling_twice_is_rippling_once() {
    let (before, [_, b, _]) = gapped();
    let (after, once) = rippled(&before, |t| clip_mut(t, b).source_out = 1.0);
    let twice = once.ripple_from(&before);
    assert!(same_cut(&once, &twice));
    assert!(!same_cut(&after, &once));
}

// ---- multi-clip moves and removals ------------------------------------------

fn mv(clip: &Clip, start: f64) -> ClipMove {
    ClipMove {
        clip_id: clip.id,
        timeline_start: start,
        track_id: None,
    }
}

fn invalid(result: Result<Vec<Clip>>) -> String {
    match result {
        Err(Error::InvalidArgument(why)) => why,
        other => panic!("expected an InvalidArgument, got {other:?}"),
    }
}

#[test]
fn a_group_moves_together_and_may_pass_through_the_places_it_is_leaving() {
    // Three abutting clips nudged by a second: each lands on its neighbour's
    // old spot, which moving them one at a time would reject.
    let clips = vec![rclip(0.0, 2.0), rclip(2.0, 2.0), rclip(4.0, 2.0)];
    let mut t = one_lane(clips.clone());
    let moved = t
        .move_clips(&[mv(&clips[2], 5.0), mv(&clips[0], 1.0), mv(&clips[1], 3.0)])
        .unwrap();
    assert_eq!(
        moved.iter().map(|c| c.timeline_start).collect::<Vec<_>>(),
        vec![5.0, 1.0, 3.0],
        "reported in request order"
    );
    assert_eq!(spans_of(&t, 0), vec![(1.0, 3.0), (3.0, 5.0), (5.0, 7.0)], "lane re-sorted");
}

#[test]
fn clips_can_swap_places() {
    let clips = vec![rclip(0.0, 2.0), rclip(2.0, 2.0)];
    let mut t = one_lane(clips.clone());
    t.move_clips(&[mv(&clips[0], 2.0), mv(&clips[1], 0.0)]).unwrap();
    assert_eq!(t.tracks[0].clips[0].id, clips[1].id);
    assert_eq!(spans_of(&t, 0), vec![(0.0, 2.0), (2.0, 4.0)]);
}

#[test]
fn a_group_can_change_tracks_of_the_same_kind() {
    let a = rclip(0.0, 2.0);
    let mut t = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![a.clone()]),
            track(StreamKind::Video, "V2", vec![rclip(0.0, 1.0)]),
            track(StreamKind::Audio, "A1", vec![]),
        ],
        ..Timeline::new()
    };
    let (v2, a1) = (t.tracks[1].id, t.tracks[2].id);

    let up = ClipMove {
        track_id: Some(v2),
        ..mv(&a, 1.0)
    };
    t.move_clips(&[up]).unwrap();
    assert!(t.tracks[0].clips.is_empty());
    assert_eq!(spans_of(&t, 1), vec![(0.0, 1.0), (1.0, 3.0)]);

    let wrong_kind = ClipMove {
        track_id: Some(a1),
        ..mv(&a, 1.0)
    };
    assert!(invalid(t.move_clips(&[wrong_kind])).contains("different kind"));
}

#[test]
fn a_move_that_cannot_land_refuses_the_whole_group_and_changes_nothing() {
    let clips = vec![rclip(0.0, 2.0), rclip(2.0, 2.0), rclip(10.0, 2.0)];
    let mut t = one_lane(clips.clone());
    let untouched = t.clone();
    let nowhere = Uuid::new_v4();

    // One good move and one that lands on the clip that is staying.
    let why = invalid(t.move_clips(&[mv(&clips[0], 5.0), mv(&clips[1], 9.5)]));
    assert!(why.contains("overlap") && why.contains("V1"), "{why}");
    // Two moved clips on top of each other.
    assert!(invalid(t.move_clips(&[mv(&clips[0], 5.0), mv(&clips[1], 6.0)])).contains("overlap"));
    // The same clip twice, a start before 0, a start that is not a number.
    assert!(invalid(t.move_clips(&[mv(&clips[0], 5.0), mv(&clips[0], 6.0)])).contains("more than once"));
    assert!(invalid(t.move_clips(&[mv(&clips[0], -1.0)])).contains("before the beginning"));
    assert!(invalid(t.move_clips(&[mv(&clips[0], f64::NAN)])).contains("finite"));
    assert!(invalid(t.move_clips(&[])).contains("no clips"));
    // An unknown clip and an unknown track.
    assert!(matches!(
        t.move_clips(&[mv(&clips[0], 5.0), mv(&rclip(0.0, 1.0), 6.0)]),
        Err(Error::ClipNotFound(_))
    ));
    assert!(matches!(
        t.move_clips(&[ClipMove {
            track_id: Some(nowhere),
            ..mv(&clips[0], 5.0)
        }]),
        Err(Error::TrackNotFound(id)) if id == nowhere
    ));
    // A locked track, as the source…
    let mut locked_lane = t.clone();
    locked_lane.tracks[0].locked = true;
    assert!(invalid(locked_lane.move_clips(&[mv(&clips[0], 5.0)])).contains("locked"));
    // …and as the destination.
    let mut two = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![clips[0].clone()]),
            Track {
                locked: true,
                ..Track::new(StreamKind::Video, "V2")
            },
        ],
        ..Timeline::new()
    };
    let v2 = two.tracks[1].id;
    let to_locked = ClipMove {
        track_id: Some(v2),
        ..mv(&clips[0], 0.0)
    };
    assert!(invalid(two.move_clips(&[to_locked])).contains("V2 is locked"));

    assert!(same_cut(&t, &untouched), "every refusal left the lane exactly as it was");
}

#[test]
fn removing_several_clips_is_all_or_nothing() {
    let clips = vec![rclip(0.0, 2.0), rclip(2.0, 2.0), rclip(4.0, 2.0)];
    let mut t = one_lane(clips.clone());

    // A clip named twice goes once.
    assert_eq!(t.remove_clips(&[clips[0].id, clips[2].id, clips[0].id]).unwrap(), 2);
    assert_eq!(spans_of(&t, 0), vec![(2.0, 4.0)], "gaps are left");

    // One unknown id refuses the lot.
    let before = t.clone();
    assert!(matches!(
        t.remove_clips(&[clips[1].id, Uuid::new_v4()]),
        Err(Error::ClipNotFound(_))
    ));
    assert!(same_cut(&t, &before));
    assert!(matches!(t.remove_clips(&[]), Err(Error::InvalidArgument(_))));

    // So does a locked track.
    t.tracks[0].locked = true;
    assert!(matches!(t.remove_clips(&[clips[1].id]), Err(Error::InvalidArgument(why)) if why.contains("locked")));
    assert!(t.tracks[0].locked && t.tracks[0].clips.len() == 1);
}

#[test]
fn clips_moved_since_says_whether_an_edit_rippled() {
    let (t, [a, b, c]) = gapped();
    assert_eq!(t.clips_moved_since(&t), 0);

    // A bare remove leaves a gap; ripple closes it and b and c follow.
    let (bare, out) = rippled(&t, |tl| tl.tracks[0].clips.retain(|x| x.id != a));
    assert_eq!(bare.clips_moved_since(&t), 0);
    assert_eq!(out.clips_moved_since(&t), 2);

    // Removing the last clip has nothing after it to move: ripple on, nothing rippled.
    let (_, out) = rippled(&t, |tl| tl.tracks[0].clips.retain(|x| x.id != c));
    assert_eq!(out.clips_moved_since(&t), 0);

    // A locked track never moves, so the same removal moves nothing there.
    let mut locked = t.clone();
    locked.tracks[0].locked = true;
    let (_, out) = rippled(&locked, |tl| tl.tracks[0].clips.retain(|x| x.id != a));
    assert_eq!(out.clips_moved_since(&locked), 0);

    // A clip on another track counts, a clip that exists on one side only does not.
    let mut two = t;
    two.tracks.push(track(StreamKind::Video, "V2", vec![]));
    let mut moved = two.clone();
    let clip = moved.tracks[0].clips.remove(1);
    assert_eq!(clip.id, b);
    moved.tracks[1].clips.push(clip);
    assert_eq!(moved.clips_moved_since(&two), 1);
    moved.tracks[1].clips.push(rclip(0.0, 1.0));
    assert_eq!(moved.clips_moved_since(&two), 1, "an added clip is not a moved one");
}

// ---- edit modes: roll, slip, slide, split-and-remove ------------------------

/// Footage of `secs` for the asset every `sclip` / `rclip` points at.
fn footage(secs: f64) -> SourceLimits {
    HashMap::from([(Uuid::nil(), secs)])
}

/// A clip of the shared test asset: source `[si, so)` placed at `start`.
fn sclip(si: f64, so: f64, start: f64) -> Clip {
    Clip::new(Uuid::nil(), si, so, start)
}

fn window(c: &Clip) -> (f64, f64) {
    (c.source_in, c.source_out)
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} is not {b}");
}

/// The reason an op refused, with the timeline it was run on shown unchanged.
fn refused<T: std::fmt::Debug>(result: Result<T>) -> String {
    match result {
        Err(Error::InvalidArgument(why)) => why,
        other => panic!("expected an InvalidArgument, got {other:?}"),
    }
}

fn kf(time: f64, scale: f64) -> Keyframe {
    Keyframe {
        time,
        scale,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    }
}

fn scales(c: &Clip) -> Vec<(f64, f64)> {
    c.keyframes.iter().map(|k| (k.time, k.scale)).collect()
}

/// `a [0,4)` src 10..14 abutting `b [4,8)` src 20..24.
fn cut_pair() -> (Timeline, Uuid, Uuid) {
    let (a, b) = (sclip(10.0, 14.0, 0.0), sclip(20.0, 24.0, 4.0));
    let ids = (a.id, b.id);
    (one_lane(vec![a, b]), ids.0, ids.1)
}

#[test]
fn a_roll_moves_the_cut_and_leaves_the_stretch_it_spans_alone() {
    let (mut t, a, b) = cut_pair();
    let out = t.roll_edit(a, b, 1.0, &footage(60.0)).unwrap();
    assert_eq!((out.requested, out.applied, out.clamped), (1.0, 1.0, false));
    assert_eq!(out.clips.len(), 2);
    let (ca, cb) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1]);
    assert_eq!(window(ca), (10.0, 15.0), "the outgoing clip gained a second");
    assert_eq!(window(cb), (21.0, 24.0), "the incoming one gave it up");
    assert_eq!(spans_of(&t, 0), vec![(0.0, 5.0), (5.0, 8.0)], "the pair still spans 0..8");

    // And back past where it started: a negative delta rolls the cut earlier.
    t.roll_edit(a, b, -2.5, &footage(60.0)).unwrap();
    let (ca, cb) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1]);
    assert_eq!(window(ca), (10.0, 12.5));
    assert_eq!(window(cb), (18.5, 24.0));
    assert_eq!(spans_of(&t, 0), vec![(0.0, 2.5), (2.5, 8.0)]);
}

#[test]
fn a_roll_clamps_to_the_footage_each_clip_has_left_and_says_so() {
    // A 6 s asset: `a` (0..4) has 2 s after its out-point, `b` (1..5) has 1 s before its in-point.
    let (a, b) = (sclip(0.0, 4.0, 0.0), sclip(1.0, 5.0, 4.0));
    let (ia, ib) = (a.id, b.id);
    let t = one_lane(vec![a, b]);

    let mut later = t.clone();
    let out = later.roll_edit(ia, ib, 5.0, &footage(6.0)).unwrap();
    assert_eq!((out.requested, out.applied, out.clamped), (5.0, 2.0, true));
    assert_eq!(window(&later.tracks[0].clips[0]), (0.0, 6.0), "all the footage `a` had left");
    assert_eq!(later.tracks[0].clips[1].timeline_start, 6.0);

    let mut earlier = t.clone();
    let out = earlier.roll_edit(ia, ib, -5.0, &footage(6.0)).unwrap();
    assert_eq!((out.applied, out.clamped), (-1.0, true));
    assert_eq!(
        window(&earlier.tracks[0].clips[1]),
        (0.0, 5.0),
        "all the footage `b` had before it"
    );
    assert_eq!(spans_of(&earlier, 0), vec![(0.0, 3.0), (3.0, 8.0)]);

    // The range an interactive drag holds the pointer to is the same numbers.
    let range = t.roll_range(ia, ib, &footage(6.0)).unwrap();
    assert_eq!((range.min, range.max), (-1.0, 2.0));
}

#[test]
fn a_roll_never_leaves_a_clip_shorter_than_the_floor() {
    let (a, b) = (sclip(10.0, 11.0, 0.0), sclip(20.0, 21.0, 1.0));
    let (ia, ib) = (a.id, b.id);
    let t = one_lane(vec![a, b]);

    let mut later = t.clone();
    near(later.roll_edit(ia, ib, 5.0, &footage(60.0)).unwrap().applied, 0.95);
    near(later.tracks[0].clips[1].duration(), MIN_EDIT_CLIP);

    let mut earlier = t;
    near(earlier.roll_edit(ia, ib, -5.0, &footage(60.0)).unwrap().applied, -0.95);
    near(earlier.tracks[0].clips[0].duration(), MIN_EDIT_CLIP);
}

#[test]
fn a_roll_honors_speed() {
    // `a` runs at 2x over 0..8 (4 s on the timeline) in a 10 s asset: 2 source
    // seconds left are 1 timeline second. `b` runs at half speed.
    let mut a = sclip(0.0, 8.0, 0.0);
    a.speed = 2.0;
    let mut b = sclip(2.0, 3.0, 4.0); // 1 source s at 0.5x: 2 s long
    b.speed = 0.5;
    let (ia, ib) = (a.id, b.id);
    let t = one_lane(vec![a, b]);

    let mut later = t.clone();
    let out = later.roll_edit(ia, ib, 3.0, &footage(10.0)).unwrap();
    assert_eq!((out.applied, out.clamped), (1.0, true));
    let (ca, cb) = (&later.tracks[0].clips[0], &later.tracks[0].clips[1]);
    assert_eq!(window(ca), (0.0, 10.0), "one timeline second at 2x is two source seconds");
    near(cb.source_in, 2.5); // one timeline second at 0.5x is half a source second
    assert_eq!((cb.timeline_start, cb.duration()), (5.0, 1.0));

    let mut earlier = t;
    earlier.roll_edit(ia, ib, -1.0, &footage(10.0)).unwrap();
    let (ca, cb) = (&earlier.tracks[0].clips[0], &earlier.tracks[0].clips[1]);
    assert_eq!(window(ca), (0.0, 6.0));
    near(cb.source_in, 1.5);
    assert_eq!((cb.timeline_start, cb.duration()), (3.0, 3.0));
}

#[test]
fn a_roll_honors_reverse_the_outgoing_edge_is_the_in_point() {
    // Both clips play backwards over 5..9. A reversed clip's *end* is its
    // in-point, so `a` extends downwards (5 s of footage below it), and `b`'s
    // start is its out-point.
    let (mut a, mut b) = (sclip(5.0, 9.0, 0.0), sclip(5.0, 9.0, 4.0));
    a.speed = -1.0;
    b.speed = -1.0;
    let (ia, ib) = (a.id, b.id);
    let t = one_lane(vec![a, b]);

    let mut later = t.clone();
    let out = later.roll_edit(ia, ib, 2.0, &footage(60.0)).unwrap();
    assert_eq!(out.applied, 2.0);
    assert_eq!(window(&later.tracks[0].clips[0]), (3.0, 9.0), "a: in-point down by 2");
    assert_eq!(window(&later.tracks[0].clips[1]), (5.0, 7.0), "b: out-point down by 2");
    assert_eq!(spans_of(&later, 0), vec![(0.0, 6.0), (6.0, 8.0)]);

    let mut earlier = t;
    earlier.roll_edit(ia, ib, -2.0, &footage(60.0)).unwrap();
    assert_eq!(window(&earlier.tracks[0].clips[0]), (7.0, 9.0), "a: in-point up by 2");
    assert_eq!(window(&earlier.tracks[0].clips[1]), (5.0, 11.0), "b: out-point up by 2");

    // The handle is the one on the *playing* side: here `a` has 1 s below its
    // in-point and `b` only half a second above its out-point.
    let (mut a, mut b) = (sclip(1.0, 5.0, 0.0), sclip(5.0, 59.5, 4.0));
    a.speed = -1.0;
    b.speed = -1.0;
    let (ia, ib) = (a.id, b.id);
    let t = one_lane(vec![a, b]);
    let range = t.roll_range(ia, ib, &footage(60.0)).unwrap();
    assert_eq!((range.min, range.max), (-0.5, 1.0));
}

#[test]
fn a_roll_through_a_still_has_footage_without_limit_and_never_a_negative_window() {
    let still = Uuid::new_v4();
    let limits: SourceLimits = HashMap::from([(Uuid::nil(), 60.0), (still, f64::INFINITY)]);

    // The still is the incoming clip: rolling earlier lengthens it freely, written
    // on its out-point so the window never reaches below 0.
    let (a, b) = (sclip(10.0, 15.0, 0.0), Clip::new(still, 0.0, 5.0, 5.0));
    let (ia, ib) = (a.id, b.id);
    let mut t = one_lane(vec![a, b]);
    t.roll_edit(ia, ib, -2.0, &limits).unwrap();
    let cb = &t.tracks[0].clips[1];
    assert_eq!((cb.timeline_start, cb.duration()), (3.0, 7.0));
    assert_eq!(window(cb), (0.0, 7.0));

    // The still is the outgoing clip: nothing stops it growing but the other clip.
    let (a, b) = (Clip::new(still, 0.0, 5.0, 0.0), sclip(10.0, 15.0, 5.0));
    let (ia, ib) = (a.id, b.id);
    let mut t = one_lane(vec![a, b]);
    near(t.roll_edit(ia, ib, 20.0, &limits).unwrap().applied, 4.95);
    assert_eq!(window(&t.tracks[0].clips[0]).0, 0.0);
}

#[test]
fn a_roll_needs_two_adjacent_clips_in_order_on_one_unlocked_track() {
    let (t, a, b) = cut_pair();
    let f = footage(60.0);
    let run = |name: &str, tl: &Timeline, ia: Uuid, ib: Uuid, delta: f64| {
        let mut copy = tl.clone();
        let why = refused(copy.roll_edit(ia, ib, delta, &f));
        assert!(same_cut(&copy, tl), "{name}: a refused roll must change nothing");
        why
    };

    assert!(run("swapped", &t, b, a, 1.0).contains("clip_a must be the earlier clip"));
    assert!(run("itself", &t, a, a, 1.0).contains("two different clips"));
    assert!(run("zero", &t, a, b, 0.0).contains("zero"));
    assert!(run("nan", &t, a, b, f64::NAN).contains("finite"));

    // A gap, an overlap.
    let mut gapped = t.clone();
    gapped.tracks[0].clips[1].timeline_start = 5.0;
    assert!(run("gap", &gapped, a, b, 1.0).contains("1.00s gap"));
    let mut overlapped = t.clone();
    overlapped.tracks[0].clips[1].timeline_start = 3.5;
    assert!(run("overlap", &overlapped, a, b, 1.0).contains("overlap by 0.50s"));

    // Different tracks.
    let two = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![t.tracks[0].clips[0].clone()]),
            track(StreamKind::Video, "V2", vec![t.tracks[0].clips[1].clone()]),
        ],
        ..Timeline::new()
    };
    assert!(run("tracks", &two, a, b, 1.0).contains("different tracks (V1 and V2)"));

    // Locked, and unknown ids.
    let mut locked = t.clone();
    locked.tracks[0].locked = true;
    assert!(run("locked", &locked, a, b, 1.0).contains("V1 is locked"));
    let mut copy = t.clone();
    assert!(matches!(
        copy.roll_edit(a, Uuid::new_v4(), 1.0, &f),
        Err(Error::ClipNotFound(_))
    ));

    // An asset the footage table does not know cannot be clamped.
    let mut copy = t;
    assert!(matches!(
        copy.roll_edit(a, b, 1.0, &SourceLimits::new()),
        Err(Error::AssetNotFound(_))
    ));
}

#[test]
fn a_roll_that_clamps_to_nothing_is_an_error_that_says_why() {
    // `a` already ends at the end of its asset; `b` starts at the start of its.
    let (a, b) = (sclip(0.0, 6.0, 0.0), sclip(0.0, 3.0, 6.0));
    let (ia, ib) = (a.id, b.id);
    let mut t = one_lane(vec![a, b]);
    let before = t.clone();
    let why = refused(t.roll_edit(ia, ib, 1.0, &footage(6.0)));
    assert!(
        why.contains("cannot roll the cut later") && why.contains("outgoing clip has no footage left"),
        "{why}"
    );
    let why = refused(t.roll_edit(ia, ib, -1.0, &footage(6.0)));
    assert!(
        why.contains("cannot roll the cut earlier") && why.contains("incoming clip has no footage left"),
        "{why}"
    );
    assert!(same_cut(&t, &before));
}

#[test]
fn two_clips_a_hair_apart_still_share_a_cut() {
    // The engine treats edges within a millisecond as touching (transition_fx).
    let (mut t, a, b) = cut_pair();
    t.tracks[0].clips[1].timeline_start = 4.0005;
    let out = t.roll_edit(a, b, 1.0, &footage(60.0)).unwrap();
    assert_eq!(out.applied, 1.0);
    near(t.tracks[0].clips[1].timeline_start, 5.0005);

    let (mut t, a, b) = cut_pair();
    t.tracks[0].clips[1].timeline_start = 4.002;
    assert!(refused(t.roll_edit(a, b, 1.0, &footage(60.0))).contains("not adjacent"));
}

#[test]
fn a_roll_carries_the_incoming_clips_animation_with_its_footage() {
    let (mut t, a, b) = cut_pair();
    t.tracks[0].clips[0].keyframes = vec![kf(0.0, 1.0), kf(2.0, 3.0)];
    t.tracks[0].clips[1].keyframes = vec![kf(0.0, 1.0), kf(2.0, 2.0)];
    let mut rf = Reframe::new(Projection::Equirect);
    rf.keyframes = vec![
        ReframeKeyframe {
            time: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            fov: 100.0,
        },
        ReframeKeyframe {
            time: 2.0,
            yaw: 90.0,
            pitch: 0.0,
            roll: 0.0,
            fov: 100.0,
        },
    ];
    t.tracks[0].clips[1].reframe = Some(rf);

    // Rolled later, b loses its first second: the pose it now opens on is
    // pinned at 0 and the later key moves up.
    let mut later = t.clone();
    later.roll_edit(a, b, 1.0, &footage(60.0)).unwrap();
    assert_eq!(scales(&later.tracks[0].clips[1]), vec![(0.0, 1.5), (1.0, 2.0)]);
    let rk: Vec<(f64, f64)> = later.tracks[0].clips[1]
        .reframe
        .as_ref()
        .unwrap()
        .keyframes
        .iter()
        .map(|k| (k.time, k.yaw))
        .collect();
    assert_eq!(rk, vec![(0.0, 45.0), (1.0, 90.0)], "the camera moves with the footage too");
    assert_eq!(
        scales(&later.tracks[0].clips[0]),
        vec![(0.0, 1.0), (2.0, 3.0)],
        "the outgoing clip's head did not move"
    );

    // Rolled earlier, b gains a second at its head: every key shifts later.
    let mut earlier = t;
    earlier.roll_edit(a, b, -1.0, &footage(60.0)).unwrap();
    assert_eq!(scales(&earlier.tracks[0].clips[1]), vec![(1.0, 1.0), (3.0, 2.0)]);
}

#[test]
fn a_roll_keeps_fades_inside_a_clip_that_shrank_and_the_transition_on_the_cut() {
    let (mut t, a, b) = cut_pair();
    t.tracks[0].clips[0].fade_out = 2.0;
    t.tracks[0].clips[0].fade_in = 0.5;
    t.tracks[0].clips[1].fade_in = 3.0;
    let transition = Transition {
        kind: TransitionKind::Crossfade,
        duration: 1.0,
    };
    t.tracks[0].clips[1].transition_in = Some(transition);

    t.roll_edit(a, b, -3.9, &footage(60.0)).unwrap(); // a is 0.1 s now
    let (ca, cb) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1]);
    near(ca.fade_out, 0.1);
    near(ca.fade_in, 0.1);
    assert_eq!(cb.fade_in, 3.0, "b grew, so its fade stays");
    assert_eq!(cb.transition_in, Some(transition));

    t.roll_edit(a, b, 10.0, &footage(60.0)).unwrap(); // b shrinks to the floor
    near(t.tracks[0].clips[1].fade_in, MIN_EDIT_CLIP);
}

#[test]
fn a_slip_moves_the_window_and_nothing_on_the_timeline() {
    let mut c = sclip(10.0, 14.0, 3.0);
    c.keyframes = vec![kf(0.0, 1.0), kf(2.0, 2.0)];
    c.fade_in = 0.5;
    let id = c.id;
    let mut t = one_lane(vec![c.clone()]);

    let out = t.slip_clip(id, 2.0, &footage(60.0)).unwrap();
    assert_eq!((out.applied, out.clamped), (2.0, false));
    let slipped = &t.tracks[0].clips[0];
    assert_eq!(window(slipped), (12.0, 16.0));
    assert_eq!((slipped.timeline_start, slipped.duration()), (3.0, 4.0));
    // Clip-local animation and the fades are timed to the clip, not to the footage.
    assert_eq!(slipped.keyframes, c.keyframes);
    assert_eq!(slipped.fade_in, 0.5);

    t.slip_clip(id, -5.0, &footage(60.0)).unwrap();
    assert_eq!(window(&t.tracks[0].clips[0]), (7.0, 11.0));
}

#[test]
fn a_slip_clamps_to_the_asset_and_errors_at_the_edge() {
    let c = sclip(10.0, 14.0, 0.0);
    let id = c.id;
    let t = one_lane(vec![c]);

    let mut up = t.clone();
    let out = up.slip_clip(id, 10.0, &footage(20.0)).unwrap();
    assert_eq!((out.applied, out.clamped), (6.0, true));
    assert_eq!(window(&up.tracks[0].clips[0]), (16.0, 20.0));
    let why = refused(up.slip_clip(id, 1.0, &footage(20.0)));
    assert!(
        why.contains("cannot slip the footage later") && why.contains("after the clip's out-point"),
        "{why}"
    );

    let mut down = t.clone();
    near(down.slip_clip(id, -50.0, &footage(20.0)).unwrap().applied, -10.0);
    assert_eq!(window(&down.tracks[0].clips[0]), (0.0, 4.0));
    let why = refused(down.slip_clip(id, -1.0, &footage(20.0)));
    assert!(
        why.contains("cannot slip the footage earlier") && why.contains("before the clip's in-point"),
        "{why}"
    );

    let range = t.slip_range(id, &footage(20.0)).unwrap();
    assert_eq!((range.min, range.max), (-10.0, 6.0));
}

#[test]
fn a_slip_is_in_source_seconds_whatever_the_speed() {
    let mut c = sclip(10.0, 14.0, 0.0);
    c.speed = 2.0; // 4 source seconds in 2 timeline seconds
    let id = c.id;
    let mut t = one_lane(vec![c]);
    t.slip_clip(id, 1.0, &footage(60.0)).unwrap();
    let slipped = &t.tracks[0].clips[0];
    assert_eq!(window(slipped), (11.0, 15.0), "one source second");
    assert_eq!((slipped.timeline_start, slipped.timeline_end()), (0.0, 2.0));
}

#[test]
fn a_reversed_clip_slips_the_mirrored_way_so_the_sign_means_the_same_on_screen() {
    // Positive = starts later in its own footage. A reversed clip's footage
    // runs high to low, so "later" is the lower source time.
    let mut c = sclip(10.0, 14.0, 0.0);
    c.speed = -1.0;
    let id = c.id;
    let t = one_lane(vec![c]);

    let mut later = t.clone();
    later.slip_clip(id, 3.0, &footage(20.0)).unwrap();
    assert_eq!(
        window(&later.tracks[0].clips[0]),
        (7.0, 11.0),
        "positive moves the window down"
    );
    let mut earlier = t.clone();
    earlier.slip_clip(id, -3.0, &footage(20.0)).unwrap();
    assert_eq!(window(&earlier.tracks[0].clips[0]), (13.0, 17.0), "negative moves it up");

    // The footage left is on the opposite side for each sign.
    let range = t.slip_range(id, &footage(20.0)).unwrap();
    assert_eq!((range.min, range.max), (-6.0, 10.0));
    let mut clamped = t.clone();
    near(clamped.slip_clip(id, 100.0, &footage(20.0)).unwrap().applied, 10.0);
    assert_eq!(window(&clamped.tracks[0].clips[0]), (0.0, 4.0));
    let mut clamped = t;
    near(clamped.slip_clip(id, -100.0, &footage(20.0)).unwrap().applied, -6.0);
    assert_eq!(window(&clamped.tracks[0].clips[0]), (16.0, 20.0));
}

#[test]
fn a_still_has_nothing_to_slip_and_the_other_refusals() {
    let still = Uuid::new_v4();
    let limits: SourceLimits = HashMap::from([(Uuid::nil(), 60.0), (still, f64::INFINITY)]);
    let c = Clip::new(still, 0.0, 5.0, 0.0);
    let id = c.id;
    let mut t = one_lane(vec![c]);
    assert!(refused(t.slip_clip(id, 1.0, &limits)).contains("still image"));
    assert!(refused(t.slip_range(id, &limits).map(|_| ())).contains("still image"));

    let (mut t, a, _) = cut_pair();
    assert!(refused(t.slip_clip(a, 0.0, &footage(60.0))).contains("zero"));
    assert!(refused(t.slip_clip(a, f64::INFINITY, &footage(60.0))).contains("finite"));
    assert!(matches!(
        t.slip_clip(Uuid::new_v4(), 1.0, &footage(60.0)),
        Err(Error::ClipNotFound(_))
    ));
    t.tracks[0].locked = true;
    assert!(refused(t.slip_clip(a, 1.0, &footage(60.0))).contains("V1 is locked"));
}

/// `p [0,4)` src 10..14, `c [4,7)` src 30..33, `n [7,12)` src 20..25, abutting.
fn three() -> (Timeline, [Uuid; 3]) {
    let (p, c, n) = (sclip(10.0, 14.0, 0.0), sclip(30.0, 33.0, 4.0), sclip(20.0, 25.0, 7.0));
    let ids = [p.id, c.id, n.id];
    (one_lane(vec![p, c, n]), ids)
}

#[test]
fn a_slide_moves_the_clip_and_the_neighbours_give_way() {
    let (t, [p, c, n]) = three();

    let mut later = t.clone();
    let out = later.slide_clip(c, 1.0, &footage(60.0)).unwrap();
    assert_eq!((out.requested, out.applied, out.clamped), (1.0, 1.0, false));
    assert_eq!(
        out.clips.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![p, c, n],
        "in timeline order"
    );
    assert_eq!(
        spans_of(&later, 0),
        vec![(0.0, 5.0), (5.0, 8.0), (8.0, 12.0)],
        "the three still span 0..12"
    );
    let cl = &later.tracks[0].clips;
    assert_eq!(window(&cl[0]), (10.0, 15.0), "the previous clip's out moved");
    assert_eq!(window(&cl[1]), (30.0, 33.0), "the slid clip's content did not");
    assert_eq!(window(&cl[2]), (21.0, 25.0), "the next clip's in moved");

    let mut earlier = t;
    earlier.slide_clip(c, -1.5, &footage(60.0)).unwrap();
    assert_eq!(spans_of(&earlier, 0), vec![(0.0, 2.5), (2.5, 5.5), (5.5, 12.0)]);
    let cl = &earlier.tracks[0].clips;
    assert_eq!(window(&cl[0]), (10.0, 12.5));
    assert_eq!(window(&cl[1]), (30.0, 33.0));
    assert_eq!(window(&cl[2]), (18.5, 25.0));
}

#[test]
fn a_slide_clamps_to_the_neighbours_footage_and_floor() {
    let (t, [_, c, _]) = three();

    // Later: the next clip (5 s) may only shrink to the floor.
    let mut later = t.clone();
    let out = later.slide_clip(c, 10.0, &footage(60.0)).unwrap();
    assert_eq!((out.applied, out.clamped), (4.95, true));
    near(later.tracks[0].clips[2].duration(), MIN_EDIT_CLIP);
    // Earlier: the previous clip (4 s) may only shrink to the floor.
    let mut earlier = t.clone();
    near(earlier.slide_clip(c, -10.0, &footage(60.0)).unwrap().applied, -3.95);
    near(earlier.tracks[0].clips[0].duration(), MIN_EDIT_CLIP);

    // A 14 s asset: the previous clip (10..14) has no footage after it to grow into.
    let mut dry = t.clone();
    let why = refused(dry.slide_clip(c, 1.0, &footage(14.0)));
    assert!(
        why.contains("cannot slide the clip later") && why.contains("previous clip has no footage left"),
        "{why}"
    );
    // …but the other way is fine, and the next clip's footage before 20 is.
    near(dry.slide_clip(c, -1.0, &footage(14.0)).unwrap().applied, -1.0);

    // The next clip's head handle bounds the earlier direction too (n starts at source 20).
    let (mut tight, ids) = three();
    tight.tracks[0].clips[2].source_in = 0.5;
    let out = tight.slide_clip(ids[1], -2.0, &footage(60.0)).unwrap();
    assert_eq!((out.applied, out.clamped), (-0.5, true));
    let range = t.slide_range(c, &footage(60.0)).unwrap();
    assert_eq!((range.min, range.max), (-3.95, 4.95));
}

#[test]
fn a_slide_beside_a_gap_trims_only_the_neighbour_that_touches() {
    // p [0,4) touches c [4,7); n starts at 9 — a 2 s gap after c.
    let (p, c, n) = (sclip(10.0, 14.0, 0.0), sclip(30.0, 33.0, 4.0), sclip(20.0, 25.0, 9.0));
    let ids = [p.id, c.id, n.id];
    let t = one_lane(vec![p, c, n]);

    let mut later = t.clone();
    let out = later.slide_clip(ids[1], 1.0, &footage(60.0)).unwrap();
    assert_eq!(
        out.clips.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![ids[0], ids[1]],
        "n is not touched"
    );
    let cl = &later.tracks[0].clips;
    assert_eq!(window(&cl[0]), (10.0, 15.0), "p grew to follow the clip");
    assert_eq!(cl[1].timeline_start, 5.0);
    assert_eq!(
        (cl[2].timeline_start, window(&cl[2])),
        (9.0, (20.0, 25.0)),
        "n stayed exactly as it was"
    );

    // It stops where it would meet n, which it is not touching, rather than trimming it.
    let mut far = t.clone();
    let out = far.slide_clip(ids[1], 5.0, &footage(60.0)).unwrap();
    assert_eq!((out.applied, out.clamped), (2.0, true));
    assert_eq!(far.tracks[0].clips[2].timeline_start, 9.0);
    assert_eq!(far.tracks[0].clips[1].timeline_end(), 9.0);

    // The other way the gap just grows; p gives way as before.
    let mut earlier = t;
    earlier.slide_clip(ids[1], -1.0, &footage(60.0)).unwrap();
    assert_eq!(spans_of(&earlier, 0), vec![(0.0, 3.0), (3.0, 6.0), (9.0, 14.0)]);
}

#[test]
fn a_slide_after_a_gap_leaves_the_previous_clip_alone() {
    // p [0,3), a 1 s gap, c [4,7) touching n [7,12).
    let (p, c, n) = (sclip(10.0, 13.0, 0.0), sclip(30.0, 33.0, 4.0), sclip(20.0, 25.0, 7.0));
    let ids = [p.id, c.id, n.id];
    let t = one_lane(vec![p, c, n]);

    let mut earlier = t.clone();
    let out = earlier.slide_clip(ids[1], -0.5, &footage(60.0)).unwrap();
    assert_eq!(out.clips.iter().map(|x| x.id).collect::<Vec<_>>(), vec![ids[1], ids[2]]);
    let cl = &earlier.tracks[0].clips;
    assert_eq!((cl[0].timeline_end(), window(&cl[0])), (3.0, (10.0, 13.0)), "p untouched");
    assert_eq!(cl[1].timeline_start, 3.5);
    assert_eq!(
        (cl[2].timeline_start, window(&cl[2])),
        (6.5, (19.5, 25.0)),
        "n extends earlier"
    );

    let mut far = t.clone();
    let out = far.slide_clip(ids[1], -3.0, &footage(60.0)).unwrap();
    assert_eq!(
        (out.applied, out.clamped),
        (-1.0, true),
        "stops at p, which it is not touching"
    );
    assert_eq!(far.tracks[0].clips[1].timeline_start, 3.0);

    // Later, the gap grows and n is trimmed.
    let mut later = t;
    later.slide_clip(ids[1], 1.0, &footage(60.0)).unwrap();
    assert_eq!(spans_of(&later, 0), vec![(0.0, 3.0), (5.0, 8.0), (8.0, 12.0)]);
}

#[test]
fn a_slide_at_the_ends_of_a_track() {
    // First clip: nothing before it, so it cannot go below 0…
    let (c, n) = (sclip(30.0, 33.0, 2.0), sclip(20.0, 25.0, 5.0));
    let ids = [c.id, n.id];
    let mut t = one_lane(vec![c, n]);
    let out = t.slide_clip(ids[0], -5.0, &footage(60.0)).unwrap();
    assert_eq!((out.applied, out.clamped), (-2.0, true));
    assert_eq!(spans_of(&t, 0), vec![(0.0, 3.0), (3.0, 10.0)], "n extended earlier to follow");
    let why = refused(t.slide_clip(ids[0], -1.0, &footage(60.0)));
    assert!(why.contains("already at the start of the timeline"), "{why}");
    // …and sliding it later opens a gap before it while n gives way.
    t.slide_clip(ids[0], 1.0, &footage(60.0)).unwrap();
    assert_eq!(spans_of(&t, 0), vec![(1.0, 4.0), (4.0, 10.0)]);

    // Last clip: nothing after it to give way, so the track's end moves with it.
    let (p, c) = (sclip(10.0, 14.0, 0.0), sclip(30.0, 33.0, 4.0));
    let ids = [p.id, c.id];
    let mut t = one_lane(vec![p, c]);
    assert_eq!(t.duration(), 7.0);
    let out = t.slide_clip(ids[1], 3.0, &footage(60.0)).unwrap();
    assert_eq!(out.applied, 3.0);
    assert_eq!(spans_of(&t, 0), vec![(0.0, 7.0), (7.0, 10.0)]);
    assert_eq!(t.duration(), 10.0);

    // Free space on both sides: a plain move, bounded by what it would run into.
    let (p, c, n) = (sclip(10.0, 11.0, 0.0), sclip(30.0, 33.0, 3.0), sclip(20.0, 21.0, 9.0));
    let ids = [p.id, c.id, n.id];
    let mut t = one_lane(vec![p, c, n]);
    let out = t.slide_clip(ids[1], 5.0, &footage(60.0)).unwrap();
    assert_eq!((out.applied, out.clips.len()), (3.0, 1));
    assert_eq!(spans_of(&t, 0), vec![(0.0, 1.0), (6.0, 9.0), (9.0, 10.0)]);
}

#[test]
fn a_slide_honors_the_neighbours_speed_and_direction() {
    // p plays backwards (its end is its in-point), n at 2x.
    let mut p = sclip(10.0, 14.0, 0.0);
    p.speed = -1.0;
    let c = sclip(30.0, 33.0, 4.0);
    let mut n = sclip(20.0, 30.0, 7.0); // 10 source s at 2x: 5 s
    n.speed = 2.0;
    let ids = [p.id, c.id, n.id];
    let mut t = one_lane(vec![p, c, n]);

    t.slide_clip(ids[1], 2.0, &footage(60.0)).unwrap();
    let cl = &t.tracks[0].clips;
    assert_eq!(
        window(&cl[0]),
        (8.0, 14.0),
        "the reversed clip's end is its in-point: down by 2"
    );
    assert_eq!(cl[1].timeline_start, 6.0);
    assert_eq!(
        (cl[2].timeline_start, window(&cl[2])),
        (9.0, (24.0, 30.0)),
        "2 timeline seconds at 2x is 4 source"
    );
    assert_eq!(spans_of(&t, 0), vec![(0.0, 6.0), (6.0, 9.0), (9.0, 12.0)]);
}

#[test]
fn a_slide_moves_the_next_clips_animation_with_its_footage_and_not_the_slid_clips() {
    let (mut t, [p, c, n]) = three();
    for (id, keys) in [
        (p, vec![kf(0.0, 1.0), kf(2.0, 3.0)]),
        (c, vec![kf(0.0, 1.0), kf(1.0, 3.0)]),
        (n, vec![kf(0.0, 1.0), kf(4.0, 4.0)]),
    ] {
        let (ti, ci) = t.locate(id).unwrap();
        t.tracks[ti].clips[ci].keyframes = keys;
    }
    t.slide_clip(c, 1.0, &footage(60.0)).unwrap();
    let cl = &t.tracks[0].clips;
    assert_eq!(scales(&cl[0]), vec![(0.0, 1.0), (2.0, 3.0)]);
    assert_eq!(
        scales(&cl[1]),
        vec![(0.0, 1.0), (1.0, 3.0)],
        "the slid clip is only repositioned"
    );
    assert_eq!(scales(&cl[2]), vec![(0.0, 1.75), (3.0, 4.0)], "n lost its first second");
}

#[test]
fn a_slide_through_a_still_neighbour_extends_it_without_limit() {
    let still = Uuid::new_v4();
    let limits: SourceLimits = HashMap::from([(Uuid::nil(), 60.0), (still, f64::INFINITY)]);
    // The still is the previous clip: it can grow as far as the slide goes.
    let (p, c, n) = (
        Clip::new(still, 0.0, 4.0, 0.0),
        sclip(30.0, 33.0, 4.0),
        sclip(20.0, 25.0, 7.0),
    );
    let ids = [p.id, c.id, n.id];
    let mut t = one_lane(vec![p, c, n]);
    near(t.slide_clip(ids[1], 10.0, &limits).unwrap().applied, 4.95);
    assert_eq!(window(&t.tracks[0].clips[0]), (0.0, 8.95));

    // The still is the next clip: pulled earlier it lengthens, keeping a window that starts at 0.
    let (p, c, n) = (
        sclip(10.0, 14.0, 0.0),
        sclip(30.0, 33.0, 4.0),
        Clip::new(still, 0.0, 5.0, 7.0),
    );
    let ids = [p.id, c.id, n.id];
    let mut t = one_lane(vec![p, c, n]);
    near(t.slide_clip(ids[1], -2.0, &limits).unwrap().applied, -2.0);
    assert_eq!(window(&t.tracks[0].clips[2]), (0.0, 7.0));
    assert_eq!(t.tracks[0].clips[2].timeline_start, 5.0);
}

#[test]
fn slide_errors() {
    let (t, [_, c, _]) = three();
    let mut copy = t.clone();
    assert!(refused(copy.slide_clip(c, 0.0, &footage(60.0))).contains("zero"));
    assert!(refused(copy.slide_clip(c, f64::NAN, &footage(60.0))).contains("finite"));
    assert!(matches!(
        copy.slide_clip(Uuid::new_v4(), 1.0, &footage(60.0)),
        Err(Error::ClipNotFound(_))
    ));
    assert!(matches!(
        copy.slide_clip(c, 1.0, &SourceLimits::new()),
        Err(Error::AssetNotFound(_))
    ));
    assert!(same_cut(&copy, &t), "none of them changed anything");
    copy.tracks[0].locked = true;
    assert!(refused(copy.slide_clip(c, 1.0, &footage(60.0))).contains("V1 is locked"));
}

#[test]
fn split_remove_right_shortens_the_clip_at_its_end() {
    let mut c = sclip(10.0, 20.0, 5.0); // [5, 15)
    c.fade_in = 1.0;
    c.fade_out = 2.0;
    let (id, keys) = (c.id, vec![kf(0.0, 1.0), kf(8.0, 3.0)]);
    c.keyframes = keys.clone();
    let mut t = one_lane(vec![c]);

    let kept = t.split_remove(id, 9.0, SplitSide::Right).unwrap();
    assert_eq!(kept.id, id, "the surviving half keeps the clip's identity");
    assert_eq!(window(&kept), (10.0, 14.0));
    assert_eq!((kept.timeline_start, kept.timeline_end()), (5.0, 9.0));
    assert_eq!(kept.fade_in, 1.0);
    assert_eq!(kept.fade_out, 0.0, "the fade belonged to the end that was removed");
    assert_eq!(kept.keyframes, keys, "the head did not move, so the animation did not");
    assert_eq!(t.tracks[0].clips.len(), 1);
    assert_eq!(t.tracks[0].clips[0].timeline_end(), 9.0);
}

#[test]
fn split_remove_left_moves_the_start_up_and_drops_what_belonged_to_the_old_start() {
    let mut c = sclip(10.0, 20.0, 5.0); // [5, 15)
    c.fade_in = 1.0;
    c.fade_out = 2.0;
    c.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 1.0,
    });
    let id = c.id;
    let mut t = one_lane(vec![c]);

    let kept = t.split_remove(id, 9.0, SplitSide::Left).unwrap();
    assert_eq!(kept.id, id);
    assert_eq!(window(&kept), (14.0, 20.0));
    assert_eq!(
        (kept.timeline_start, kept.timeline_end()),
        (9.0, 15.0),
        "the right edge stays put"
    );
    assert_eq!(kept.fade_in, 0.0);
    assert_eq!(kept.transition_in, None);
    assert_eq!(kept.fade_out, 2.0, "the end is still the clip's own");
}

#[test]
fn split_remove_honors_speed_and_reverse() {
    // A reversed clip at 2x over 10..20: 5 s on the timeline, starting at its out-point.
    let mut c = sclip(10.0, 20.0, 0.0);
    c.speed = -2.0;
    let id = c.id;
    let t = one_lane(vec![c]);

    let mut left = t.clone();
    let kept = left.split_remove(id, 2.0, SplitSide::Left).unwrap();
    assert_eq!(
        window(&kept),
        (10.0, 16.0),
        "2 timeline seconds at 2x is 4 source off the out-point"
    );
    assert_eq!((kept.timeline_start, kept.duration()), (2.0, 3.0));

    let mut right = t;
    let kept = right.split_remove(id, 2.0, SplitSide::Right).unwrap();
    assert_eq!(window(&kept), (16.0, 20.0), "…and off the in-point for the end");
    assert_eq!((kept.timeline_start, kept.duration()), (0.0, 2.0));
}

#[test]
fn split_remove_left_carries_the_animation_with_the_footage_that_stays() {
    let mut c = sclip(0.0, 8.0, 0.0);
    c.keyframes = vec![kf(0.0, 1.0), kf(4.0, 5.0)];
    let id = c.id;
    let mut t = one_lane(vec![c]);
    let kept = t.split_remove(id, 1.0, SplitSide::Left).unwrap();
    assert_eq!(
        scales(&kept),
        vec![(0.0, 2.0), (3.0, 5.0)],
        "the pose at the cut is pinned, the rest shifts"
    );
}

#[test]
fn split_remove_holds_the_fade_inside_what_is_left() {
    let mut c = sclip(0.0, 4.0, 0.0);
    c.fade_in = 3.0;
    let id = c.id;
    let mut t = one_lane(vec![c]);
    let kept = t.split_remove(id, 1.0, SplitSide::Right).unwrap();
    assert_eq!(kept.fade_in, 1.0, "a 3 s fade cannot outlast a 1 s clip");
}

#[test]
fn split_remove_only_cuts_inside_the_clip_and_leaves_something() {
    let c = sclip(0.0, 4.0, 2.0); // [2, 6)
    let id = c.id;
    let t = one_lane(vec![c]);
    let run = |at: f64, side: SplitSide| {
        let mut copy = t.clone();
        let why = refused(copy.split_remove(id, at, side));
        assert!(same_cut(&copy, &t), "a refusal changes nothing");
        why
    };
    assert!(run(1.0, SplitSide::Left).contains("not inside the clip (0:02.0–0:06.0)"));
    assert!(run(7.0, SplitSide::Right).contains("not inside the clip"));
    assert!(
        run(2.0, SplitSide::Left).contains("not inside the clip"),
        "the clip's own start is not a cut"
    );
    assert!(run(6.0, SplitSide::Right).contains("not inside the clip"));
    assert!(run(f64::NAN, SplitSide::Left).contains("finite"));
    assert!(run(5.99, SplitSide::Left).contains("only 0.01s of the clip"));
    assert!(run(2.02, SplitSide::Right).contains("only 0.02s of the clip"));

    // The part that goes can be as small as it likes; only what stays has a floor.
    let mut copy = t.clone();
    near(copy.split_remove(id, 2.02, SplitSide::Left).unwrap().duration(), 3.98);

    let mut locked = t.clone();
    locked.tracks[0].locked = true;
    assert!(refused(locked.split_remove(id, 4.0, SplitSide::Left)).contains("V1 is locked"));
    let mut copy = t;
    assert!(matches!(
        copy.split_remove(Uuid::new_v4(), 4.0, SplitSide::Left),
        Err(Error::ClipNotFound(_))
    ));
}

#[test]
fn a_split_remove_under_ripple_closes_the_gap_through_the_standard_path() {
    // `a [0,4)  b [5,8)  c [10,12)`, ripple applied the way `edit_timeline` does.
    let (before, [_, b, _]) = gapped();
    let (_, right) = rippled(&before, |t| {
        t.split_remove(b, 6.0, SplitSide::Right).unwrap();
    });
    assert_eq!(
        spans_of(&right, 0),
        vec![(0.0, 4.0), (5.0, 6.0), (8.0, 10.0)],
        "c followed the 2 s that went"
    );

    let (bare, left) = rippled(&before, |t| {
        t.split_remove(b, 6.0, SplitSide::Left).unwrap();
    });
    assert_eq!(
        spans_of(&bare, 0),
        vec![(0.0, 4.0), (6.0, 8.0), (10.0, 12.0)],
        "without ripple the gap stays"
    );
    assert_eq!(
        spans_of(&left, 0),
        vec![(0.0, 4.0), (5.0, 7.0), (9.0, 11.0)],
        "with it b holds its start and the rest of the track closes in behind"
    );
}

#[test]
fn a_range_slice_pins_the_pose_at_a_cut_front_the_way_a_head_move_does() {
    // `Timeline::slice` shares `rebase_animation` with the edit modes: a clip cut
    // at the front keeps its animated pose as a key at 0, later keys shift back,
    // and the fade and transition that belonged to the lost head go.
    let mut c = sclip(0.0, 8.0, 0.0);
    c.keyframes = vec![kf(0.0, 1.0), kf(4.0, 5.0)];
    c.fade_in = 1.0;
    let t = one_lane(vec![c]);
    let s = t.slice(1.0, 8.0);
    let cut = &s.tracks[0].clips[0];
    assert_eq!(scales(cut), vec![(0.0, 2.0), (3.0, 5.0)]);
    assert_eq!((cut.timeline_start, cut.source_in, cut.fade_in), (0.0, 1.0, 0.0));
}

/// A tiny deterministic generator, so the fuzz below is the same on every run.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

/// A lane of `n` clips of random length, speed (and direction) and window in a
/// 600 s asset — the float values a real cut is made of, where `start + (out - in)
/// / speed` is not exact — laid end to end, with an occasional real gap.
fn random_lane(rng: &mut Lcg, n: usize) -> Timeline {
    let mut clips = Vec::new();
    let mut cursor = 0.0;
    for _ in 0..n {
        let mut c = sclip(0.0, 0.0, 0.0);
        c.speed = [1.0, 1.0, 2.0, 0.5, 1.5, -1.0, -2.0, 0.37][(rng.next() * 8.0) as usize % 8];
        c.source_in = rng.between(30.0, 400.0);
        c.source_out = c.source_in + rng.between(0.4, 7.0) * c.speed_mag();
        if rng.next() < 0.25 {
            cursor += rng.between(0.2, 2.0);
        }
        c.timeline_start = cursor;
        cursor = c.timeline_end();
        clips.push(c);
    }
    one_lane(clips)
}

/// Every junction where a clip starts *before* the one ahead of it ends, by float
/// noise — what a strict overlap test (`Project::move_clip`'s) reads as one clip
/// lying on the next — as `(index of the later clip, how far)`.
fn noise_overlaps(t: &Timeline) -> Vec<(usize, f64)> {
    let clips = &t.tracks[0].clips;
    (1..clips.len())
        .filter_map(|i| {
            let over = clips[i - 1].timeline_end() - clips[i].timeline_start;
            (over > 0.0 && over < ADJACENT_EPS).then_some((i, over))
        })
        .collect()
}

#[test]
fn roll_and_slide_leave_no_float_residue_between_clips() {
    // A roll or slide computes one side of a cut from a window and the other from
    // `start + delta`; left alone they disagree by a few ulps (±6e-14 s) in a large
    // share of cases, which a strict overlap test reads as clips lying on each other.
    // Every junction of the lane — the cut itself and the edge the edit runs into —
    // has to come out clean, for requests both inside the range and far past it.
    let mut rng = Lcg(7);
    let mut tried = 0;
    for _ in 0..3000 {
        let t = random_lane(&mut rng, 5);
        assert!(noise_overlaps(&t).is_empty());
        let ids: Vec<Uuid> = t.tracks[0].clips.iter().map(|c| c.id).collect();
        let at = 1 + (rng.next() * 3.0) as usize;
        let delta = if rng.next() < 0.3 {
            rng.between(-9.0, 9.0)
        } else {
            rng.between(-1.5, 1.5)
        };
        for op in 0..2 {
            let mut u = t.clone();
            let out = if op == 0 {
                u.roll_edit(ids[at - 1], ids[at], delta, &footage(600.0))
            } else {
                u.slide_clip(ids[at], delta, &footage(600.0))
            };
            let Ok(out) = out else { continue };
            tried += 1;
            assert!(noise_overlaps(&u).is_empty(), "op {op} by {delta}: {:?}", noise_overlaps(&u));
            // …and nothing but noise moved to get there.
            let (was, now) = (&t.tracks[0].clips, &u.tracks[0].clips);
            for (w, n) in was.iter().zip(now) {
                let edited = out.clips.iter().any(|c| c.id == w.id);
                if !edited {
                    assert_eq!(
                        (w.timeline_start, w.source_in, w.source_out),
                        (n.timeline_start, n.source_in, n.source_out)
                    );
                }
            }
            if op == 0 {
                near(now[at].timeline_start, was[at].timeline_start + out.applied);
                near(now[at].timeline_end(), was[at].timeline_end());
                near(now[at - 1].timeline_end(), was[at - 1].timeline_end() + out.applied);
            } else {
                near(now[at].timeline_start, was[at].timeline_start + out.applied);
                near(now[at].duration(), was[at].duration());
            }
        }
    }
    assert!(tried > 4000, "the fuzz should mostly be legal edits, got {tried}");
}

#[test]
fn a_cut_a_hair_apart_keeps_its_real_gap_and_only_float_noise_is_welded() {
    // 0.5 ms is data (within the engine's tolerance for a touching cut), not noise.
    let (mut t, a, b) = cut_pair();
    t.tracks[0].clips[1].timeline_start = 4.0005;
    t.roll_edit(a, b, 1.0, &footage(60.0)).unwrap();
    assert_eq!(t.tracks[0].clips[1].timeline_start, 5.0005);

    // Exactly closed: starting where the leader's computed end is, bit for bit.
    let (mut t, a, b) = cut_pair();
    t.roll_edit(a, b, 0.1, &footage(60.0)).unwrap();
    assert_eq!(t.tracks[0].clips[1].timeline_start, t.tracks[0].clips[0].timeline_end());
}

#[test]
fn a_slip_diff_has_the_precision_of_a_frame_and_the_sign_the_op_documents() {
    let slip = |mut c: Clip, delta: f64| {
        let id = c.id;
        c.timeline_start = 3.0;
        let before = one_lane(vec![c]);
        let mut after = before.clone();
        after.slip_clip(id, delta, &footage(60.0)).unwrap();
        before.diff(&after).entries[0].summary.clone()
    };
    // One frame at 30 fps is 0.033 s: one decimal printed it as `footage -0.0s
    // (in-point 10.0s → 10.0s)`, which says nothing happened.
    let frame = 1.0 / 30.0;
    let said = slip(sclip(10.0, 14.0, 0.0), frame);
    assert!(said.contains("footage +0.03s (in-point 10.00s → 10.03s)"), "{said}");
    let said = slip(sclip(10.0, 14.0, 0.0), -frame);
    assert!(said.contains("footage -0.03s (in-point 10.00s → 9.97s)"), "{said}");
    // Smaller than two decimals can show: three, rather than a zero.
    let said = slip(sclip(10.0, 14.0, 0.0), 0.001);
    assert!(said.contains("footage +0.001s"), "{said}");

    // A reversed clip's positive slip is the window moving *down*, and is still
    // the `+` the op documents (later in its own footage).
    let mut reversed = sclip(10.0, 14.0, 0.0);
    reversed.speed = -1.0;
    let said = slip(reversed.clone(), 3.0);
    assert!(said.contains("footage +3.00s (in-point 10.00s → 7.00s)"), "{said}");
    let said = slip(reversed, -3.0);
    assert!(said.contains("footage -3.00s (in-point 10.00s → 13.00s)"), "{said}");
}

#[test]
fn times_and_deltas_round_half_to_even_on_the_exact_value() {
    // The TS mirror (`formatTime`) has to agree with this to the digit: Rust rounds
    // an exact binary tie to the even digit and anything else by its exact value.
    assert_eq!(fmt_time(4.25), "0:04.2");
    assert_eq!(fmt_time(4.75), "0:04.8");
    assert_eq!(fmt_time(0.25), "0:00.2");
    assert_eq!(fmt_time(72.25), "1:12.2");
    assert_eq!(fmt_time(0.35), "0:00.3", "0.35 is a hair under a tie");
    assert_eq!(fmt_time(0.45), "0:00.5", "0.45 is a hair over one");
    assert_eq!(fmt_delta(-0.25), "-0.2s");
    assert_eq!(fmt_delta(0.75), "+0.8s");
    assert_eq!(format!("{:.2}", 0.125), "0.12");
    assert_eq!(format!("{:.2}", 0.375), "0.38");
}

/// V1 `v [0,6)` over A1 `a [1,7)` — a picture and its sound, not quite in step.
fn picture_and_sound() -> (Timeline, Uuid, Uuid) {
    let (v, a) = (sclip(10.0, 16.0, 0.0), sclip(10.0, 16.0, 1.0));
    let ids = (v.id, a.id);
    let t = Timeline {
        tracks: vec![
            track(StreamKind::Video, "V1", vec![v]),
            track(StreamKind::Audio, "A1", vec![a]),
        ],
        ..Timeline::new()
    };
    (t, ids.0, ids.1)
}

#[test]
fn a_group_split_remove_cuts_every_track_in_one_edit() {
    let (mut t, v, a) = picture_and_sound();
    let kept = t
        .split_remove_clips(
            &[ClipCut { clip_id: v, at: 3.0 }, ClipCut { clip_id: a, at: 3.0 }],
            SplitSide::Left,
        )
        .unwrap();
    assert_eq!(kept.iter().map(|c| c.id).collect::<Vec<_>>(), vec![v, a], "in request order");
    assert_eq!((kept[0].timeline_start, kept[0].source_in), (3.0, 13.0));
    assert_eq!((kept[1].timeline_start, kept[1].source_in), (3.0, 12.0));
    assert_eq!(spans_of(&t, 0), vec![(3.0, 6.0)]);
    assert_eq!(spans_of(&t, 1), vec![(3.0, 7.0)]);

    // Each track is cut at its own time, and the other side works the same way.
    let (mut t, v, a) = picture_and_sound();
    t.split_remove_clips(
        &[ClipCut { clip_id: v, at: 2.0 }, ClipCut { clip_id: a, at: 4.5 }],
        SplitSide::Right,
    )
    .unwrap();
    assert_eq!(spans_of(&t, 0), vec![(0.0, 2.0)]);
    assert_eq!(spans_of(&t, 1), vec![(1.0, 4.5)]);
}

#[test]
fn a_group_split_remove_is_all_or_nothing() {
    let (t, v, a) = picture_and_sound();
    let cut = |clip_id, at| ClipCut { clip_id, at };
    let run = |name: &str, cuts: &[ClipCut]| {
        let mut copy = t.clone();
        let why = refused(copy.split_remove_clips(cuts, SplitSide::Left));
        assert!(same_cut(&copy, &t), "{name}: a refused group changes nothing");
        why
    };
    // The second cut is bad, the first would have been fine.
    assert!(run("outside", &[cut(v, 3.0), cut(a, 0.5)]).contains("not inside the clip"));
    assert!(run("floor", &[cut(v, 3.0), cut(a, 6.99)]).contains("only 0.01s"));
    assert!(run("empty", &[]).contains("no clips"));
    assert!(run("twice", &[cut(v, 3.0), cut(v, 4.0)]).contains("more than once"));

    // One clip per track.
    let mut crowded = t.clone();
    let extra = sclip(0.0, 2.0, 7.0);
    let extra_id = extra.id;
    crowded.tracks[0].clips.push(extra);
    let mut copy = crowded.clone();
    let why = refused(copy.split_remove_clips(&[cut(v, 3.0), cut(extra_id, 7.5)], SplitSide::Right));
    assert!(why.contains("one clip per track") && why.contains("V1"), "{why}");
    assert!(same_cut(&copy, &crowded));

    let mut locked = t.clone();
    locked.tracks[1].locked = true;
    let mut copy = locked.clone();
    assert!(refused(copy.split_remove_clips(&[cut(v, 3.0), cut(a, 3.0)], SplitSide::Left)).contains("A1 is locked"));
    assert!(same_cut(&copy, &locked), "the unlocked track was not cut either");

    let mut copy = t.clone();
    assert!(matches!(
        copy.split_remove_clips(&[cut(v, 3.0), cut(Uuid::new_v4(), 3.0)], SplitSide::Left),
        Err(Error::ClipNotFound(_))
    ));
}

#[test]
fn a_group_split_remove_ripples_each_track_on_its_own() {
    // Behind each clip a follower: V1 `v [0,6) w [8,10)`, A1 `a [1,7) x [9,11)`.
    let (mut before, v, a) = picture_and_sound();
    before.tracks[0].clips.push(sclip(0.0, 2.0, 8.0));
    before.tracks[1].clips.push(sclip(0.0, 2.0, 9.0));
    let (after, ripple) = rippled(&before, |t| {
        t.split_remove_clips(
            &[ClipCut { clip_id: v, at: 2.0 }, ClipCut { clip_id: a, at: 5.0 }],
            SplitSide::Left,
        )
        .unwrap();
    });
    // Without ripple the gaps stay; with it each track closes by what *it* lost
    // (V1 2 s, A1 4 s) and the cut clips hold their own starts.
    assert_eq!(spans_of(&after, 0), vec![(2.0, 6.0), (8.0, 10.0)]);
    assert_eq!(spans_of(&ripple, 0), vec![(0.0, 4.0), (6.0, 8.0)]);
    assert_eq!(spans_of(&after, 1), vec![(5.0, 7.0), (9.0, 11.0)]);
    assert_eq!(spans_of(&ripple, 1), vec![(1.0, 3.0), (5.0, 7.0)]);
}

#[test]
fn a_slip_reads_as_a_slip_in_a_diff_not_a_zero_second_trim() {
    let (t, a, _) = cut_pair();
    let mut slipped = t.clone();
    slipped.slip_clip(a, 2.0, &footage(60.0)).unwrap();
    let diff = t.diff(&slipped);
    assert_eq!(diff.entries.len(), 1);
    assert_eq!(diff.entries[0].kind, DiffKind::ClipRetrimmed);
    assert!(
        diff.entries[0].summary.contains("Slipped clip on V1 at 0:00.0"),
        "{}",
        diff.entries[0].summary
    );
    assert!(diff.entries[0].summary.contains("+2.00s"), "{}", diff.entries[0].summary);
    assert!(
        diff.entries[0].summary.contains("in-point 10.00s → 12.00s"),
        "{}",
        diff.entries[0].summary
    );

    let mut rolled = t.clone();
    rolled.roll_edit(a, t.tracks[0].clips[1].id, 1.0, &footage(60.0)).unwrap();
    assert!(t.diff(&rolled).entries.iter().any(|e| e.summary.starts_with("Trimmed clip")));
}

// ---- animated SVG overlays ----------------------------------------------

fn svg_title(start: f64, end: f64) -> SvgOverlay {
    SvgOverlay {
        id: Uuid::new_v4(),
        name: "Logo".to_string(),
        svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"/>".to_string(),
        start,
        end,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
        opacity: 1.0,
        offset: 0.0,
        preset: None,
        template: None,
    }
}

fn preset_title(start: f64, end: f64) -> SvgOverlay {
    let mut o = svg_title(start, end);
    o.name = String::new();
    o.preset = Some(SvgPreset {
        kind: SvgPresetKind::FadeUp,
        params: SvgPresetParams::new("Hello"),
    });
    o.regenerate((1920, 1080));
    o
}

#[test]
fn a_timeline_without_svg_overlays_serialises_as_it_always_did() {
    let json = serde_json::to_string(&Timeline::new()).unwrap();
    assert!(!json.contains("svg_overlays"), "{json}");
    let old = r#"{"tracks":[]}"#;
    let tl: Timeline = serde_json::from_str(old).unwrap();
    assert!(tl.svg_overlays.is_empty());
    assert_eq!(
        serde_json::to_string(&tl).unwrap(),
        old,
        "an old project round-trips byte for byte"
    );
    let mut with = Timeline::new();
    with.svg_overlays.push(svg_title(0.0, 2.0));
    let back: Timeline = serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
    assert_eq!(back.svg_overlays, with.svg_overlays);
}

#[test]
fn a_template_instance_round_trips_in_snake_case_and_is_absent_when_there_is_none() {
    use crate::template::{TemplateControl, TemplateInstance, TemplateKind, TemplateTarget};
    let mut o = svg_title(0.0, 2.0);
    let plain = serde_json::to_string(&o).unwrap();
    assert!(
        !plain.contains("template"),
        "a title without one stores the bytes it always did: {plain}"
    );
    o.template = Some(TemplateInstance {
        template_id: "6f1e0c0e-3b0f-4a5b-9c5e-0a8f2f6d7a11".into(),
        name: "Kitefun end card".into(),
        controls: vec![TemplateControl {
            id: "brand".into(),
            label: "Brand colour".into(),
            kind: TemplateKind::Color,
            targets: vec![TemplateTarget {
                element: "bg-stop".into(),
                attribute: Some("stop-color".into()),
            }],
            min: None,
            max: None,
        }],
        values: [("brand".to_string(), "#112233".to_string())].into(),
    });
    let json = serde_json::to_value(&o).unwrap();
    assert_eq!(
        json["template"],
        serde_json::json!({
            "template_id": "6f1e0c0e-3b0f-4a5b-9c5e-0a8f2f6d7a11",
            "name": "Kitefun end card",
            "controls": [{
                "id": "brand", "label": "Brand colour", "kind": "color",
                "targets": [{"element": "bg-stop", "attribute": "stop-color"}]
            }],
            "values": {"brand": "#112233"}
        })
    );
    let back: SvgOverlay = serde_json::from_value(json).unwrap();
    assert_eq!(back, o);
}

#[test]
fn svg_overlay_json_defaults_and_omissions() {
    let id = Uuid::new_v4();
    let minimal = format!(r#"{{"id":"{id}","svg":"<svg/>","start":1.0,"end":2.5}}"#);
    let o: SvgOverlay = serde_json::from_str(&minimal).unwrap();
    assert_eq!(
        (o.x, o.y, o.width, o.height, o.opacity, o.offset, o.name.as_str()),
        (0.0, 0.0, 1.0, 1.0, 1.0, 0.0, "")
    );
    assert!(o.preset.is_none() && o.template.is_none());
    let json = serde_json::to_value(&o).unwrap();
    assert!(
        json.get("offset").is_none() && json.get("preset").is_none() && json.get("template").is_none(),
        "{json}"
    );
    assert_eq!(json["width"], 1.0);
    let sliced = SvgOverlay { offset: 1.5, ..o };
    assert_eq!(serde_json::to_value(&sliced).unwrap()["offset"], 1.5);

    let p: SvgPreset = serde_json::from_str(r#"{"kind":"lower_third","params":{"text":"Jane"}}"#).unwrap();
    assert_eq!(p.kind, SvgPresetKind::LowerThird);
    assert_eq!(
        (
            p.params.color.as_str(),
            p.params.accent.as_str(),
            p.params.subtitle.clone(),
            p.params.font.clone()
        ),
        ("#ffffff", "#ffb000", None, None)
    );
    for kind in SvgPresetKind::ALL {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
    }
    let patch: SvgOverlayPatch = serde_json::from_str(r#"{"opacity":0.5}"#).unwrap();
    assert_eq!(
        patch,
        SvgOverlayPatch {
            opacity: Some(0.5),
            ..Default::default()
        }
    );
    assert_eq!(serde_json::to_string(&SvgOverlayPatch::default()).unwrap(), "{}");
    let kind: SvgOverlayPatch = serde_json::from_str(r#"{"preset_kind":"word_pop"}"#).unwrap();
    assert_eq!(kind.preset_kind, Some(SvgPresetKind::WordPop));
    assert_eq!(serde_json::to_string(&kind).unwrap(), r#"{"preset_kind":"word_pop"}"#);
}

#[test]
fn svg_overlay_clock_and_names() {
    let mut o = svg_title(2.0, 6.0);
    assert_eq!(
        (o.doc_time(1.0), o.doc_time(2.0), o.doc_time(3.5), o.doc_duration()),
        (0.0, 0.0, 1.5, 4.0)
    );
    o.offset = 1.0;
    assert_eq!((o.doc_time(3.5), o.doc_duration()), (2.5, 5.0));
    assert_eq!(o.display_name(), "Logo");
    assert_eq!(preset_title(0.0, 3.0).display_name(), "Hello");
    let mut anon = svg_title(0.0, 1.0);
    anon.name = "  ".to_string();
    assert_eq!(anon.display_name(), "SVG");
}

#[test]
fn slice_clips_and_shifts_svg_overlays_and_keeps_their_clock() {
    let mut tl = Timeline::new();
    tl.svg_overlays = vec![
        svg_title(2.0, 8.0),
        svg_title(0.0, 3.0),
        svg_title(9.0, 12.0),
        svg_title(5.0, 6.0),
    ];
    let ids: Vec<Uuid> = tl.svg_overlays.iter().map(|o| o.id).collect();
    let s = tl.slice(4.0, 10.0);
    let got: Vec<(Uuid, f64, f64, f64)> = s.svg_overlays.iter().map(|o| (o.id, o.start, o.end, o.offset)).collect();
    assert_eq!(
        got,
        vec![(ids[0], 0.0, 4.0, 2.0), (ids[2], 5.0, 6.0, 0.0), (ids[3], 1.0, 2.0, 0.0),],
        "cut at the front, clipped at the back, shifted; one wholly before is gone"
    );
    assert!(tl.slice(20.0, 30.0).svg_overlays.is_empty());
    assert!(tl.slice(0.0, 2.0).svg_overlays.iter().all(|o| o.offset == 0.0));
    // slicing a slice keeps counting from the document's start
    let again = s.slice(1.0, 3.0);
    let first = &again.svg_overlays[0];
    assert_eq!((first.id, first.start, first.end, first.offset), (ids[0], 0.0, 2.0, 3.0));
    assert_eq!(
        first.doc_time(first.start),
        3.0,
        "the animation is already 3 s in where the cut begins"
    );
    // the whole span keeps its offset untouched
    let whole = tl.slice(0.0, 100.0);
    assert!(whole.svg_overlays.iter().all(|o| o.offset == 0.0));
    assert_eq!(whole.svg_overlays.len(), 4);
}

#[test]
fn for_render_keeps_svg_overlays_and_ripple_never_moves_them() {
    let (mut before, [a, ..]) = gapped();
    before.svg_overlays.push(svg_title(6.0, 9.0));
    assert_eq!(before.for_render().svg_overlays, before.svg_overlays);
    let (_, out) = rippled(&before, |t| clip_mut(t, a).source_out = 3.0);
    assert_eq!((out.svg_overlays[0].start, out.svg_overlays[0].end), (6.0, 9.0));
    assert_eq!(spans_of(&out, 0)[1], (4.0, 7.0), "while the clips did move");
}

#[test]
fn for_delivery_remakes_presets_for_a_new_shape_and_leaves_custom_art_alone() {
    let mut tl = Timeline::new();
    let custom = svg_title(0.0, 4.0);
    let preset = preset_title(0.0, 4.0);
    tl.svg_overlays = vec![custom.clone(), preset.clone()];
    let vertical = tl.for_delivery(Delivery::new(1080, 1920, Fit::Cover));
    assert_eq!(vertical.svg_overlays[0], custom, "custom artwork is only ever scaled");
    assert!(vertical.svg_overlays[1].svg.contains("viewBox=\"0 0 1080 1920\""));
    assert_ne!(vertical.svg_overlays[1].svg, preset.svg);
    assert_eq!(vertical.svg_overlays[1].preset, preset.preset);
    assert_eq!(tl.svg_overlays[1], preset, "the cut itself is not touched");

    tl.format = Some(Delivery::new(1920, 1080, Fit::Contain));
    let same = tl.for_delivery(Delivery::new(1280, 720, Fit::Contain));
    assert_eq!(
        same.svg_overlays, tl.svg_overlays,
        "the same shape at another size changes only the size"
    );
    let square = tl.for_delivery(Delivery::new(1080, 1080, Fit::Cover));
    assert!(square.svg_overlays[1].svg.contains("viewBox=\"0 0 1080 1080\""));

    let mut cut = Timeline::new();
    let mut sliced = preset_title(0.0, 4.0);
    sliced.offset = 1.0;
    cut.svg_overlays = vec![sliced];
    let remade = cut.for_delivery(Delivery::new(1080, 1920, Fit::Cover)).svg_overlays.remove(0);
    assert!(
        remade.svg.contains("begin=\"4.6s\""),
        "the preset is re-made for its whole document: {}",
        remade.svg
    );
}

#[test]
fn diff_names_animated_titles_added_changed_and_removed() {
    let mut before = Timeline::new();
    let kept = svg_title(0.0, 2.0);
    let gone = preset_title(5.0, 7.0);
    before.svg_overlays = vec![kept.clone(), gone.clone()];

    let mut after = before.clone();
    let mut moved = kept.clone();
    moved.start = 0.5;
    moved.x = 0.25;
    moved.opacity = 0.5;
    let fresh = svg_title(1.0, 5.0);
    after.svg_overlays = vec![moved, fresh.clone()];

    let diff = before.diff(&after);
    let by = |kind: DiffKind| diff.entries.iter().find(|e| e.kind == kind).unwrap();
    let added = by(DiffKind::OverlayAdded);
    assert_eq!(added.summary, "Added animated title “Logo” at 0:01.0–0:05.0");
    assert_eq!((added.overlay_id, added.at), (Some(fresh.id), Some(1.0)));
    let changed = by(DiffKind::OverlayChanged);
    assert_eq!(changed.summary, "Changed animated title “Logo”");
    assert_eq!(changed.overlay_id, Some(kept.id));
    let detail = changed.detail.as_deref().unwrap();
    assert!(detail.contains("timing 0:00.0–0:02.0 → 0:00.5–0:02.0"), "{detail}");
    assert!(
        detail.contains("placement 0.00,0.00 1.00x1.00 → 0.25,0.00 1.00x1.00"),
        "{detail}"
    );
    assert!(detail.contains("opacity 100% → 50%"), "{detail}");
    let removed = by(DiffKind::OverlayRemoved);
    assert_eq!(removed.summary, "Removed animated title “Hello” at 0:05.0");
    assert_eq!(removed.overlay_id, Some(gone.id));
    assert_eq!(diff.entries.len(), 3);
    assert!(before.diff(&before).is_empty());
}

#[test]
fn diff_reads_a_preset_retime_as_timing_and_a_conversion_as_a_conversion() {
    let mut before = Timeline::new();
    let p = preset_title(0.0, 4.0);
    before.svg_overlays = vec![p];
    let mut after = before.clone();
    after.svg_overlays[0].end = 8.0;
    after.svg_overlays[0].regenerate((1920, 1080));
    let detail = before.diff(&after).entries[0].detail.clone().unwrap();
    assert_eq!(
        detail, "timing 0:00.0–0:04.0 → 0:00.0–0:08.0",
        "the re-made artwork is not a separate change"
    );

    let mut reworded = before.clone();
    reworded.svg_overlays[0].preset.as_mut().unwrap().params.text = "Bye".to_string();
    let detail = before.diff(&reworded).entries[0].detail.clone().unwrap();
    assert_eq!(detail, "text “Hello” → “Bye”");

    let mut custom = before.clone();
    custom.svg_overlays[0].preset = None;
    assert_eq!(
        before.diff(&custom).entries[0].detail.as_deref(),
        Some("converted to custom SVG")
    );

    let mut edited = Timeline::new();
    edited.svg_overlays = vec![svg_title(0.0, 1.0)];
    let mut after = edited.clone();
    after.svg_overlays[0].svg.push(' ');
    assert_eq!(edited.diff(&after).entries[0].detail.as_deref(), Some("artwork changed"));
}

#[test]
fn a_timeline_knows_whether_a_clip_comes_in_through_a_dip_to_white() {
    let dip = |kind: TransitionKind| {
        let mut c = Clip::new(Uuid::new_v4(), 0.0, 1.0, 1.0);
        c.transition_in = Some(Transition { kind, duration: 0.4 });
        c
    };
    let track = |clips: Vec<Clip>, muted: bool| Track {
        clips,
        muted,
        ..Track::new(StreamKind::Video, "V")
    };
    let timeline = |tracks: Vec<Track>| Timeline {
        tracks,
        ..Timeline::new()
    };
    let plain = Clip::new(Uuid::new_v4(), 0.0, 1.0, 0.0);
    assert!(!Timeline::new().has_white_dip());
    assert!(!timeline(vec![track(vec![plain.clone()], false)]).has_white_dip());
    // Other transitions are not the dip to white (a dip to black is a fade in the YUV).
    for kind in [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::SlideLeft,
    ] {
        assert!(
            !timeline(vec![track(vec![plain.clone(), dip(kind)], false)]).has_white_dip(),
            "{kind:?}"
        );
    }
    assert!(timeline(vec![track(vec![plain.clone(), dip(TransitionKind::DipToWhite)], false)]).has_white_dip());
    // The export's graph has the clips `for_render` keeps: a muted track's, a track another one's solo shadows and a disabled clip's are
    // not in it, so they are no reason to ask for the conversions.
    assert!(!timeline(vec![
        track(vec![plain.clone()], false),
        track(vec![dip(TransitionKind::DipToWhite)], true),
    ])
    .has_white_dip());
    let mut soloed = track(vec![plain.clone()], false);
    soloed.solo = true;
    assert!(!timeline(vec![soloed.clone(), track(vec![dip(TransitionKind::DipToWhite)], false)]).has_white_dip());
    assert!(timeline(vec![soloed, {
        let mut shown = track(vec![dip(TransitionKind::DipToWhite)], false);
        shown.solo = true;
        shown
    }])
    .has_white_dip());
    let mut off = dip(TransitionKind::DipToWhite);
    off.enabled = false;
    assert!(!timeline(vec![track(vec![plain.clone(), off], false)]).has_white_dip());
    // (A track that renders and has the dip does.)
    assert!(timeline(vec![
        track(vec![plain], false),
        track(vec![dip(TransitionKind::DipToWhite)], false),
    ])
    .has_white_dip());
}
