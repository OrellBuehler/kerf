//! A bounded least-recently-used store of composited JPEGs, keyed by what the frame was drawn from.
//!
//! Pure: no process, no clock. [`StillKey`] is a digest of the argv a one-shot still of that
//! frame would be handed plus the stamp (size, modified time) of every file it reads, so **an edit
//! that changes the frame changes its key** and nothing has to be invalidated: a stale entry is a
//! different key that nothing asks for again, and ages out. Bounded twice, by entries and by
//! bytes, because the entry count is no memory bound (a 1920 px JPEG is ten times a 960 px one).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use sha2::{Digest, Sha256};

/// What one composited frame was drawn from: 128 bits of a SHA-256 over the still's argv and the
/// files behind its `-i`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct StillKey([u8; 16]);

impl StillKey {
    /// The key of the still `args` describes. `stamp` says what is on disk behind an input path
    /// (see [`file_stamp`]); a file replaced under the same name has another stamp, so another key.
    pub(crate) fn of(args: &[String], stamp: impl Fn(&str) -> Vec<u8>) -> Self {
        let mut hash = Sha256::new();
        for arg in args {
            hash.update(arg.as_bytes());
            hash.update([0]);
        }
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            if arg == "-i" {
                if let Some(path) = rest.next() {
                    hash.update(stamp(path));
                    hash.update([1]);
                }
            }
        }
        let digest = hash.finalize();
        let mut key = [0u8; 16];
        key.copy_from_slice(&digest[..16]);
        StillKey(key)
    }
}

/// A file as the key sees it: its length and modification time, or that it is not there.
pub(crate) fn file_stamp(path: &str) -> Vec<u8> {
    // A title's picture is named by its content, so the name is the whole stamp (and it may not be
    // drawn yet when a key is asked for, which a `stat` would call a different file).
    if super::svg_layer::in_cache(path) {
        return b"svg".to_vec();
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return b"missing".to_vec();
    };
    let nanos = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let mut stamp = meta.len().to_le_bytes().to_vec();
    stamp.extend_from_slice(&nanos.to_le_bytes());
    stamp
}

struct Entry {
    frame: Arc<Vec<u8>>,
    tick: u64,
}

/// What the cache has done, for logs and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StillCacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub hits: u64,
    pub misses: u64,
    pub inserted: u64,
    pub evicted: u64,
}

/// The bounded store. `get` and `insert` are O(log n).
pub(crate) struct StillCache {
    map: HashMap<StillKey, Entry>,
    /// Recency: the smallest tick is the next to go.
    order: BTreeMap<u64, StillKey>,
    tick: u64,
    bytes: usize,
    max_bytes: usize,
    max_entries: usize,
    stats: StillCacheStats,
}

impl StillCache {
    pub(crate) fn new(max_bytes: usize, max_entries: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: BTreeMap::new(),
            tick: 0,
            bytes: 0,
            max_bytes,
            max_entries: max_entries.max(1),
            stats: StillCacheStats::default(),
        }
    }

    /// The frame under `key`, counted as used.
    pub(crate) fn get(&mut self, key: StillKey) -> Option<Arc<Vec<u8>>> {
        match self.touch(key) {
            Some(frame) => {
                self.stats.hits += 1;
                Some(frame)
            }
            None => {
                self.stats.misses += 1;
                None
            }
        }
    }

    /// Mark `key` the most recently used.
    fn touch(&mut self, key: StillKey) -> Option<Arc<Vec<u8>>> {
        let entry = self.map.get_mut(&key)?;
        self.tick += 1;
        self.order.remove(&entry.tick);
        entry.tick = self.tick;
        self.order.insert(self.tick, key);
        Some(Arc::clone(&entry.frame))
    }

    /// Whether `key` is held, without counting it as used.
    #[cfg(test)]
    pub(crate) fn contains(&self, key: StillKey) -> bool {
        self.map.contains_key(&key)
    }

    /// Keep `frame` under `key`. A key already held **keeps the frame it has** (the picture on
    /// show for a moment does not change under whoever is looking at it) and is counted as used. A
    /// frame bigger than a quarter of the byte budget is not kept: one such frame would turn the
    /// cache over. Returns whether the frame is held afterwards.
    pub(crate) fn insert(&mut self, key: StillKey, frame: Vec<u8>) -> bool {
        if self.touch(key).is_some() {
            return true;
        }
        if frame.is_empty() || frame.len() > self.max_bytes / 4 {
            return false;
        }
        self.tick += 1;
        self.bytes += frame.len();
        self.order.insert(self.tick, key);
        self.map.insert(
            key,
            Entry {
                frame: Arc::new(frame),
                tick: self.tick,
            },
        );
        self.stats.inserted += 1;
        while self.map.len() > self.max_entries || self.bytes > self.max_bytes {
            let Some((&oldest, _)) = self.order.iter().next() else { break };
            let Some(victim) = self.order.remove(&oldest) else { break };
            if let Some(gone) = self.map.remove(&victim) {
                self.bytes -= gone.frame.len();
                self.stats.evicted += 1;
            }
        }
        true
    }

    pub(crate) fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.bytes = 0;
    }

    pub(crate) fn stats(&self) -> StillCacheStats {
        StillCacheStats {
            entries: self.map.len(),
            bytes: self.bytes,
            ..self.stats
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> StillKey {
        StillKey::of(&[format!("frame-{n}")], |_| Vec::new())
    }

    fn jpeg(n: usize) -> Vec<u8> {
        vec![0xAB; n]
    }

    #[test]
    fn a_key_is_the_argv_and_the_files_behind_it() {
        let args: Vec<String> = ["-ss", "1.000000", "-i", "/a.mp4", "-frames:v", "1"].map(String::from).into();
        let same = StillKey::of(&args, |p| p.as_bytes().to_vec());
        assert_eq!(same, StillKey::of(&args, |p| p.as_bytes().to_vec()));

        let mut later = args.clone();
        later[1] = "1.033333".to_string();
        assert_ne!(
            same,
            StillKey::of(&later, |p| p.as_bytes().to_vec()),
            "another time is another frame"
        );

        let mut other_file = args.clone();
        other_file[3] = "/b.mp4".to_string();
        assert_ne!(
            same,
            StillKey::of(&other_file, |p| p.as_bytes().to_vec()),
            "another input is another frame"
        );

        assert_ne!(
            same,
            StillKey::of(&args, |p| format!("{p}-replaced").into_bytes()),
            "the same name with other contents is another frame"
        );
    }

    #[test]
    fn two_argvs_that_join_to_the_same_text_have_different_keys() {
        let one: Vec<String> = ["a", "bc"].map(String::from).into();
        let other: Vec<String> = ["ab", "c"].map(String::from).into();
        assert_ne!(StillKey::of(&one, |_| Vec::new()), StillKey::of(&other, |_| Vec::new()));
    }

    #[test]
    fn a_stamp_follows_the_file() {
        let dir = std::env::temp_dir().join(format!("kerf-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.bin");
        std::fs::write(&path, b"one").unwrap();
        let p = path.to_string_lossy().into_owned();
        let before = file_stamp(&p);
        assert_eq!(before, file_stamp(&p));
        std::fs::write(&path, b"three").unwrap();
        assert_ne!(before, file_stamp(&p), "a rewritten file reads differently");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(file_stamp(&p), b"missing".to_vec());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn it_returns_what_it_was_given() {
        let mut cache = StillCache::new(1 << 20, 16);
        assert!(cache.get(key(1)).is_none());
        assert!(cache.insert(key(1), jpeg(10)));
        assert_eq!(*cache.get(key(1)).unwrap(), jpeg(10));
        assert!(cache.contains(key(1)));
        assert!(!cache.contains(key(2)));
        let stats = cache.stats();
        assert_eq!((stats.entries, stats.bytes, stats.hits, stats.misses), (1, 10, 1, 1));
    }

    #[test]
    fn the_least_recently_used_frame_goes_first() {
        let mut cache = StillCache::new(1 << 20, 3);
        for n in 1..=3 {
            cache.insert(key(n), jpeg(10));
        }
        // 1 is the oldest, until it is looked at.
        let _ = cache.get(key(1));
        cache.insert(key(4), jpeg(10));
        assert!(cache.contains(key(1)), "a frame just used stays");
        assert!(!cache.contains(key(2)), "the one used longest ago went");
        assert!(cache.contains(key(3)) && cache.contains(key(4)));
        assert_eq!(cache.stats().entries, 3);
        assert_eq!(cache.stats().evicted, 1);
    }

    #[test]
    fn bytes_are_bounded_as_well_as_entries() {
        let mut cache = StillCache::new(1000, 1000);
        for n in 0..40u8 {
            cache.insert(key(n), jpeg(200));
            assert!(cache.stats().bytes <= 1000, "the byte budget holds after every insert");
        }
        // 200-byte frames, a 1000-byte budget: five fit.
        assert_eq!(cache.stats().entries, 5);
        assert!(cache.contains(key(39)) && !cache.contains(key(0)));
    }

    #[test]
    fn a_frame_that_would_turn_the_cache_over_is_not_kept() {
        let mut cache = StillCache::new(1000, 10);
        cache.insert(key(1), jpeg(100));
        assert!(!cache.insert(key(2), jpeg(251)), "more than a quarter of the budget");
        assert!(cache.insert(key(3), jpeg(250)));
        assert!(!cache.insert(key(4), Vec::new()), "nothing is not a frame");
        assert!(cache.contains(key(1)), "and the refused one pushed nothing out");
    }

    #[test]
    fn a_key_already_held_keeps_its_first_frame() {
        let mut cache = StillCache::new(1 << 20, 4);
        cache.insert(key(1), vec![1, 1, 1]);
        assert!(cache.insert(key(1), vec![2, 2, 2, 2]));
        assert_eq!(
            *cache.get(key(1)).unwrap(),
            vec![1, 1, 1],
            "the picture on show does not change"
        );
        assert_eq!(cache.stats().bytes, 3);
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut cache = StillCache::new(1 << 20, 4);
        cache.insert(key(1), jpeg(10));
        cache.clear();
        assert!(!cache.contains(key(1)));
        assert_eq!((cache.stats().entries, cache.stats().bytes), (0, 0));
        assert!(cache.insert(key(1), jpeg(10)));
    }

    #[test]
    fn a_frame_handed_out_outlives_its_eviction() {
        let mut cache = StillCache::new(1 << 20, 1);
        cache.insert(key(1), jpeg(10));
        let held = cache.get(key(1)).unwrap();
        cache.insert(key(2), jpeg(10));
        assert!(!cache.contains(key(1)));
        assert_eq!(held.len(), 10);
    }
}
