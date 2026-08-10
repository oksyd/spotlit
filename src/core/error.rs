use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, SpotlitError>;

#[derive(Debug, Error)]
pub enum SpotlitError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse JSON at {path}: {source}")]
    JsonRead {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to write JSON at {path}: {source}")]
    JsonWrite {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("image error at {path}: {source}")]
    Image {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },

    #[error("wallpaper {0} was not found")]
    WallpaperNotFound(String),

    #[error("no wallpaper has been discovered yet")]
    NoWallpaperAvailable,

    #[error("platform operation failed: {0}")]
    Platform(String),
}

impl SpotlitError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub fn image(path: impl Into<PathBuf>, source: image::ImageError) -> Self {
        Self::Image {
            path: path.into(),
            source,
        }
    }

    pub fn platform(message: impl std::fmt::Display) -> Self {
        Self::Platform(message.to_string())
    }
}
