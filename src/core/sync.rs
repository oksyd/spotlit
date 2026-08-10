use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::core::WallpaperId;

#[derive(Debug, Clone)]
pub struct SyncReport {
    pub id: WallpaperId,
    pub image_path: PathBuf,
    pub synced_at: DateTime<Utc>,
}
