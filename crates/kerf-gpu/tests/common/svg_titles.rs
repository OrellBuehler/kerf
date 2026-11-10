//! Floating animated SVG titles the parity suites share (`parity.rs` for a still, `motion_parity.rs` for an export).

/// A custom title's artwork: a gradient with translucent stops, soft circles, a hairline and a stroked box, so that every alpha and
/// both chroma phases are in the picture.
pub const TITLE_ART: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 160 90">
<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#ff3355" stop-opacity="0.9"/><stop offset="1" stop-color="#3355ff" stop-opacity="0.15"/></linearGradient></defs>
<rect x="6" y="6" width="148" height="78" rx="9" fill="url(#g)"/>
<circle cx="50" cy="45" r="22" fill="#ffd000" fill-opacity="0.55"/>
<path d="M10 80 L150 12" stroke="#ffffff" stroke-width="1.3" stroke-opacity="0.8" fill="none"/>
<rect x="100" y="52" width="40" height="22" fill="#00e0a0" fill-opacity="0.35" stroke="#ffffff" stroke-width="0.7"/>
</svg>"##;

/// A title that moves: a bar that slides, a disc that fades, and a hairline that grows, over two seconds.
pub const MOVING_ART: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 160 90">
<rect id="bar" x="0" y="30" width="46" height="30" fill="#ff3355"><animate attributeName="x" from="0" to="114" dur="2s" fill="freeze"/></rect>
<circle cx="120" cy="20" r="14" fill="#33d0ff"><animate attributeName="fill-opacity" values="0;1;0.3" dur="2s" fill="freeze"/></circle>
<rect x="8" y="76" width="4" height="3" fill="#ffffff"><animate attributeName="width" from="4" to="140" dur="2s" fill="freeze"/></rect>
</svg>"##;

/// A title showing `svg` from `start` to `end` in the rectangle `at` (`x`, `y`, `width`, `height`: fractions of the frame).
pub fn title(svg: &str, start: f64, end: f64, at: [f64; 4], opacity: f64) -> kerf_core::SvgOverlay {
    kerf_core::SvgOverlay {
        id: uuid::Uuid::new_v4(),
        name: String::new(),
        svg: svg.to_string(),
        start,
        end,
        x: at[0],
        y: at[1],
        width: at[2],
        height: at[3],
        opacity,
        offset: 0.0,
        preset: None,
        template: None,
    }
}

/// The fractions of a `frame`-sized picture whose rectangle lands on exactly `x, y, w x h` pixels.
pub fn pixels(frame: (u32, u32), x: i32, y: i32, w: u32, h: u32) -> [f64; 4] {
    let (fw, fh) = (f64::from(frame.0), f64::from(frame.1));
    [f64::from(x) / fw, f64::from(y) / fh, f64::from(w) / fw, f64::from(h) / fh]
}

/// A preset as the app makes it: the artwork for the title's span, drawn for `frame`.
pub fn preset(
    kind: kerf_core::SvgPresetKind,
    text: &str,
    subtitle: Option<&str>,
    (start, end): (f64, f64),
    at: [f64; 4],
    frame: (u32, u32),
) -> kerf_core::SvgOverlay {
    let params = kerf_core::SvgPresetParams {
        subtitle: subtitle.map(String::from),
        ..kerf_core::SvgPresetParams::new(text)
    };
    kerf_core::SvgOverlay {
        svg: kerf_core::render_preset(kind, &params, end - start, frame),
        preset: Some(kerf_core::SvgPreset { kind, params }),
        ..title("", start, end, at, 1.0)
    }
}
