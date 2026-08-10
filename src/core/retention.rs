use std::num::NonZeroU16;

use crate::core::{Wallpaper, WallpaperId};

pub(crate) fn history_removal_candidates<'a>(
    wallpapers: impl Iterator<Item = (&'a WallpaperId, &'a Wallpaper)>,
    retain_id: Option<&WallpaperId>,
    max_history_wallpapers: Option<NonZeroU16>,
) -> Vec<WallpaperId> {
    let Some(max_history_wallpapers) = max_history_wallpapers else {
        return Vec::new();
    };

    let max_history_wallpapers = usize::from(max_history_wallpapers.get());
    let mut history_ids = wallpapers
        .filter(|(id, wallpaper)| retain_id != Some(*id) && !wallpaper.is_favorite())
        .map(|(id, wallpaper)| (id.clone(), wallpaper.seen_at(), wallpaper.discovered_at))
        .collect::<Vec<_>>();

    if history_ids.len() <= max_history_wallpapers {
        return Vec::new();
    }

    history_ids.sort_by_key(|(id, seen_at, discovered_at)| (*seen_at, *discovered_at, id.clone()));

    let remove_count = history_ids.len() - max_history_wallpapers;
    history_ids
        .into_iter()
        .take(remove_count)
        .map(|(id, _, _)| id)
        .collect()
}
