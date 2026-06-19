use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("asset not found: {0}")]
    AssetNotFound(uuid::Uuid),

    #[error("clip not found: {0}")]
    ClipNotFound(uuid::Uuid),

    #[error("track not found: {0}")]
    TrackNotFound(uuid::Uuid),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("FFmpeg support is not enabled in this build (enable the `ffmpeg` feature)")]
    FfmpegDisabled,

    #[cfg(feature = "ffmpeg")]
    #[error("ffmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg_next::Error),

    #[error("{0}")]
    Other(String),
}
