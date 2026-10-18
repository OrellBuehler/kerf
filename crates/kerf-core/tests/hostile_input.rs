//! Hostile input for what Kerf reads from files: caption files (SRT, ASS / SSA, in any encoding) and
//! the stored timeline JSON. Seeded mutations (`kerf_core::hostile`) under `catch_unwind`: an
//! unreadable file is an error or a skipped cue, never a panic.

use std::cell::Cell;

use kerf_core::hostile::assert_no_panic;
use kerf_core::{
    decode_caption_bytes, parse_ass, parse_captions, parse_srt, CaptionFormat, CaptionImportRequest, Project, Timeline,
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
        let _ = project.import_captions(&file, CaptionImportRequest { offset: -3600.0, ..Default::default() });
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
    let base = Project::sample().unwrap().timeline().unwrap();
    let json = serde_json::to_vec(&base).unwrap();
    assert_no_panic("stored timeline", &[&json], ROUNDS, |b| read_timeline(&base, b));
}
