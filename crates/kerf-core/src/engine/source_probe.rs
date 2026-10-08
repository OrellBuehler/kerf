//! Per-file `ffprobe` answers a decode asks for on its request path, and the one place they are
//! bounded and remembered.
//!
//! [`ProbeCache`] is what [`source_traits`](super::cli::source_traits) and
//! [`source_seek_points_are_keyframes`] sit behind: one probe per file (size and modified time are
//! part of its identity) however many threads ask at once, killed after
//! [`SOURCE_PROBE_TIMEOUT`], an answer kept for good and a **failure remembered for a while**
//! ([`SOURCE_PROBE_RETRY`], as `proxy_video_info` does) so a broken or hung `ffprobe` is not
//! respawned for every frame that asks.
//!
//! [`source_seek_points_are_keyframes`] answers the one question a long-lived decode run cannot
//! answer for itself: **is a seek into this file a decode from a keyframe?** `-ss T` seeks the
//! demuxer to the last *sync sample* at or before `T` and decodes from there; the frame it returns
//! is the first one the decoder outputs at or after `T`. That is "the frame at `T`" only if the
//! decoder outputs the sync sample's own picture first. A sync sample that is not a keyframe
//! (x264's `intra-refresh`: the encoder marks the start of every refresh wave as a sync sample,
//! but the picture is a P frame whose columns refresh over the next 25 frames and the decoder
//! outputs nothing until they have) makes `-ss 2.0` return the frame at 2.72 s, where a run that
//! read through from an earlier keyframe has the true frames 2.0 to 2.68 — and only a seek *to*
//! that sync sample shows it, so no check on a run's own first frame can.
//!
//! So the sync samples are compared with what the decoder says is a keyframe: the packets
//! flagged `K` (what a seek lands on) against the pictures a `-skip_frame nokey` decode outputs
//! as `I` with `key_frame` set, matched by timestamp. Cost is bounded twice: only the packet
//! list is read (growing rung by rung, so a dense stream costs a few packets and a heavy
//! all-intra one is not read for twenty seconds), only the first [`SYNC_PROBE_POINTS`] sync
//! samples are looked at, and only their packets are decoded (`nokey` skips the rest). **The
//! limit is the file's head**: a stream whose first sync samples are keyframes and whose later
//! ones are not (two encodes concatenated) is not caught; intra refresh is a property of the
//! whole encode, so it is. A probe that cannot tell (a failure, a timeout, a timestamp
//! `ffprobe` does not print) is "no" for this call and retried after the backoff: a file a run
//! cannot be proved equal on is decoded one-shot.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use super::cli::{command, ffprobe_bin, run_piped_until, source_key};

/// How long one probe of a source (all of its `ffprobe` runs together) may take before it is
/// killed.
pub(super) const SOURCE_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a failed probe of a source is remembered before it is tried again.
const SOURCE_PROBE_RETRY: Duration = Duration::from_secs(60);

/// What is known of one probe of one file.
enum Found<T> {
    Unknown,
    Known(T),
    /// The probe failed at this moment.
    Failed(Instant),
}

/// One probe's answer per file, however many threads ask at once. See the [module](self).
pub(super) struct ProbeCache<T> {
    files: Mutex<HashMap<String, Arc<Mutex<Found<T>>>>>,
    retry: Duration,
}

impl<T: Copy> ProbeCache<T> {
    pub(super) fn new() -> Self {
        Self::retrying(SOURCE_PROBE_RETRY)
    }

    fn retrying(retry: Duration) -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            retry,
        }
    }

    /// The answer for the file at `path`: the remembered one, `None` while a failure is still
    /// remembered, else `probe()`'s — run once, with every other caller for the same file waiting
    /// for it (a probe is bounded, so the wait is) instead of spawning its own.
    pub(super) fn get(&self, path: &Path, probe: impl FnOnce() -> Option<T>) -> Option<T> {
        let slot = {
            let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(
                files
                    .entry(source_key(path))
                    .or_insert_with(|| Arc::new(Mutex::new(Found::Unknown))),
            )
        };
        let mut found = slot.lock().unwrap_or_else(PoisonError::into_inner);
        match *found {
            Found::Known(answer) => return Some(answer),
            Found::Failed(at) if at.elapsed() < self.retry => return None,
            _ => {}
        }
        let probed = probe();
        *found = match probed {
            Some(answer) => Found::Known(answer),
            None => Found::Failed(Instant::now()),
        };
        probed
    }
}

/// Whether a seek into the file at `path` is a decode from a keyframe: every one of the first
/// sync samples (see the [module](self)) decodes to a keyframe picture at its own timestamp.
/// `false` for a file where one does not, one with no sync sample in view, and one that could not
/// be probed (retried after a minute).
///
/// One cached, bounded `ffprobe` run per file (it spawns a process: not under a lock).
pub fn source_seek_points_are_keyframes(path: &Path) -> bool {
    static CACHE: OnceLock<ProbeCache<bool>> = OnceLock::new();
    CACHE
        .get_or_init(ProbeCache::new)
        .get(path, || {
            let ffprobe = ffprobe_bin();
            let deadline = Instant::now() + SOURCE_PROBE_TIMEOUT;
            probe_seek_points(|interval, entries, extra| {
                let mut cmd = command(&ffprobe);
                cmd.args(["-v", "error", "-select_streams", "v:0"])
                    .args(extra)
                    .args(["-read_intervals", interval, "-show_entries", entries, "-of", "json"])
                    .arg(path);
                run_piped_until(&mut cmd, Vec::new(), deadline).map(|out| String::from_utf8_lossy(&out).into_owned())
            })
        })
        .unwrap_or(false)
}

/// The sync samples the probe looks at: the first, and the next few.
const SYNC_PROBE_POINTS: usize = 4;

/// Packets read from the start of the file at each try, until [`SYNC_PROBE_POINTS`] sync samples
/// are in view: a stream where every packet is one (all-intra) costs the first rung, a long GOP
/// the rung that holds its first few.
const SYNC_PROBE_RUNGS: [u32; 5] = [16, 64, 256, 1024, 4096];

/// One packet of the video stream, as the demuxer states it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Packet {
    pts: Option<i64>,
    /// Flagged `K`: a seek can land here.
    sync: bool,
}

/// The probe, with `run(interval, entries, extra args)` answering one `ffprobe` run's JSON.
fn probe_seek_points(mut run: impl FnMut(&str, &str, &[&str]) -> Option<String>) -> Option<bool> {
    let mut sync = Vec::new();
    let mut packets_to_read = 0;
    for &rung in &SYNC_PROBE_RUNGS {
        let packets = parse_packets(&run(&format!("%+#{rung}"), "packet=pts,flags", &[])?)?;
        (sync, packets_to_read) = sync_points(&packets, SYNC_PROBE_POINTS);
        // Enough in view, or the whole file is.
        if sync.len() == SYNC_PROBE_POINTS || packets.len() < rung as usize {
            break;
        }
    }
    if sync.is_empty() {
        return Some(false);
    }
    let decoded = run(
        &format!("%+#{packets_to_read}"),
        "frame=pts,pkt_pts,best_effort_timestamp,key_frame,pict_type",
        &["-skip_frame", "nokey"],
    )?;
    Some(decode_as_keyframes(&sync, &parse_keyframes(&decoded)?))
}

/// The video packets of `ffprobe -show_entries packet=pts,flags -of json`.
fn parse_packets(json: &str) -> Option<Vec<Packet>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    // A file with no packet at all prints no list.
    let list = v.get("packets").map_or(Some(&[][..]), |p| p.as_array().map(Vec::as_slice))?;
    Some(
        list.iter()
            .map(|p| Packet {
                pts: p.get("pts").and_then(serde_json::Value::as_i64),
                sync: p
                    .get("flags")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|f| f.starts_with('K')),
            })
            .collect(),
    )
}

/// The timestamps of the first `points` sync samples among `packets`, and how many packets
/// (from the start) hold them.
fn sync_points(packets: &[Packet], points: usize) -> (Vec<Option<i64>>, usize) {
    let mut found = Vec::new();
    let mut read = 0;
    for (i, p) in packets.iter().enumerate() {
        if p.sync {
            found.push(p.pts);
            read = i + 1;
            if found.len() == points {
                break;
            }
        }
    }
    (found, read)
}

/// The timestamps of the keyframes in `ffprobe -skip_frame nokey -show_entries
/// frame=pts,pkt_pts,best_effort_timestamp,key_frame,pict_type -of json`: pictures that are `I`
/// and flagged `key_frame`. The frame's timestamp is `pts` where ffprobe prints it and `pkt_pts`
/// (FFmpeg before 5) or the best effort one where it does not.
fn parse_keyframes(json: &str) -> Option<HashSet<i64>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let list = v.get("frames").map_or(Some(&[][..]), |f| f.as_array().map(Vec::as_slice))?;
    Some(
        list.iter()
            .filter(|f| {
                f.get("key_frame").and_then(serde_json::Value::as_i64) == Some(1)
                    && f.get("pict_type").and_then(serde_json::Value::as_str) == Some("I")
            })
            .filter_map(|f| {
                ["pts", "pkt_pts", "best_effort_timestamp"]
                    .iter()
                    .find_map(|k| f.get(k).and_then(serde_json::Value::as_i64))
            })
            .collect(),
    )
}

/// Every sync sample decoded to a keyframe at its own timestamp (and there was one to look at).
fn decode_as_keyframes(sync: &[Option<i64>], keyframes: &HashSet<i64>) -> bool {
    !sync.is_empty() && sync.iter().all(|pts| pts.is_some_and(|p| keyframes.contains(&p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packets_json(sync_every: usize, count: usize) -> String {
        let list: Vec<String> = (0..count)
            .map(|i| {
                let flags = if i % sync_every == 0 { "K__" } else { "___" };
                format!(r#"{{"pts":{},"flags":"{flags}"}}"#, i * 512)
            })
            .collect();
        format!(r#"{{"packets":[{}]}}"#, list.join(","))
    }

    fn frames_json(frames: &[(i64, u8, &str)]) -> String {
        let list: Vec<String> = frames
            .iter()
            .map(|(pts, key, kind)| format!(r#"{{"key_frame":{key},"pts":{pts},"pict_type":"{kind}"}}"#))
            .collect();
        format!(r#"{{"frames":[{}]}}"#, list.join(","))
    }

    #[test]
    fn the_sync_points_are_the_first_flagged_packets_and_the_packets_that_hold_them() {
        let packets = parse_packets(&packets_json(50, 300)).expect("packets");
        assert_eq!(packets.len(), 300);
        let (sync, read) = sync_points(&packets, 4);
        assert_eq!(sync, vec![Some(0), Some(25600), Some(51200), Some(76800)]);
        assert_eq!(read, 151);
        // Fewer than asked for: all that there are, and the packets up to the last.
        let (sync, read) = sync_points(&packets[..120], 4);
        assert_eq!((sync.len(), read), (3, 101));
        // All-intra: the first few packets.
        let intra = parse_packets(&packets_json(1, 40)).expect("packets");
        assert_eq!(sync_points(&intra, 4).1, 4);
        // None flagged, or no packets: nothing to look at.
        let none = parse_packets(&packets_json(1000, 5).replace("K__", "___")).expect("packets");
        assert_eq!(sync_points(&none, 4), (Vec::new(), 0));
        assert_eq!(parse_packets("{}").expect("an empty list"), Vec::new());
        // A packet with no timestamp is still a sync point, with none to match.
        let odd = parse_packets(r#"{"packets":[{"flags":"K_"},{"pts":3,"flags":"__"}]}"#).expect("packets");
        assert_eq!(sync_points(&odd, 4).0, vec![None]);
        // FFmpeg 4's flags are two characters wide.
        assert!(parse_packets(r#"{"packets":[{"pts":0,"flags":"K_"}]}"#).expect("packets")[0].sync);
        assert_eq!(parse_packets("not json"), None);
        assert_eq!(parse_packets(r#"{"packets":3}"#), None);
    }

    #[test]
    fn a_keyframe_is_an_intra_picture_the_decoder_flags_with_a_timestamp() {
        let keys = parse_keyframes(&frames_json(&[
            (0, 1, "I"),
            (25600, 1, "I"),
            (512, 0, "P"),
            (1024, 0, "I"),
            (2048, 1, "P"),
        ]))
        .expect("frames");
        assert_eq!(keys, HashSet::from([0, 25600]));
        // FFmpeg 4 prints `pkt_pts`, and the best effort one is the last resort.
        let old = r#"{"frames":[{"key_frame":1,"pkt_pts":512,"pict_type":"I"},{"key_frame":1,"best_effort_timestamp":9,"pict_type":"I"},{"key_frame":1,"pict_type":"I"}]}"#;
        assert_eq!(parse_keyframes(old).expect("frames"), HashSet::from([512, 9]));
        // No picture decoded is an empty set, a broken answer is none.
        assert_eq!(parse_keyframes("{}").expect("no frames"), HashSet::new());
        assert_eq!(parse_keyframes("{"), None);
    }

    #[test]
    fn a_sync_sample_that_decodes_to_a_p_picture_is_not_a_keyframe() {
        let sync = [Some(0), Some(25600), Some(51200)];
        let all = HashSet::from([0, 25600, 51200]);
        assert!(decode_as_keyframes(&sync, &all));
        // Intra refresh: the stream's sync samples after the first decode to P pictures (skipped,
        // under `nokey`, or not flagged).
        assert!(!decode_as_keyframes(&sync, &HashSet::from([0])));
        assert!(!decode_as_keyframes(&sync, &HashSet::from([0, 25600])));
        // A sync sample with no timestamp cannot be matched, and none at all is nothing to trust.
        assert!(!decode_as_keyframes(&[Some(0), None], &all));
        assert!(!decode_as_keyframes(&[], &all));
    }

    /// A canned ffprobe: `sync_every` packets between sync samples in a file of `total`
    /// packets; the decode says `keyframes` and the rungs asked for are recorded.
    fn probe(sync_every: usize, total: usize, keyframes: &[i64]) -> (Option<bool>, Vec<String>) {
        let mut asked = Vec::new();
        let keys: Vec<(i64, u8, &str)> = keyframes.iter().map(|&p| (p, 1, "I")).collect();
        let answer = probe_seek_points(|interval, entries, extra| {
            asked.push(format!("{interval} {entries} {}", extra.join(" ")).trim_end().to_string());
            if entries.starts_with("packet") {
                let n: usize = interval.trim_start_matches("%+#").parse().expect("a packet count");
                Some(packets_json(sync_every, n.min(total)))
            } else {
                assert_eq!(extra, ["-skip_frame", "nokey"]);
                Some(frames_json(&keys))
            }
        });
        (answer, asked)
    }

    #[test]
    fn the_packet_list_grows_only_as_far_as_the_sync_points_need() {
        // All-intra: the first rung has them, and four packets are decoded.
        let (answer, asked) = probe(1, 10_000, &[0, 512, 1024, 1536]);
        assert_eq!(answer, Some(true));
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked[0].starts_with("%+#16 packet"), "{asked:?}");
        assert!(asked[1].starts_with("%+#4 frame"), "{asked:?}");
        // A GOP of 50 packets: four sync samples are in the 256-packet rung.
        let (answer, asked) = probe(50, 10_000, &[0, 25600, 51200, 76800]);
        assert_eq!(answer, Some(true));
        assert!(asked[2].starts_with("%+#256 packet"), "{asked:?}");
        assert!(asked[3].starts_with("%+#151 frame"), "{asked:?}");
        // A short file is read whole, once, whatever the rungs.
        let (answer, asked) = probe(50, 60, &[0, 25600]);
        assert_eq!(answer, Some(true));
        assert_eq!(asked.len(), 3, "{asked:?}");
        // One keyframe in the whole file (a single IDR): the last rung is as far as it looks.
        let (answer, asked) = probe(100_000, 100_000, &[0]);
        assert_eq!(answer, Some(true));
        assert_eq!(
            asked.iter().filter(|a| a.contains("packet")).count(),
            SYNC_PROBE_RUNGS.len(),
            "{asked:?}"
        );
        // Intra refresh: the decode has the first picture only.
        let (answer, _) = probe(50, 10_000, &[0]);
        assert_eq!(answer, Some(false));
    }

    #[test]
    fn a_probe_that_cannot_tell_is_not_an_answer() {
        assert_eq!(probe_seek_points(|_, _, _| None), None);
        assert_eq!(probe_seek_points(|_, _, _| Some("not json".into())), None);
        let mut calls = 0;
        let broken_decode = probe_seek_points(|_, entries, _| {
            calls += 1;
            entries.starts_with("packet").then(|| packets_json(1, 20))
        });
        assert_eq!((broken_decode, calls), (None, 2));
        // No sync sample at all is a "no" the file earns, not a failure.
        let flagless = probe_seek_points(|_, _, _| Some(packets_json(1, 20).replace("K__", "___")));
        assert_eq!(flagless, Some(false));
    }

    #[test]
    fn an_answer_is_kept_and_a_failure_is_remembered_for_a_while_and_then_retried() {
        let dir = std::env::temp_dir().join(format!("kerf-probe-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.bin");
        std::fs::write(&file, b"x").unwrap();
        let cache: ProbeCache<u32> = ProbeCache::retrying(Duration::from_millis(150));
        let mut probes = 0;
        // A failure is remembered: asking again does not probe.
        assert_eq!(
            cache.get(&file, || {
                probes += 1;
                None
            }),
            None
        );
        for _ in 0..5 {
            assert_eq!(
                cache.get(&file, || {
                    probes += 1;
                    Some(9)
                }),
                None
            );
        }
        assert_eq!(probes, 1);
        // After the wait it is tried again, and an answer is kept for good.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            cache.get(&file, || {
                probes += 1;
                Some(7)
            }),
            Some(7)
        );
        for _ in 0..5 {
            assert_eq!(
                cache.get(&file, || {
                    probes += 1;
                    Some(8)
                }),
                Some(7)
            );
        }
        assert_eq!(probes, 2);
        // Another file is another probe; a replaced file (another size) is too.
        let other = dir.join("b.bin");
        std::fs::write(&other, b"xy").unwrap();
        assert_eq!(cache.get(&other, || Some(1)), Some(1));
        std::fs::write(&file, b"changed").unwrap();
        assert_eq!(cache.get(&file, || Some(2)), Some(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn threads_asking_for_one_file_share_one_probe() {
        let dir = std::env::temp_dir().join(format!("kerf-probe-share-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.bin");
        std::fs::write(&file, b"x").unwrap();
        let cache: ProbeCache<u32> = ProbeCache::new();
        let probes = std::sync::atomic::AtomicU32::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let got = cache.get(&file, || {
                        probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        Some(5)
                    });
                    assert_eq!(got, Some(5));
                });
            }
        });
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
