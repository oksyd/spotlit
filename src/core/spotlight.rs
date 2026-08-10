use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use image::ImageReader;
use sha2::{Digest, Sha256};

use crate::core::{Result, SpotlitError, WallpaperId, ensure_thumbnail};

const MIN_WALLPAPER_FILE_SIZE: u64 = 80_000;
const MIN_WALLPAPER_WIDTH: u32 = 1024;
const MIN_WALLPAPER_HEIGHT: u32 = 576;
const ID_HASH_PREFIX_LEN: usize = 16;

#[derive(Debug, Clone)]
pub struct ScannedWallpaper {
    pub id: WallpaperId,
    pub source_path: PathBuf,
    pub cached_path: PathBuf,
    pub thumbnail_path: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
}

#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub inserted: usize,
    pub updated: usize,
}

#[derive(Debug, Clone)]
pub struct SpotlightScanner {
    sources: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ThumbnailPolicy {
    Generate,
    Defer,
}

impl SpotlightScanner {
    pub fn new(sources: Vec<PathBuf>) -> Self {
        Self { sources }
    }

    pub fn scan_into_cache(
        &self,
        cache_dir: &Path,
        thumbnail_dir: &Path,
    ) -> Result<Vec<ScannedWallpaper>> {
        self.scan_into_cache_with_thumbnail_policy(
            cache_dir,
            thumbnail_dir,
            ThumbnailPolicy::Generate,
        )
    }

    pub fn scan_into_cache_deferred_thumbnails(
        &self,
        cache_dir: &Path,
        thumbnail_dir: &Path,
    ) -> Result<Vec<ScannedWallpaper>> {
        self.scan_into_cache_with_thumbnail_policy(cache_dir, thumbnail_dir, ThumbnailPolicy::Defer)
    }

    fn scan_into_cache_with_thumbnail_policy(
        &self,
        cache_dir: &Path,
        thumbnail_dir: &Path,
        thumbnail_policy: ThumbnailPolicy,
    ) -> Result<Vec<ScannedWallpaper>> {
        ensure_cache_dirs(cache_dir, thumbnail_dir)?;

        let mut wallpapers = Vec::new();
        for source in &self.sources {
            if !source.exists() {
                tracing::debug!(path = %source.display(), "spotlight source does not exist");
                continue;
            }

            scan_dir(
                source,
                cache_dir,
                thumbnail_dir,
                thumbnail_policy,
                &mut wallpapers,
            )?;
        }

        Ok(wallpapers)
    }

    pub fn scan_file_into_cache(
        &self,
        path: &Path,
        cache_dir: &Path,
        thumbnail_dir: &Path,
    ) -> Result<Option<ScannedWallpaper>> {
        self.scan_file_into_cache_with_thumbnail_policy(
            path,
            cache_dir,
            thumbnail_dir,
            ThumbnailPolicy::Generate,
        )
    }

    pub fn scan_file_into_cache_deferred_thumbnail(
        &self,
        path: &Path,
        cache_dir: &Path,
        thumbnail_dir: &Path,
    ) -> Result<Option<ScannedWallpaper>> {
        self.scan_file_into_cache_with_thumbnail_policy(
            path,
            cache_dir,
            thumbnail_dir,
            ThumbnailPolicy::Defer,
        )
    }

    fn scan_file_into_cache_with_thumbnail_policy(
        &self,
        path: &Path,
        cache_dir: &Path,
        thumbnail_dir: &Path,
        thumbnail_policy: ThumbnailPolicy,
    ) -> Result<Option<ScannedWallpaper>> {
        ensure_cache_dirs(cache_dir, thumbnail_dir)?;

        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(SpotlitError::io(path, source)),
        };

        if !is_candidate_file(&metadata) {
            return Ok(None);
        }

        inspect_candidate(path, cache_dir, thumbnail_dir, thumbnail_policy)
    }
}

fn scan_dir(
    dir: &Path,
    cache_dir: &Path,
    thumbnail_dir: &Path,
    thumbnail_policy: ThumbnailPolicy,
    wallpapers: &mut Vec<ScannedWallpaper>,
) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) => {
            tracing::warn!(path = %dir.display(), error = %source, "failed to read spotlight source");
            return Ok(());
        }
    };

    for entry in entries {
        let entry = entry.map_err(|source| SpotlitError::io(dir, source))?;
        let path = entry.path();
        let metadata = entry
            .metadata()
            .map_err(|source| SpotlitError::io(&path, source))?;

        if metadata.is_dir() {
            scan_dir(
                &path,
                cache_dir,
                thumbnail_dir,
                thumbnail_policy,
                wallpapers,
            )?;
            continue;
        }

        if !is_candidate_file(&metadata) {
            continue;
        }

        match inspect_candidate(&path, cache_dir, thumbnail_dir, thumbnail_policy) {
            Ok(Some(wallpaper)) => wallpapers.push(wallpaper),
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(path = %path.display(), error = %error, "skipping image candidate")
            }
        }
    }

    Ok(())
}

fn inspect_candidate(
    path: &Path,
    cache_dir: &Path,
    thumbnail_dir: &Path,
    thumbnail_policy: ThumbnailPolicy,
) -> Result<Option<ScannedWallpaper>> {
    let reader = ImageReader::open(path)
        .map_err(|source| SpotlitError::io(path, source))?
        .with_guessed_format()
        .map_err(|source| SpotlitError::io(path, source))?;

    let Some(format) = reader.format() else {
        return Ok(None);
    };

    let (width, height) = reader
        .into_dimensions()
        .map_err(|source| SpotlitError::image(path, source))?;

    if !is_landscape_wallpaper(width, height) {
        return Ok(None);
    }

    let sha256 = hash_file(path)?;
    let id = WallpaperId::new(sha256.chars().take(ID_HASH_PREFIX_LEN).collect::<String>());
    let extension = format.extensions_str().first().copied().unwrap_or("jpg");
    let cached_path = cache_dir.join(format!("{}.{}", id.as_str(), extension));

    if !cached_path.exists() {
        fs::copy(path, &cached_path).map_err(|source| SpotlitError::io(&cached_path, source))?;
    }

    let thumbnail_path = match thumbnail_policy {
        ThumbnailPolicy::Generate => Some(ensure_thumbnail(&cached_path, thumbnail_dir)?),
        ThumbnailPolicy::Defer => None,
    };

    Ok(Some(ScannedWallpaper {
        id,
        source_path: path.to_path_buf(),
        cached_path,
        thumbnail_path,
        width,
        height,
        sha256,
    }))
}

fn ensure_cache_dirs(cache_dir: &Path, thumbnail_dir: &Path) -> Result<()> {
    fs::create_dir_all(cache_dir).map_err(|source| SpotlitError::io(cache_dir, source))?;
    fs::create_dir_all(thumbnail_dir).map_err(|source| SpotlitError::io(thumbnail_dir, source))?;
    Ok(())
}

fn is_candidate_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file() && metadata.len() >= MIN_WALLPAPER_FILE_SIZE
}

fn is_landscape_wallpaper(width: u32, height: u32) -> bool {
    width >= MIN_WALLPAPER_WIDTH && height >= MIN_WALLPAPER_HEIGHT && width > height
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|source| SpotlitError::io(path, source))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| SpotlitError::io(path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(bytes_to_lower_hex(hasher.finalize().as_ref()))
}

fn bytes_to_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[(byte >> 4) as usize]));
        encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    encoded
}
