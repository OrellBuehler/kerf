//! The decoded-frame cache: `Arc<YuvFrame>`s keyed by **which file** and **which pts**, held
//! under a byte cap, least recently used out first.
//!
//! * **Identity is `(source, pts in ticks)`**, never an index (a seek lands on a different
//!   frame number each time). `source` is [`kerf_core::source_identity`] — path, size and
//!   modified time, so a replaced file or a freshly generated proxy never serves a stale frame,
//!   and a proxy and its original are two sources — plus the [`DecodeFormat`] the planes are in.
//! * **Coverage makes a miss exact.** Every frame states `covers_from`: "no frame of this file
//!   has a pts in `covers_from..pts`", so it is the answer to `AtOrAfter(t)` for every tick `t`
//!   in `covers_from..=pts` ([`FrameCache::at_or_after`]). A frame decoded right after another
//!   covers from the tick after it; a run's *first* frame covers from the seek tick (clamped by
//!   the caller to one frame interval before the frame: an `mpegts` seek may land a whole GOP
//!   late and must not claim the frames it skipped). That is one `BTreeMap` range query and is
//!   exact on a variable frame rate, where "the frame containing `t`" is not. The same fact
//!   answers `Before(t)` ([`FrameCache::before`]): the frame at `covers_from - 1`.
//! * **The end of a file is remembered** ([`FrameCache::mark_end`]): a time past the last frame
//!   is [`Lookup::PastEnd`] — FFmpeg draws nothing there — not a miss that decodes again.
//! * **A hit is an `Arc`**, so a frame being composited outlives its eviction; what the cache
//!   counts is what it holds. **Pinned** frames (a cursor's read-ahead, a reversed window being
//!   played back) are never evicted: the cap is soft for them, and [`CacheStats::bytes`] above
//!   [`CacheStats::cap`] says so. The frame just inserted is not evicted by its own insert.
//!
//! Eviction scans for the oldest unpinned entry: the cap is a few hundred frames at most
//! (256 MiB of 720p), and a scan is what `kerf-core`'s own preview cache does.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use crate::source::YuvFrame;

/// The default cap (`KERF_FRAME_CACHE_MB` overrides it, in `FrameSource`).
pub const DEFAULT_CAP_BYTES: usize = 256 << 20;

/// What the planes of a cached frame are. Only 8-bit 4:2:0 is decoded today; HDR / 10-bit
/// delivery would add a variant, and its frames of the same file must not alias these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DecodeFormat {
    Yuv420p8,
}

/// One decoded file: its identity at the time it was opened, and what was made of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId {
    pub file: u64,
    pub format: DecodeFormat,
}

impl SourceId {
    /// The 8-bit 4:2:0 decode of the file at `path` as it is on disk now (one `stat`).
    pub fn of(path: &Path) -> Self {
        Self {
            file: kerf_core::source_identity(path),
            format: DecodeFormat::Yuv420p8,
        }
    }
}

/// A frame's identity: its file and its timestamp in the stream's own ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameKey {
    pub source: SourceId,
    pub pts: i64,
}

/// A frame the cache answered with.
#[derive(Debug, Clone)]
pub struct Hit {
    pub key: FrameKey,
    pub frame: Arc<YuvFrame>,
}

/// The cache's answer to a time.
#[derive(Debug, Clone)]
pub enum Lookup {
    Hit(Hit),
    /// The file ended before this time: there is no frame, and nothing to decode.
    PastEnd,
    Miss,
}

struct Entry {
    frame: Arc<YuvFrame>,
    covers_from: i64,
    bytes: usize,
    used: u64,
    pins: u32,
}

/// What the cache holds and how it has been used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub cap: usize,
    pub pinned: usize,
    pub pinned_bytes: usize,
    pub inserts: u64,
    pub hits: u64,
    pub misses: u64,
    pub past_end: u64,
    pub evictions: u64,
}

/// See the [module](self).
pub struct FrameCache {
    cap: usize,
    frames: BTreeMap<FrameKey, Entry>,
    /// The last frame's pts of every file known to have ended.
    ends: HashMap<SourceId, i64>,
    bytes: usize,
    clock: u64,
    stats: CacheStats,
}

fn frame_bytes(f: &YuvFrame) -> usize {
    f.y.len() + f.u.len() + f.v.len()
}

impl FrameCache {
    pub fn new(cap_bytes: usize) -> Self {
        Self {
            cap: cap_bytes,
            frames: BTreeMap::new(),
            ends: HashMap::new(),
            bytes: 0,
            clock: 0,
            stats: CacheStats::default(),
        }
    }

    /// Keep `frame` under `key`, covering `covers_from..=key.pts` (see the [module](self)), and
    /// return the frame the cache now holds. A frame already there stays (it is the same
    /// picture) and only widens its coverage: two true statements about the same file add up.
    /// Older unpinned frames go until the cap is met; the one inserted never does.
    pub fn insert(&mut self, key: FrameKey, frame: Arc<YuvFrame>, covers_from: i64) -> Arc<YuvFrame> {
        let covers_from = covers_from.min(key.pts);
        self.clock += 1;
        let used = self.clock;
        let held = match self.frames.get_mut(&key) {
            Some(e) => {
                e.covers_from = e.covers_from.min(covers_from);
                e.used = used;
                Arc::clone(&e.frame)
            }
            None => {
                let bytes = frame_bytes(&frame);
                self.bytes += bytes;
                self.stats.inserts += 1;
                self.frames.insert(
                    key,
                    Entry {
                        frame: Arc::clone(&frame),
                        covers_from,
                        bytes,
                        used,
                        pins: 0,
                    },
                );
                frame
            }
        };
        self.evict(Some(key));
        held
    }

    /// The frame at exactly `key`.
    pub fn get(&mut self, key: FrameKey) -> Option<Arc<YuvFrame>> {
        self.clock += 1;
        let used = self.clock;
        self.frames.get_mut(&key).map(|e| {
            e.used = used;
            Arc::clone(&e.frame)
        })
    }

    /// The first frame with `pts >= ticks` — what `-ss` returns — if the cache can *prove* it is
    /// that frame (its coverage reaches `ticks`), else [`Lookup::PastEnd`] when the file is
    /// known to end before `ticks`, else a miss.
    pub fn at_or_after(&mut self, source: SourceId, ticks: i64) -> Lookup {
        let found = self.find(source, ticks);
        let outcome = match found {
            Some(hit) => Lookup::Hit(hit),
            None if self.ends.get(&source).is_some_and(|&last| ticks > last) => Lookup::PastEnd,
            None => Lookup::Miss,
        };
        match outcome {
            Lookup::Hit(_) => self.stats.hits += 1,
            Lookup::PastEnd => self.stats.past_end += 1,
            Lookup::Miss => self.stats.misses += 1,
        }
        outcome
    }

    /// The frame preceding [`FrameCache::at_or_after`]'s (the last with `pts < ticks`) when
    /// the cache holds both it and the proof that nothing lies between: the coverage of the
    /// frame after it starts one tick past it. A run's first frame has no such predecessor
    /// on record, so this is a miss there — the caller restarts earlier. Past the end of a file
    /// known to have ended it is the last frame.
    pub fn before(&mut self, source: SourceId, ticks: i64) -> Option<Hit> {
        let key = match self.find(source, ticks) {
            Some(after) => FrameKey {
                source,
                pts: self.frames.get(&after.key)?.covers_from - 1,
            },
            None => FrameKey {
                source,
                pts: *self.ends.get(&source).filter(|&&last| ticks > last)?,
            },
        };
        let frame = self.get(key)?;
        Some(Hit { key, frame })
    }

    fn find(&mut self, source: SourceId, ticks: i64) -> Option<Hit> {
        let from = FrameKey { source, pts: ticks };
        let (key, entry) = self.frames.range_mut(from..).next()?;
        if key.source != source || entry.covers_from > ticks {
            return None;
        }
        self.clock += 1;
        entry.used = self.clock;
        Some(Hit {
            key: *key,
            frame: Arc::clone(&entry.frame),
        })
    }

    /// Record that the file has no frame after `last_pts`.
    pub fn mark_end(&mut self, source: SourceId, last_pts: i64) {
        self.ends.insert(source, last_pts);
    }

    /// Keep a frame in the cache whatever is inserted next. Counted: every `pin` needs an
    /// `unpin`. `false` when there is no such frame.
    pub fn pin(&mut self, key: FrameKey) -> bool {
        self.frames.get_mut(&key).map(|e| e.pins += 1).is_some()
    }

    /// Release one [`FrameCache::pin`] (extra releases are ignored), and evict if the cap was
    /// being held over.
    pub fn unpin(&mut self, key: FrameKey) {
        if let Some(e) = self.frames.get_mut(&key) {
            e.pins = e.pins.saturating_sub(1);
        }
        self.evict(None);
    }

    /// Forget a file entirely, pins included (its runs are gone).
    pub fn purge(&mut self, source: SourceId) {
        self.frames.retain(|k, e| {
            let keep = k.source != source;
            if !keep {
                self.bytes -= e.bytes;
            }
            keep
        });
        self.ends.remove(&source);
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.ends.clear();
        self.bytes = 0;
    }

    pub fn stats(&self) -> CacheStats {
        let pinned = self.frames.values().filter(|e| e.pins > 0);
        CacheStats {
            entries: self.frames.len(),
            bytes: self.bytes,
            cap: self.cap,
            pinned: pinned.clone().count(),
            pinned_bytes: pinned.map(|e| e.bytes).sum(),
            ..self.stats
        }
    }

    /// Drop the least recently used unpinned frames (never `keep`) until the cap is met.
    fn evict(&mut self, keep: Option<FrameKey>) {
        while self.bytes > self.cap {
            let oldest = self
                .frames
                .iter()
                .filter(|(k, e)| e.pins == 0 && Some(**k) != keep)
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| *k);
            let Some(oldest) = oldest else { break };
            if let Some(e) = self.frames.remove(&oldest) {
                self.bytes -= e.bytes;
                self.stats.evictions += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: SourceId = SourceId {
        file: 1,
        format: DecodeFormat::Yuv420p8,
    };
    const B: SourceId = SourceId {
        file: 2,
        format: DecodeFormat::Yuv420p8,
    };

    /// A 4x2 frame: 8 + 2 + 2 = 12 bytes, its first luma byte `tag`.
    fn frame(tag: u8) -> Arc<YuvFrame> {
        let mut y = vec![0; 8];
        y[0] = tag;
        Arc::new(YuvFrame {
            width: 4,
            height: 2,
            y,
            u: vec![0; 2],
            v: vec![0; 2],
        })
    }

    fn key(source: SourceId, pts: i64) -> FrameKey {
        FrameKey { source, pts }
    }

    fn tag(l: Lookup) -> Option<u8> {
        match l {
            Lookup::Hit(h) => Some(h.frame.y[0]),
            _ => None,
        }
    }

    /// A decoded run of `pts` (one frame after another, the first covering from `from`).
    fn run(c: &mut FrameCache, source: SourceId, from: i64, pts: &[i64]) {
        let mut covers = from;
        for &p in pts {
            c.insert(key(source, p), frame(p as u8), covers);
            covers = p + 1;
        }
    }

    #[test]
    fn a_frame_is_held_once_and_a_second_insert_returns_the_first() {
        let mut c = FrameCache::new(1 << 20);
        let first = c.insert(key(A, 10), frame(1), 5);
        let again = c.insert(key(A, 10), frame(2), 8);
        assert!(Arc::ptr_eq(&first, &again), "the same picture is not stored twice");
        assert_eq!(c.stats().entries, 1);
        assert_eq!(c.stats().bytes, 12);
        assert!(Arc::ptr_eq(&c.get(key(A, 10)).unwrap(), &first));
        assert!(c.get(key(A, 11)).is_none());
        // The wider of two true coverages wins.
        assert_eq!(tag(c.at_or_after(A, 5)), Some(1));
        assert_eq!(c.stats().inserts, 1);
    }

    #[test]
    fn the_cap_evicts_the_least_recently_used_and_a_hit_renews_a_frame() {
        let mut c = FrameCache::new(36);
        for p in 0..3 {
            c.insert(key(A, p), frame(p as u8), p);
        }
        assert_eq!(c.stats().bytes, 36);
        // Touch frame 0, so frame 1 is now the oldest.
        assert!(c.get(key(A, 0)).is_some());
        c.insert(key(A, 3), frame(3), 3);
        assert!(c.get(key(A, 1)).is_none(), "the oldest went");
        for p in [0, 2, 3] {
            assert!(c.get(key(A, p)).is_some(), "frame {p}");
        }
        let s = c.stats();
        assert_eq!((s.entries, s.bytes, s.evictions, s.inserts), (3, 36, 1, 4));
    }

    #[test]
    fn an_evicted_frame_outlives_its_eviction_in_the_hands_of_whoever_has_it() {
        let mut c = FrameCache::new(12);
        let held = c.insert(key(A, 0), frame(7), 0);
        c.insert(key(A, 1), frame(8), 1);
        assert!(c.get(key(A, 0)).is_none());
        assert_eq!(held.y[0], 7);
        assert_eq!(Arc::strong_count(&held), 1, "the cache let go of it");
    }

    #[test]
    fn a_pinned_frame_is_never_evicted_and_the_cap_gives_way_rather_than_the_frame() {
        let mut c = FrameCache::new(24);
        c.insert(key(A, 0), frame(0), 0);
        c.insert(key(A, 1), frame(1), 1);
        assert!(c.pin(key(A, 0)));
        assert!(c.pin(key(A, 0)), "pins count");
        assert!(!c.pin(key(A, 9)), "nothing to pin");
        c.insert(key(A, 2), frame(2), 2);
        assert!(c.get(key(A, 0)).is_some(), "pinned");
        assert!(c.get(key(A, 1)).is_none(), "the unpinned one went instead");
        // Everything held is pinned or just inserted: the cap is exceeded, not a frame dropped.
        assert!(c.pin(key(A, 2)));
        c.insert(key(A, 3), frame(3), 3);
        let s = c.stats();
        assert_eq!((s.entries, s.pinned, s.pinned_bytes), (3, 2, 24));
        assert!(s.bytes > s.cap, "{s:?}");
        // Releasing a pin trims what the cap was being held over: the one frame nothing
        // holds (3) goes at the first release, though frame 0 is still pinned once.
        c.unpin(key(A, 0));
        assert_eq!((c.stats().entries, c.stats().bytes, c.stats().pinned), (2, 24, 2));
        assert!(c.get(key(A, 3)).is_none());
        // Within the cap nothing else goes, and releasing what is not pinned is harmless.
        c.unpin(key(A, 0));
        c.unpin(key(A, 0));
        c.unpin(key(A, 9));
        c.unpin(key(A, 2));
        assert_eq!((c.stats().entries, c.stats().pinned), (2, 0));
    }

    #[test]
    fn the_frame_just_inserted_is_not_its_own_victim_even_over_the_cap() {
        let mut c = FrameCache::new(5);
        c.insert(key(A, 0), frame(0), 0);
        assert_eq!(c.stats().entries, 1);
        c.insert(key(A, 1), frame(1), 1);
        assert_eq!(c.stats().entries, 1);
        assert!(c.get(key(A, 1)).is_some());
    }

    #[test]
    fn a_time_is_answered_by_the_frame_whose_coverage_reaches_it() {
        let mut c = FrameCache::new(1 << 20);
        // A run from a seek to tick 100, then 3 frames 40 ticks apart.
        run(&mut c, A, 100, &[130, 170, 210]);
        // The first covers from the seek tick, each next from the tick after its predecessor.
        for (t, want) in [(100, 130), (130, 130), (131, 170), (170, 170), (171, 210), (210, 210)] {
            assert_eq!(tag(c.at_or_after(A, t)), Some(want as u8), "t = {t}");
        }
        // Before the seek, or past the last frame, the cache cannot say (the file may go on).
        assert!(matches!(c.at_or_after(A, 99), Lookup::Miss));
        assert!(matches!(c.at_or_after(A, 211), Lookup::Miss));
        // Another file's identical timestamps are another frame.
        run(&mut c, B, 0, &[130]);
        assert_eq!(tag(c.at_or_after(B, 100)), Some(130));
        assert!(
            matches!(c.at_or_after(B, 131), Lookup::Miss),
            "no leaking into the next source"
        );
        let s = c.stats();
        assert_eq!((s.hits, s.misses), (7, 3));
    }

    #[test]
    fn a_hole_is_a_miss_not_the_next_frame() {
        let mut c = FrameCache::new(36);
        run(&mut c, A, 0, &[10, 20, 30]);
        // Frame 20 is evicted (touch the others so it is the oldest, then add a fourth).
        c.get(key(A, 10));
        c.get(key(A, 30));
        c.insert(key(A, 40), frame(40), 31);
        assert!(c.get(key(A, 20)).is_none());
        // Ticks 11..=20 were frame 20's: frame 30 does not claim them, though it is the next frame held.
        assert!(matches!(c.at_or_after(A, 15), Lookup::Miss));
        assert_eq!(tag(c.at_or_after(A, 25)), Some(30));
    }

    #[test]
    fn the_end_of_a_file_is_remembered_and_answers_for_every_later_time() {
        let mut c = FrameCache::new(1 << 20);
        run(&mut c, A, 0, &[10, 20]);
        assert!(matches!(c.at_or_after(A, 21), Lookup::Miss));
        c.mark_end(A, 20);
        assert!(matches!(c.at_or_after(A, 21), Lookup::PastEnd));
        assert!(matches!(c.at_or_after(A, 99), Lookup::PastEnd));
        assert_eq!(tag(c.at_or_after(A, 20)), Some(20), "the last frame itself is a frame");
        assert!(matches!(c.at_or_after(B, 21), Lookup::Miss), "per file");
        assert_eq!(c.stats().past_end, 2);
    }

    #[test]
    fn the_frame_before_a_time_needs_the_proof_that_nothing_lies_between() {
        let mut c = FrameCache::new(1 << 20);
        run(&mut c, A, 100, &[130, 170, 210]);
        let before = |c: &mut FrameCache, t| c.before(A, t).map(|h| h.frame.y[0]);
        assert_eq!(before(&mut c, 171), Some(170), "t just past a frame");
        assert_eq!(before(&mut c, 210), Some(170));
        assert_eq!(
            before(&mut c, 211),
            None,
            "frame 210 is the last held, but the file may go on"
        );
        assert_eq!(before(&mut c, 131), Some(130));
        // The run's first frame has no predecessor on record: restart earlier.
        assert_eq!(before(&mut c, 130), None);
        assert_eq!(before(&mut c, 101), None);
        // A file known to end: the last frame is before anything past it.
        c.mark_end(A, 210);
        assert_eq!(before(&mut c, 500), Some(210));
        assert_eq!(before(&mut c, 210), Some(170));
    }

    #[test]
    fn purging_a_source_drops_its_frames_pins_and_end_and_no_others() {
        let mut c = FrameCache::new(1 << 20);
        run(&mut c, A, 0, &[1, 2]);
        run(&mut c, B, 0, &[1]);
        c.pin(key(A, 1));
        c.mark_end(A, 2);
        c.purge(A);
        let s = c.stats();
        assert_eq!((s.entries, s.bytes, s.pinned), (1, 12, 0));
        assert!(matches!(c.at_or_after(A, 3), Lookup::Miss));
        assert!(c.get(key(B, 1)).is_some());
        c.clear();
        assert_eq!((c.stats().entries, c.stats().bytes), (0, 0));
    }
}
