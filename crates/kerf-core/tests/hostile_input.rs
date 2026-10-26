//! Hostile input for what Kerf reads from files: caption files (SRT, ASS / SSA, in any encoding) and
//! the stored timeline JSON. Seeded mutations (`kerf_core::hostile`) under `catch_unwind`: an
//! unreadable file is an error or a skipped cue, never a panic.

use std::cell::Cell;

use kerf_core::hostile::assert_no_panic;
use kerf_core::{
    decode_caption_bytes, parse_ass, parse_captions, parse_srt, CaptionFormat, CaptionImportRequest, Project, RampEasing,
    SpeedKey, Timeline, MAX_RAMP_TIME, MAX_SPEED, MIN_SPEED,
};

const ROUNDS: usize = 1500;

const SRT: &str =
    "\u{feff}1\r\n00:00:01,000 --> 00:00:02,500\r\n<i>Hello</i> {\\an8}<font color=\"#ff0000\">world</font>\r\n\r\n\
2\n00:00:03.000 --> 00:00:04.000 X1:10 Y1:20\nsecond\nline\n\n\
99\n01:02:03,4 --> 01:02:05,45\nlast &amp; <b>bold</b>\n";

const ASS: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,48\n\n\
[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
Dialogue: 0,0:00:01.00,0:00:02.50,Default,,0,0,0,,{\\i1}Hello{\\i0}\\Nworld, again\\h!\n\
Comment: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,note\n\
Dialogue: 1,1:02:03.45,1:02:05.00,Default,,0,0,0,,{\\p1}m 0 0 l 10 10{\\p0}last\n";

const SSA: &str = "[Events]\nFormat: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
Dialogue: Marked=0,0:00:00.50,0:00:01.00,*Default,NTP,0000,0000,0000,!Effect,old style\n";

fn utf16le(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    out
}

fn read_captions(bytes: &[u8]) {
    for text in [
        decode_caption_bytes(bytes).ok(),
        Some(String::from_utf8_lossy(bytes).into_owned()),
    ]
    .into_iter()
    .flatten()
    {
        let _ = parse_srt(&text);
        let _ = parse_ass(&text);
        let _ = CaptionFormat::detect(&text);
        for format in [None, Some(CaptionFormat::Srt), Some(CaptionFormat::Ass)] {
            let _ = parse_captions(&text, format);
        }
    }
}

#[test]
fn hostile_caption_files_never_panic() {
    let wide = utf16le(SRT);
    assert_no_panic(
        "caption files",
        &[SRT.as_bytes(), ASS.as_bytes(), SSA.as_bytes(), &wide],
        ROUNDS,
        read_captions,
    );
}

/// What survives parsing is put on a cut: placing hostile cues (absurd times, empty or huge text)
/// is the import's to refuse, not to panic on. One project for the run; most cases do not parse.
#[test]
fn hostile_captions_import_onto_the_cut_without_panicking() {
    let project = Project::sample().unwrap();
    let imported = Cell::new(0);
    assert_no_panic("caption import", &[SRT.as_bytes(), ASS.as_bytes()], 300, |bytes| {
        let Ok(file) = parse_captions(&String::from_utf8_lossy(bytes), None) else {
            return;
        };
        let _ = project.import_captions(&file, CaptionImportRequest::default());
        let _ = project.import_captions(
            &file,
            CaptionImportRequest {
                offset: -3600.0,
                ..Default::default()
            },
        );
        imported.set(imported.get() + 1);
    });
    assert!(imported.get() > 0);
}

fn read_timeline(base: &Timeline, bytes: &[u8]) {
    let Ok(t) = serde_json::from_slice::<Timeline>(bytes) else {
        return;
    };
    let _ = t.duration();
    let _ = t.for_render().duration();
    let _ = t.export_span(None);
    let _ = t.export_span(Some((0.0, 1.0)));
    let _ = t.diff(base);
    let _ = base.diff(&t);
    let _ = t.ripple_from(base);
    let _ = t.clips_moved_since(base);
    let _ = t.slice(0.0, t.duration() / 2.0);
    let _ = serde_json::to_string(&t);
}

/// The timeline is stored as one JSON blob, written by this build, an older one or an agent's
/// staged copy, and read back on every open; a damaged blob is a load error, not a panic.
#[test]
fn hostile_stored_timelines_never_panic() {
    let mut base = Project::sample().unwrap().timeline().unwrap();
    // A speed ramp in the file, so the mutations land on its keys too.
    base.tracks[0].clips[0].speed_ramp = vec![
        SpeedKey::new(0.0, 1.0),
        SpeedKey {
            time: 1.5,
            speed: 0.25,
            easing: RampEasing::Hold,
        },
        SpeedKey::new(2.5, 2.0),
    ];
    // And stabilized, so they land on its setting.
    base.tracks[0].clips[0].stabilize = Some(kerf_core::Stabilize {
        strength: 0.5,
        zoom: Some(6.0),
    });
    let json = serde_json::to_vec(&base).unwrap();
    assert_no_panic("stored timeline", &[&json], ROUNDS, |b| read_timeline(&base, b));
}

/// A ramp somebody wrote by hand: keys out of order, twice at one time, off to the edge of the number line,
/// speeds of zero or ten thousand, a thousand of them. The map must never panic or hand back a number that is
/// not one: a ramp it cannot read is ignored (the clip plays at its constant speed), one it can read is finite
/// everywhere, and the invariant net flags the unreadable ones so no edit introduces one.
#[test]
fn hostile_speed_ramps_never_panic_or_produce_a_nan() {
    let key = |time: f64, speed: f64| SpeedKey::new(time, speed);
    let many = |n: usize| (0..n).map(|i| key(i as f64, 1.0 + (i % 7) as f64)).collect::<Vec<_>>();
    let cases: Vec<(&str, Vec<SpeedKey>)> = vec![
        ("nan time", vec![key(f64::NAN, 1.0)]),
        ("inf time", vec![key(0.0, 1.0), key(f64::INFINITY, 2.0)]),
        ("nan speed", vec![key(0.0, f64::NAN)]),
        ("inf speed", vec![key(0.0, f64::INFINITY)]),
        ("zero speed", vec![key(0.0, 0.0)]),
        ("negative speed", vec![key(0.0, -3.0), key(1.0, 2.0)]),
        ("huge speed", vec![key(0.0, 1e300)]),
        ("tiny speed", vec![key(0.0, 1e-300)]),
        ("out of order", vec![key(5.0, 1.0), key(1.0, 2.0), key(3.0, 3.0)]),
        ("twice at a time", vec![key(1.0, 1.0), key(1.0, 5.0)]),
        ("huge times", vec![key(0.0, 1.0), key(1e300, 100.0)]),
        (
            "the edge of the cap",
            vec![key(-MAX_RAMP_TIME, MIN_SPEED), key(MAX_RAMP_TIME, MAX_SPEED)],
        ),
        (
            "past the cap",
            vec![key(-MAX_RAMP_TIME * 2.0, MIN_SPEED), key(MAX_RAMP_TIME * 2.0, MAX_SPEED)],
        ),
        (
            "keys a hair apart",
            vec![key(1.0, MIN_SPEED), key(1.0 + f64::EPSILON, MAX_SPEED), key(2.0, MIN_SPEED)],
        ),
        ("keys a denormal apart", vec![key(0.0, MIN_SPEED), key(5e-324, MAX_SPEED)]),
        ("one key", vec![key(3.0, 2.0)]),
        ("too many", many(17)),
        ("a thousand", many(1000)),
        ("the most", many(16)),
        (
            "all holds",
            (0..16)
                .map(|i| SpeedKey {
                    time: i as f64 * 0.25,
                    speed: MAX_SPEED,
                    easing: RampEasing::Hold,
                })
                .collect(),
        ),
    ];
    let base = Project::sample().unwrap().timeline().unwrap();
    for (what, keys) in cases {
        for speed in [1.0, -1.0, 0.01, 100.0, f64::NAN] {
            let mut t = base.clone();
            {
                let c = &mut t.tracks[0].clips[0];
                c.speed_ramp = keys.clone();
                c.speed = speed;
            }
            let c = t.tracks[0].clips[0].clone();
            let label = format!("{what} at speed {speed}");
            if speed.is_finite() {
                for x in [-1e9, -3.0, -0.5, 0.0, 0.25, 1.0, 7.5, 1e3, 1e9] {
                    assert!(c.along(x).is_finite(), "{label}: along({x})");
                    assert!(c.local_at_along(x).is_finite(), "{label}: local_at_along({x})");
                    assert!(c.speed_at(x).is_finite(), "{label}: speed_at({x})");
                    assert!(c.timeline_to_source(x).is_finite(), "{label}: timeline_to_source({x})");
                    assert!(c.source_to_timeline(x).is_finite(), "{label}: source_to_timeline({x})");
                }
                assert!(c.duration().is_finite() && c.duration() >= 0.0, "{label}: duration");
                assert!(c.mean_speed().is_finite(), "{label}: mean_speed");
            }
            // Whatever it is, the engine's reads of the cut do not panic.
            let _ = t.duration();
            let _ = t.slice(0.0, t.duration() / 2.0);
            let _ = t.slice(0.1, 0.2);
            let _ = t.diff(&base);
            let _ = base.diff(&t);
            let _ = t.for_render().duration();
            if !keys.is_empty() && c.ramp().is_none() {
                assert!(
                    t.check().is_err(),
                    "{label}: an unreadable ramp must be flagged by the invariants"
                );
            }
            let mut edited = t.clone();
            let _ = edited.split_clip(c.id, c.timeline_start + c.duration() / 2.0);
            let mut edited = t.clone();
            let _ = edited.set_speed_ramp(c.id, keys.clone());
            if let Ok(json) = serde_json::to_string(&t) {
                let _ = serde_json::from_str::<Timeline>(&json);
            }
        }
    }
}

/// A `.kerftemplate` arrives by email: its zip, its manifest and the values a surface hands the splice
/// are all read or applied as hostile. An unreadable one is an error, never a panic.
#[test]
fn hostile_templates_never_panic() {
    use std::collections::BTreeMap;

    use kerf_core::{
        template::{apply_values, ManifestFont, FORMAT, VERSION},
        SvgDoc, Template, TemplateControl, TemplateKind, TemplateManifest, TemplateTarget,
    };

    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 200 100">
<rect id="bg" width="200" height="100" fill="#102030"/>
<text id="tag" x="10" y="60" font-family="Geist">Hello</text>
<image id="logo" xlink:href="data:image/png;base64,AAAA" width="4" height="4"/>
</svg>"##;
    let control = |id: &str, kind, element: &str, attribute: Option<&str>| TemplateControl {
        id: id.into(),
        label: id.into(),
        kind,
        targets: vec![TemplateTarget {
            element: element.into(),
            attribute: attribute.map(str::to_string),
        }],
        min: (kind == TemplateKind::Number).then_some(0.0),
        max: (kind == TemplateKind::Number).then_some(1000.0),
    };
    let manifest = TemplateManifest {
        format: FORMAT.into(),
        version: VERSION,
        id: "6f1e0c0e-3b0f-4a5b-9c5e-0a8f2f6d7a11".into(),
        name: "Card".into(),
        description: String::new(),
        duration: 4.0,
        width: 200,
        height: 100,
        controls: vec![
            control("tagline", TemplateKind::Text, "tag", None),
            control("brand", TemplateKind::Color, "bg", Some("fill")),
            control("w", TemplateKind::Number, "bg", Some("width")),
            control("face", TemplateKind::Font, "tag", Some("font-family")),
            control("logo", TemplateKind::Image, "logo", Some("xlink:href")),
        ],
        fonts: Vec::<ManifestFont>::new(),
    };
    let template = Template {
        manifest: manifest.clone(),
        svg: svg.into(),
        fonts: Vec::new(),
        poster: None,
    };
    let zip = template.to_bytes().unwrap();
    let json = serde_json::to_vec(&manifest).unwrap();

    // The zip: mutated bytes mostly break the compressed streams, which is the zip reader's to refuse.
    assert_no_panic("template zip", &[&zip], 600, |bytes| {
        if let Ok(mut t) = Template::read(bytes) {
            let _ = t.validate();
            let _ = t.prepare(&BTreeMap::new());
        }
    });
    // The manifest, which a mutated zip rarely reaches: whatever parses is written, read and checked.
    assert_no_panic("template manifest", &[&json], ROUNDS, |bytes| {
        let Ok(manifest) = serde_json::from_slice::<TemplateManifest>(bytes) else {
            return;
        };
        let t = Template {
            manifest,
            svg: svg.into(),
            fonts: Vec::new(),
            poster: None,
        };
        if let Ok(written) = t.to_bytes() {
            if let Ok(mut back) = Template::read(&written) {
                let _ = back.validate();
            }
        }
    });
    // The values: any text at all goes through the splice and the sanitiser behind it.
    let values = |text: &str| -> BTreeMap<String, String> {
        ["tagline", "brand", "w", "face", "logo"]
            .into_iter()
            .map(|id| (id.to_string(), text.to_string()))
            .collect()
    };
    assert_no_panic(
        "template values",
        &[
            b"Hello",
            b"#ff00ff",
            b"12.5",
            b"Barlow Condensed",
            b"data:image/png;base64,AAAA",
            "<&>\"'\u{e9}\n".as_bytes(),
        ],
        ROUNDS,
        |bytes| {
            let text = String::from_utf8_lossy(bytes);
            for control in &manifest.controls {
                let one: BTreeMap<String, String> = [(control.id.clone(), text.to_string())].into();
                if let Ok(out) = apply_values(svg, &manifest.controls, &one) {
                    let _ = SvgDoc::parse(&out);
                }
            }
            let _ = apply_values(svg, &manifest.controls, &values(&text));
        },
    );
}
