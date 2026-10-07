//! What decoding an asset yields: [`SourceMedia`], and the resolvers that decide which file.
//!
//! The preview decodes a **proxy** (a smaller, all-intra, `yuv420p` file) where one is ready,
//! and the export always decodes the original. `Project::preview_assets` swaps the path and
//! keeps the original's [`StreamInfo`], which is right for the filter graph (it scales
//! whatever frame arrives) and wrong for a renderer that has to know the size and format
//! of the picture it was handed. A plan therefore asks a [`MediaResolver`] which file is
//! decoded and what is in it:
//!
//! * [`OriginalMedia`] — the asset's own file and the stream the probe recorded. What every plan
//!   did before, and what an export reads.
//! * [`ProxyMedia`] — the ready proxy and **its** stream, read from the sidecar `generate_proxy`
//!   wrote beside it (see `engine::proxy_video_info`), else the original.
//!
//! The delivery canvas still derives from the originals' streams (`render_geometry`): a proxy
//! changes which pixels are decoded, never the frame the cut is made for.

use std::path::Path;

use crate::engine::proxy_video_info;
use crate::model::{Asset, StreamInfo, StreamKind};
use crate::project::Project;

/// The file a decoder opens for an asset and the picture it yields.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SourceMedia {
    /// The file to open.
    pub path: String,
    /// `path` is a generated preview proxy, not the asset's own file.
    pub proxy: bool,
    /// The first video stream **of that file** — its displayed size, pixel format and colour
    /// tags. `None` when the asset has no video.
    pub video: Option<StreamInfo>,
}

impl SourceMedia {
    /// The asset's own file, described by the stream its probe recorded.
    pub fn original(asset: &Asset) -> Self {
        Self {
            path: asset.path.clone(),
            proxy: false,
            video: asset.streams.iter().find(|s| s.kind == StreamKind::Video).cloned(),
        }
    }

    /// `asset` as decoding this media sees it: the path swapped and the video stream replaced
    /// by the decoded file's own. Everything else (id, duration, audio) is the asset's.
    pub(crate) fn decoded(&self, asset: &Asset) -> Asset {
        let mut decoded = asset.clone();
        decoded.path.clone_from(&self.path);
        if let (Some(video), Some(slot)) = (&self.video, decoded.streams.iter_mut().find(|s| s.kind == StreamKind::Video)) {
            // A proxy file has no spherical metadata of its own (the CLI writes no `sv3d` box), but
            // it holds the same projection, only smaller: that is the original's to say.
            let projection = slot.projection;
            *slot = video.clone();
            slot.projection = slot.projection.or(projection);
        }
        decoded
    }
}

/// Which file a plan's layers are decoded from.
pub trait MediaResolver: Send + Sync + std::fmt::Debug {
    /// The media decoded for `asset`. May read the disk (a sidecar, a probe): resolve off the
    /// project lock, once per plan.
    fn resolve(&self, asset: &Asset) -> SourceMedia;
}

/// Every asset decodes from its own file.
#[derive(Debug, Clone, Copy, Default)]
pub struct OriginalMedia;

impl MediaResolver for OriginalMedia {
    fn resolve(&self, asset: &Asset) -> SourceMedia {
        SourceMedia::original(asset)
    }
}

/// An asset decodes from its ready proxy, as the preview does ([`Project::preview_source`]:
/// not for a still or an audio-only asset, and not until one has been generated), and the
/// plan describes **that file's** stream.
///
/// The stream is read from the proxy's sidecar. A proxy without one (made before sidecars
/// existed) costs one `ffprobe`, remembered and written back as the sidecar; if even that
/// fails the asset decodes from its original, which is always correct and only slower.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProxyMedia;

impl MediaResolver for ProxyMedia {
    fn resolve(&self, asset: &Asset) -> SourceMedia {
        let source = Project::preview_source(asset);
        if source == Path::new(&asset.path) {
            return SourceMedia::original(asset);
        }
        match proxy_video_info(&source) {
            Some(video) => SourceMedia {
                path: source.to_string_lossy().into_owned(),
                proxy: true,
                video: Some(video),
            },
            None => SourceMedia::original(asset),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{av_asset, img_asset};
    use uuid::Uuid;

    #[test]
    fn the_original_is_the_asset_and_a_decoded_asset_wears_the_files_own_stream() {
        let asset = av_asset(Uuid::new_v4(), 10.0);
        let media = OriginalMedia.resolve(&asset);
        assert_eq!(media.path, asset.path);
        assert!(!media.proxy);
        assert_eq!(media.video, asset.streams.first().cloned());
        let same = media.decoded(&asset);
        assert_eq!((same.path.as_str(), same.streams.len()), (asset.path.as_str(), 2));

        // A proxy: another path, another size, and the audio stream left alone.
        let mut spherical = asset.clone();
        spherical.streams[0].projection = Some(crate::model::Projection::Equirect);
        let mut video = asset.streams[0].clone();
        video.width = Some(1280);
        video.height = Some(720);
        let proxy = SourceMedia {
            path: "/cache/kerf/proxies/0000000000000abc.mp4".into(),
            proxy: true,
            video: Some(video),
        };
        let decoded = proxy.decoded(&asset);
        assert_eq!(decoded.path, "/cache/kerf/proxies/0000000000000abc.mp4");
        assert_eq!((decoded.streams[0].width, decoded.streams[0].height), (Some(1280), Some(720)));
        assert_eq!(decoded.streams[1], asset.streams[1]);
        // The proxy's own probe has no projection, the asset's still is the picture's.
        assert_eq!(decoded.projection(), None);
        let kept = proxy.decoded(&spherical);
        assert_eq!(kept.projection(), Some(crate::model::Projection::Equirect));
        assert_eq!((kept.streams[0].width, kept.streams[0].height), (Some(1280), Some(720)));
        assert_eq!((decoded.id, decoded.duration), (asset.id, asset.duration));
    }

    #[test]
    fn a_still_and_an_asset_without_a_proxy_resolve_to_their_original() {
        // No proxy exists for a path that was never imported, and stills never get one.
        for asset in [av_asset(Uuid::new_v4(), 10.0), img_asset(Uuid::new_v4())] {
            let media = ProxyMedia.resolve(&asset);
            assert_eq!((media.path.as_str(), media.proxy), (asset.path.as_str(), false));
        }
    }
}
