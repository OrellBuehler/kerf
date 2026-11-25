#![allow(dead_code)]

//! The planes a 10-bit export hands its encoder: the export's own argv (`kerf_core::export_args`), its encoder swapped for the raw
//! pipe, so the reference is the graph's terminal frames, not a file an encoder has had its way with (x264's `-crf 0` is not lossless at
//! 10 bits, and ProRes never is).

use std::process::{Command, Stdio};

use kerf_core::{Asset, ExportOptions, Timeline};

/// FFmpeg's `-filter_complex_threads` for the run; `None` leaves the default.
pub fn export_planes(
    tl: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    threads: Option<u32>,
    frames: Option<u32>,
) -> Vec<u8> {
    let args = kerf_core::export_args(tl, assets, "OUT", opts).expect("the export's argv");
    let mut run: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into(), "-nostdin".into()];
    if let Some(n) = threads {
        run.extend(["-filter_complex_threads".into(), n.to_string()]);
    }
    let mut it = args.into_iter().peekable();
    let mut sink = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-hide_banner" | "-nostats" => {}
            "-c:v" | "-crf" => {
                it.next();
            }
            _ if it.peek().is_none() => sink = Some(arg),
            _ => run.push(arg),
        }
    }
    assert_eq!(sink.as_deref(), Some("OUT"), "the export's sink");
    if let Some(n) = frames {
        run.extend(["-frames:v".into(), n.to_string()]);
    }
    run.extend(["-f".into(), "rawvideo".into(), "pipe:1".into()]);
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(&run)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {run:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Little-endian 16-bit samples.
pub fn samples(bytes: &[u8]) -> Vec<u16> {
    bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
}

/// The frames of the export `keep` says yes to, each `frame_bytes` long (the delivery's planes at 16 bits), read as the child writes them so that
/// a long 1080p export is never held whole; the child stops after frame `upto`.
pub fn export_frames(
    tl: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    threads: Option<u32>,
    upto: u64,
    frame_bytes: usize,
    keep: &dyn Fn(u64) -> bool,
) -> std::collections::BTreeMap<u64, Vec<u8>> {
    use std::io::Read;
    let args = kerf_core::export_args(tl, assets, "OUT", opts).expect("the export's argv");
    let mut run: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into(), "-nostdin".into()];
    if let Some(n) = threads {
        run.extend(["-filter_complex_threads".into(), n.to_string()]);
    }
    let mut it = args.into_iter().peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-hide_banner" | "-nostats" => {}
            "-c:v" | "-crf" => {
                it.next();
            }
            _ if it.peek().is_none() => {}
            _ => run.push(arg),
        }
    }
    run.extend([
        "-frames:v".into(),
        (upto + 1).to_string(),
        "-f".into(),
        "rawvideo".into(),
        "pipe:1".into(),
    ]);
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(&run)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stderr = child.stderr.take().expect("piped");
    let log = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let mut stdout = child.stdout.take().expect("piped");
    let mut out = std::collections::BTreeMap::new();
    let mut k = 0u64;
    loop {
        let mut frame = vec![0u8; frame_bytes];
        match stdout.read_exact(&mut frame) {
            Ok(()) => {
                if keep(k) {
                    out.insert(k, frame);
                }
                k += 1;
            }
            Err(_) => break,
        }
    }
    let status = child.wait().expect("wait for ffmpeg");
    let log = log.join().unwrap_or_default();
    assert!(status.success(), "ffmpeg {run:?}: {log}");
    out
}
