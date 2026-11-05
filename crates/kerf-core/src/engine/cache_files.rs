//! Which files of the disk cache an asset's work lives in.
//!
//! Every cache keys its file on the original's identity (path, size, modified time), so the names
//! can be worked out from the asset alone: a stat of the original and a hash, no probe and no
//! decode. The cache cap uses it to keep what the open project is using.

use std::path::{Path, PathBuf};

use super::{cli, filmstrip, music, peaks, stems};
use crate::model::Asset;

/// Every cache file or directory `asset` reads or writes, whether or not it exists: its proxy (in
/// each spelling) and the proxy's sidecar, its waveform pyramid, filmstrip, stems and music analysis.
pub(crate) fn asset_cache_files(asset: &Asset) -> Vec<PathBuf> {
    let src = Path::new(&asset.path);
    let mut files = cli::proxy_cache_files(src, cli::proxy_width(asset.projection()));
    files.extend(peaks::cache_path(src));
    files.extend(music::cache_path(src));
    files.extend(stems::stems_dir(src));
    files.extend(filmstrip::cached_entry_dir(asset));
    files
}
