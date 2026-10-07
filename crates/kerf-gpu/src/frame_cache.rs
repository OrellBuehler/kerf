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
//!   played back) are never evicted: [`FrameCache::pin`] hands out a [`PinGuard`] and the pin
//!   lasts as long as it does (no `unpin` to forget). The cap is soft for them, and
//!   [`CacheStats::bytes`] above [`CacheStats::cap`] says so. The frame just inserted is not
//!   evicted by its own insert.
//! * **A run that reads forward past frames nobody asked for must not flush the ones somebody
//!   did.** Reading 90 frames to reach the wanted one inserts 90 frames, and on 1080p the cap is
//!   86 of them: with the cache full of other layers' frames, one read-forward would evict all of
//!   them and the next request would restart a run to get one back. So a frame the run only
//!   *passed* goes in with [`FrameCache::insert_cold`]: it takes free space or the place of
//!   another cold frame, **never a warm frame's**, and is dropped (the caller still has the
//!   `Arc`) when there is no such room. A cold frame that is looked up turns warm; warm eviction
//!   takes cold frames first. Which frames are *passed*: those before `Request::warm_from` (see
//!   `router`), the wanted frame less the backward lead.
//!
//! Eviction scans for the oldest unpinned entry (cold ones first): the cap is a few hundred
//! frames at most (256 MiB of 720p), and a scan is what `kerf-core`'s own preview cache does.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

/// The pin count of one entry, shared with its [`PinGuard`]s so that dropping one needs no
/// access to the cache (and so no lock). `live` goes false when the entry is purged.
struct PinState {
    count: AtomicU32,
    live: AtomicBool,
}

struct Entry {
    frame: Arc<YuvFrame>,
    covers_from: i64,
    bytes: usize,
    used: u64,
    /// Inserted by [`FrameCache::insert_cold`] and not looked up since.
    cold: bool,
    pins: Arc<PinState>,
}

impl Entry {
    fn pinned(&self) -> bool {
        self.pins.count.load(Ordering::Relaxed) > 0
    }
}

/// A frame held in the cache for as long as this lives: it is not evicted. Dropping it releases
/// the pin; that cannot evict anything (it has no cache to do it with), so the cap is met at
/// the next insert or [`FrameCache::trim`].
///
/// The guard owns the *entry's* pin count, so purging the file ([`FrameCache::purge`],
/// [`FrameCache::clear`]) detaches it: [`PinGuard::is_live`] is `false`, the frame it holds is
/// still good, and when the guard drops it touches nothing that was cached afterwards under the
/// same key (the new entry has a count of its own — no generation number to compare).
#[derive(Debug)]
pub struct PinGuard {
    key: FrameKey,
    frame: Arc<YuvFrame>,
    pins: Arc<PinState>,
}

impl PinGuard {
    pub fn key(&self) -> FrameKey {
        self.key
    }

    pub fn frame(&self) -> &Arc<YuvFrame> {
        &self.frame
    }

    /// The cache still holds the entry this pins.
    pub fn is_live(&self) -> bool {
        self.pins.live.load(Ordering::Relaxed)
    }
}

impl Drop for PinGuard {
    fn drop(&mut self) {
        self.pins.count.fetch_sub(1, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for PinState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PinState({})", self.count.load(Ordering::Relaxed))
    }
}

/// What the cache holds and how it has been used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub cap: usize,
    pub pinned: usize,
    pub pinned_bytes: usize,
    /// Frames held that were only passed on the way to another and not looked up since.
    pub cold: usize,
    pub inserts: u64,
    pub hits: u64,
    pub misses: u64,
    pub past_end: u64,
    pub evictions: u64,
    /// Passed frames that were not kept because keeping them would have cost a warm one.
    pub cold_dropped: u64,
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
    /// Older unpinned frames go until the cap is met (cold ones first); the one inserted never does.
    pub fn insert(&mut self, key: FrameKey, frame: Arc<YuvFrame>, covers_from: i64) -> Arc<YuvFrame> {
        let held = self.put(key, frame, covers_from, false);
        self.evict(Some(key));
        held
    }

    /// [`FrameCache::insert`] for a frame the decode only **passed** on the way to the one that
    /// was asked for (see the [module](self)): kept in free space, or in the place of other
    /// cold frames, but never at the cost of a warm one — when that is all there is, it is not
    /// kept and `cold_dropped` counts it. A frame already held is left as warm as it was.
    /// Either way the caller gets the frame back.
    pub fn insert_cold(&mut self, key: FrameKey, frame: Arc<YuvFrame>, covers_from: i64) -> Arc<YuvFrame> {
        if self.frames.contains_key(&key) {
            return self.put(key, frame, covers_from, true);
        }
        let excess = (self.bytes + frame_bytes(&frame)).saturating_sub(self.cap);
        let room: usize = self.frames.values().filter(|e| e.cold && !e.pinned()).map(|e| e.bytes).sum();
        if excess > room {
            self.stats.cold_dropped += 1;
            return frame;
        }
        let held = self.put(key, frame, covers_from, true);
        // Only cold frames can be taken: they alone were counted as room.
        self.evict_where(Some(key), |e| e.cold);
        held
    }

    fn put(&mut self, key: FrameKey, frame: Arc<YuvFrame>, covers_from: i64, cold: bool) -> Arc<YuvFrame> {
        let covers_from = covers_from.min(key.pts);
        self.clock += 1;
        let used = self.clock;
        match self.frames.get_mut(&key) {
            Some(e) => {
                e.covers_from = e.covers_from.min(covers_from);
                if !cold {
                    (e.used, e.cold) = (used, false);
                }
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
                        cold,
                        pins: Arc::new(PinState {
                            count: AtomicU32::new(0),
                            live: AtomicBool::new(true),
                        }),
                    },
                );
                frame
            }
        }
    }

    /// The frame at exactly `key`.
    pub fn get(&mut self, key: FrameKey) -> Option<Arc<YuvFrame>> {
        self.clock += 1;
        let used = self.clock;
        self.frames.get_mut(&key).map(|e| {
            (e.used, e.cold) = (used, false);
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
                pts: self.frames.get(&after.key)?.covers_from.checked_sub(1)?,
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
        (entry.used, entry.cold) = (self.clock, false);
        Some(Hit {
            key: *key,
            frame: Arc::clone(&entry.frame),
        })
    }

    /// Record that the file has no frame after `last_pts`.
    pub fn mark_end(&mut self, source: SourceId, last_pts: i64) {
        self.ends.insert(source, last_pts);
    }

    /// Keep the frame at `key` in the cache for as long as the guard lives. `None` when there is
    /// no such frame.
    pub fn pin(&mut self, key: FrameKey) -> Option<PinGuard> {
        let e = self.frames.get(&key)?;
        e.pins.count.fetch_add(1, Ordering::Relaxed);
        Some(PinGuard {
            key,
            frame: Arc::clone(&e.frame),
            pins: Arc::clone(&e.pins),
        })
    }

    /// Meet the cap if frames were being held over it and their guards have dropped.
    pub fn trim(&mut self) {
        self.evict(None);
    }

    /// Forget a file entirely, pinned frames included (its runs are gone): their guards stop
    /// being [live](PinGuard::is_live).
    pub fn purge(&mut self, source: SourceId) {
        let mut freed = 0;
        self.frames.retain(|k, e| {
            let keep = k.source != source;
            if !keep {
                freed += e.bytes;
                e.pins.live.store(false, Ordering::Relaxed);
            }
            keep
        });
        self.bytes -= freed;
        self.ends.remove(&source);
    }

    pub fn clear(&mut self) {
        for e in self.frames.values() {
            e.pins.live.store(false, Ordering::Relaxed);
        }
        self.frames.clear();
        self.ends.clear();
        self.bytes = 0;
    }

    pub fn stats(&self) -> CacheStats {
        let pinned = self.frames.values().filter(|e| e.pinned());
        CacheStats {
            entries: self.frames.len(),
            bytes: self.bytes,
            cap: self.cap,
            pinned: pinned.clone().count(),
            pinned_bytes: pinned.map(|e| e.bytes).sum(),
            cold: self.frames.values().filter(|e| e.cold).count(),
            ..self.stats
        }
    }

    /// Drop the least recently used unpinned frames (cold ones first, never `keep`) until the
    /// cap is met.
    fn evict(&mut self, keep: Option<FrameKey>) {
        self.evict_where(keep, |_| true);
    }

    fn evict_where(&mut self, keep: Option<FrameKey>, allowed: impl Fn(&Entry) -> bool) {
        while self.bytes > self.cap {
            let oldest = self
                .frames
                .iter()
                .filter(|(k, e)| !e.pinned() && Some(**k) != keep && allowed(e))
                .min_by_key(|(_, e)| (!e.cold, e.used))
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
        let first = c.pin(key(A, 0)).expect("held");
        let again = c.pin(key(A, 0)).expect("pins count");
        assert!(c.pin(key(A, 9)).is_none(), "nothing to pin");
        assert_eq!((first.key(), first.frame().y[0]), (key(A, 0), 0));
        c.insert(key(A, 2), frame(2), 2);
        assert!(c.get(key(A, 0)).is_some(), "pinned");
        assert!(c.get(key(A, 1)).is_none(), "the unpinned one went instead");
        // Everything held is pinned or just inserted: the cap is exceeded, not a frame dropped.
        let third = c.pin(key(A, 2)).expect("held");
        c.insert(key(A, 3), frame(3), 3);
        let s = c.stats();
        assert_eq!((s.entries, s.pinned, s.pinned_bytes), (3, 2, 24));
        assert!(s.bytes > s.cap, "{s:?}");
        // A guard drops with no cache to evict from: the cap is met at the next `trim` (or
        // insert). One of frame 0's two pins is still held, so frame 0 stays; frame 3, which
        // nothing holds, goes.
        drop(first);
        c.trim();
        assert_eq!((c.stats().entries, c.stats().bytes, c.stats().pinned), (2, 24, 2));
        assert!(c.get(key(A, 3)).is_none());
        drop(again);
        drop(third);
        c.trim();
        assert_eq!(
            (c.stats().entries, c.stats().pinned),
            (2, 0),
            "within the cap nothing else goes"
        );
    }

    #[test]
    fn a_guard_dropped_after_its_file_was_purged_touches_nothing_cached_since() {
        let mut c = FrameCache::new(1 << 20);
        run(&mut c, A, 0, &[1, 2]);
        let stale = c.pin(key(A, 1)).expect("held");
        assert!(stale.is_live());
        c.purge(A);
        assert!(!stale.is_live(), "the entry is gone");
        assert_eq!(stale.frame().y[0], 1, "the frame it held is still good");
        // The same key cached again is a new entry with a pin count of its own.
        run(&mut c, A, 0, &[1]);
        let fresh = c.pin(key(A, 1)).expect("held");
        drop(stale);
        assert_eq!(c.stats().pinned, 1, "dropping the stale guard did not unpin the new entry");
        assert!(fresh.is_live());
        c.clear();
        assert!(!fresh.is_live());
        drop(fresh);
        assert_eq!(c.stats().pinned, 0);
    }

    #[test]
    fn a_frame_that_was_only_passed_never_costs_a_frame_somebody_asked_for() {
        // Room for three frames, all of them warm: a run reading past more of them keeps none.
        let mut c = FrameCache::new(36);
        run(&mut c, A, 0, &[10, 20, 30]);
        for p in 100..110 {
            let back = c.insert_cold(key(B, p), frame(p as u8), p);
            assert_eq!(back.y[0], p as u8, "the caller still has the frame");
        }
        let s = c.stats();
        assert_eq!((s.entries, s.cold, s.cold_dropped, s.evictions), (3, 0, 10, 0));
        assert_eq!(tag(c.at_or_after(A, 10)), Some(10));
        assert!(matches!(c.at_or_after(B, 100), Lookup::Miss));
        // With free room they are kept, and they take each other's place, oldest first.
        let mut c = FrameCache::new(36);
        run(&mut c, A, 0, &[10]);
        for p in 100..105 {
            c.insert_cold(key(B, p), frame(p as u8), p);
        }
        let s = c.stats();
        assert_eq!((s.entries, s.cold, s.cold_dropped, s.evictions), (3, 2, 0, 3));
        assert!(c.get(key(B, 100)).is_none() && c.get(key(B, 102)).is_none());
        assert!(
            c.get(key(B, 103)).is_some() && c.get(key(B, 104)).is_some(),
            "the newest passed frames"
        );
        assert!(c.get(key(A, 10)).is_some(), "the warm one never moved");
    }

    #[test]
    fn a_cold_frame_that_is_looked_up_turns_warm_and_cold_ones_are_evicted_first() {
        let mut c = FrameCache::new(36);
        c.insert(key(A, 1), frame(1), 1);
        c.insert_cold(key(A, 2), frame(2), 2);
        c.insert_cold(key(A, 3), frame(3), 3);
        // Looking frame 2 up (a hit) makes it a frame somebody wanted.
        assert_eq!(tag(c.at_or_after(A, 2)), Some(2));
        assert_eq!(c.stats().cold, 1);
        // A warm insert over the cap takes the cold frame (3) although it is the *newest* ...
        c.insert(key(A, 4), frame(4), 4);
        assert!(c.get(key(A, 3)).is_none());
        // ... and inserting a frame cold again does not warm it, nor evict anything: the next
        // warm insert takes the oldest warm frame (1; frame 2 was looked up since).
        c.insert_cold(key(A, 4), frame(4), 4);
        c.insert(key(A, 5), frame(5), 5);
        assert!(c.get(key(A, 1)).is_none());
        assert!([2, 4, 5].iter().all(|&p| c.get(key(A, p)).is_some()));
        // A cold insert of a frame already held leaves it as warm as it was and keeps one `Arc`.
        let held = c.get(key(A, 4)).unwrap();
        assert!(Arc::ptr_eq(&c.insert_cold(key(A, 4), frame(9), 3), &held));
        assert_eq!(c.stats().cold, 0);
    }

    #[test]
    fn a_pinned_cold_frame_is_not_room_and_a_frame_bigger_than_the_cache_is_not_kept() {
        let mut c = FrameCache::new(24);
        c.insert_cold(key(A, 1), frame(1), 1);
        c.insert_cold(key(A, 2), frame(2), 2);
        let pinned = c.pin(key(A, 1)).unwrap();
        c.insert_cold(key(A, 3), frame(3), 3);
        assert!(c.get(key(A, 1)).is_some() && c.get(key(A, 3)).is_some() && c.get(key(A, 2)).is_none());
        let again = c.pin(key(A, 3)).unwrap();
        // Everything cold is pinned: nothing can make room.
        c.insert_cold(key(A, 4), frame(4), 4);
        assert!(c.get(key(A, 4)).is_none());
        assert_eq!(c.stats().cold_dropped, 1);
        drop((pinned, again));
        let mut tiny = FrameCache::new(5);
        tiny.insert_cold(key(A, 1), frame(1), 1);
        assert_eq!((tiny.stats().entries, tiny.stats().cold_dropped), (0, 1));
    }

    #[test]
    fn the_extremes_of_a_tick_do_not_overflow() {
        let mut c = FrameCache::new(1 << 20);
        // A frame that claims to cover from the very first tick: the one before it is no frame.
        c.insert(key(A, i64::MIN + 5), frame(1), i64::MIN);
        assert_eq!(tag(c.at_or_after(A, i64::MIN)), Some(1));
        assert!(c.before(A, i64::MIN).is_none());
        assert!(c.before(A, i64::MIN + 5).is_none());
        c.insert(key(A, i64::MAX), frame(2), i64::MAX - 1);
        assert_eq!(tag(c.at_or_after(A, i64::MAX)), Some(2));
        assert_eq!(c.before(A, i64::MAX).map(|h| h.key.pts), None);
        c.mark_end(A, i64::MAX);
        assert!(matches!(c.at_or_after(A, i64::MAX), Lookup::Hit(_)));
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
        let pin = c.pin(key(A, 1));
        c.mark_end(A, 2);
        c.purge(A);
        drop(pin);
        let s = c.stats();
        assert_eq!((s.entries, s.bytes, s.pinned), (1, 12, 0));
        assert!(matches!(c.at_or_after(A, 3), Lookup::Miss));
        assert!(c.get(key(B, 1)).is_some());
        c.clear();
        assert_eq!((c.stats().entries, c.stats().bytes), (0, 0));
    }
}
