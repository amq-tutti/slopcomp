use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ffms2: {0}")]
    Ffms(String),
    #[error("zimg: {0}")]
    Zimg(String),
    #[error("unsupported pixel format {name} ({id})")]
    UnsupportedPixelFormat { id: i32, name: String },
    #[error("frame {frame} out of range (source has {count} frames)")]
    FrameOutOfRange { frame: i64, count: i32 },
    #[error("no video track in {0}")]
    NoVideoTrack(PathBuf),
    #[error("path is not valid UTF-8 / contains NUL: {0}")]
    BadPath(PathBuf),
    #[error("{0}")]
    Spec(String),
    #[error("png: {0}")]
    Png(#[from] png::EncodingError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
