use std::{
    collections::BTreeSet,
    fs,
    ops::Deref,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use image::{Rgb, RgbImage};

use crate::core::{
    AppPaths, SpotlightMetadata, SpotlitCore, WallpaperId, WallpaperLibrary, WallpaperSource,
    normalized_history_limit,
};

#[test]
fn favoriting_copies_and_unfavoriting_removes_wallpaper_file()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("favorite-copy");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 0)?;
    assert!(fs::metadata(&source_image)?.len() > 80_000);

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, vec![source_dir])?;
    core.scan_spotlight_wallpapers()?;

    let wallpaper = core.current_wallpaper().expect("wallpaper was scanned");
    core.set_favorite(&wallpaper.id, true)?;

    let favorite = core
        .list_favorites()
        .into_iter()
        .next()
        .expect("favorite was saved");
    let favorite_path = favorite.favorite_path.expect("favorite path was recorded");
    assert!(favorite_path.exists());

    core.set_favorite(&wallpaper.id, false)?;
    assert!(!favorite_path.exists());
    assert!(core.list_favorites().is_empty());

    Ok(())
}

#[test]
fn sync_enforces_history_limit() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("history-limit-sync");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let first_image = source_dir.join("first.png");
    let second_image = source_dir.join("second.png");
    let third_image = source_dir.join("third.png");
    write_test_wallpaper(&first_image, 1)?;
    write_test_wallpaper(&second_image, 2)?;
    write_test_wallpaper(&third_image, 3)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let first = core
        .import_wallpaper_file(&first_image)?
        .expect("first wallpaper was imported");
    let second = core
        .import_wallpaper_file(&second_image)?
        .expect("second wallpaper was imported");
    let third = core
        .import_wallpaper_file(&third_image)?
        .expect("third wallpaper was imported");

    let mut config = core.config().clone();
    config.max_history_wallpapers = Some(normalized_history_limit(1));
    core.update_config(config)?;

    let candidate = core.wallpaper_for_sync(Some(&third_image))?;
    assert_eq!(candidate.id, third.id);
    core.record_lock_screen_sync(&candidate.id)?;

    let wallpapers = core.list_wallpapers();
    assert_eq!(wallpapers.len(), 2);
    assert!(wallpapers.iter().any(|wallpaper| wallpaper.id == second.id));
    assert!(wallpapers.iter().any(|wallpaper| wallpaper.id == third.id));
    assert!(!first.cached_path.exists());
    assert!(second.cached_path.exists());
    assert!(third.cached_path.exists());

    Ok(())
}

#[test]
fn sync_by_id_records_requested_wallpaper() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("sync-by-id");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let first_image = source_dir.join("first.png");
    let second_image = source_dir.join("second.png");
    write_test_wallpaper(&first_image, 3)?;
    write_test_wallpaper(&second_image, 4)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let first = core
        .import_wallpaper_file(&first_image)?
        .expect("first wallpaper was imported");
    let second = core
        .import_wallpaper_file(&second_image)?
        .expect("second wallpaper was imported");
    let candidate = core.wallpaper_for_sync_by_id(&first.id)?;
    let report = core.record_lock_screen_sync(&candidate.id)?;

    assert_eq!(report.image_path, first.cached_path);
    assert!(
        core.wallpaper(&first.id)
            .expect("first wallpaper remains")
            .last_synced_at
            .is_some()
    );
    assert!(
        core.wallpaper(&second.id)
            .expect("second wallpaper remains")
            .last_synced_at
            .is_none()
    );

    Ok(())
}

#[test]
fn lock_screen_image_preparation_uses_unique_bounded_staging_files()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("lock-screen-staging");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 19)?;

    let paths = AppPaths::new(root.join("data"));
    let lock_screen_dir = paths.lock_screen_dir.clone();
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file(&source_image)?
        .expect("wallpaper was imported");

    let mut staged_paths = BTreeSet::new();
    for _ in 0..6 {
        staged_paths.insert(core.prepare_lock_screen_image(&wallpaper)?);
    }

    assert_eq!(staged_paths.len(), 6);
    assert!(
        staged_paths
            .iter()
            .all(|path| path != &wallpaper.cached_path)
    );
    assert!(
        staged_paths
            .iter()
            .all(|path| path.parent() == Some(lock_screen_dir.as_path()))
    );
    assert_eq!(fs::read_dir(lock_screen_dir)?.count(), 4);

    Ok(())
}

#[test]
fn lock_screen_rotation_prefers_unsynced_wallpapers()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("lock-screen-rotation");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let first_image = source_dir.join("first.png");
    let second_image = source_dir.join("second.png");
    write_test_wallpaper(&first_image, 21)?;
    write_test_wallpaper(&second_image, 22)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    core.import_wallpaper_file(&first_image)?
        .expect("first wallpaper was imported");
    core.import_wallpaper_file(&second_image)?
        .expect("second wallpaper was imported");

    let first_candidate = core
        .wallpaper_rotation_candidate(WallpaperSource::RandomLibrary)
        .expect("first candidate exists");
    core.record_lock_screen_sync(&first_candidate.id)?;

    let second_candidate = core
        .wallpaper_rotation_candidate(WallpaperSource::RandomLibrary)
        .expect("second candidate exists");

    assert_ne!(first_candidate.id, second_candidate.id);
    assert!(
        core.latest_wallpaper_rotation_sync_at(WallpaperSource::RandomLibrary)
            .is_some()
    );

    Ok(())
}

#[test]
fn lock_screen_favorite_rotation_uses_only_favorites()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("lock-screen-favorite-rotation");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let first_image = source_dir.join("first.png");
    let second_image = source_dir.join("second.png");
    write_test_wallpaper(&first_image, 23)?;
    write_test_wallpaper(&second_image, 24)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let favorite = core
        .import_wallpaper_file(&first_image)?
        .expect("favorite wallpaper was imported");
    let other = core
        .import_wallpaper_file(&second_image)?
        .expect("other wallpaper was imported");
    core.set_favorite(&favorite.id, true)?;

    let candidate = core
        .wallpaper_rotation_candidate(WallpaperSource::RandomFavorites)
        .expect("favorite candidate exists");

    assert_eq!(candidate.id, favorite.id);
    assert_ne!(candidate.id, other.id);

    Ok(())
}

#[test]
fn importing_existing_wallpaper_marks_it_as_current_candidate()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("import-existing-current");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let first_image = source_dir.join("first.png");
    let second_image = source_dir.join("second.png");
    write_test_wallpaper(&first_image, 6)?;
    write_test_wallpaper(&second_image, 7)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let first = core
        .import_wallpaper_file(&first_image)?
        .expect("first wallpaper was imported");
    let second = core
        .import_wallpaper_file(&second_image)?
        .expect("second wallpaper was imported");
    assert_eq!(
        core.current_wallpaper()
            .expect("current wallpaper exists")
            .id,
        second.id
    );

    core.import_wallpaper_file(&first_image)?;

    let current = core.current_wallpaper().expect("current wallpaper exists");
    assert_eq!(current.id, first.id);
    assert!(current.seen_at() >= second.seen_at());

    Ok(())
}

#[test]
fn maintenance_removes_wallpapers_with_missing_cached_files()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("maintain-missing-cache");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 5)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file(&source_image)?
        .expect("wallpaper was imported");
    fs::remove_file(&wallpaper.cached_path)?;

    let report = core.maintain_library()?;

    assert_eq!(report.removed_missing_wallpapers, 1);
    assert!(core.list_wallpapers().is_empty());

    Ok(())
}

#[test]
fn maintenance_regenerates_missing_thumbnails()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("maintain-thumbnail");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 12)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file(&source_image)?
        .expect("wallpaper was imported");
    let original_thumbnail = wallpaper.thumbnail_path.expect("thumbnail exists");
    fs::remove_file(&original_thumbnail)?;

    let report = core.maintain_library()?;
    let maintained = core
        .wallpaper(&wallpaper.id)
        .expect("wallpaper remains after maintenance");
    let regenerated_thumbnail = maintained.thumbnail_path.expect("thumbnail was restored");

    assert_eq!(report.cleared_missing_thumbnails, 1);
    assert_eq!(report.regenerated_thumbnails, 1);
    assert!(regenerated_thumbnail.exists());

    Ok(())
}

#[test]
fn deferred_scan_does_not_generate_thumbnail_files()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("deferred-scan-thumbnail");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 17)?;

    let paths = AppPaths::new(root.join("data"));
    let thumbnail_dir = paths.thumbnail_dir.clone();
    let mut core = SpotlitCore::open(paths, vec![source_dir])?;

    let report = core.scan_spotlight_wallpapers_deferred_thumbnails()?;
    let wallpaper = core.current_wallpaper().expect("wallpaper was scanned");

    assert_eq!(report.inserted, 1);
    assert!(wallpaper.thumbnail_path.is_none());
    assert_eq!(fs::read_dir(thumbnail_dir)?.count(), 0);

    Ok(())
}

#[test]
fn thumbnail_warm_cache_respects_batch_limit() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let root = temp_root("thumbnail-warm-limit");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    for index in 0..3 {
        write_test_wallpaper(
            &source_dir.join(format!("wallpaper-{index}.png")),
            30 + index,
        )?;
    }

    let paths = AppPaths::new(root.join("data"));
    let thumbnail_dir = paths.thumbnail_dir.clone();
    let mut core = SpotlitCore::open(paths, vec![source_dir])?;

    core.scan_spotlight_wallpapers_deferred_thumbnails()?;
    assert_eq!(
        core.list_wallpapers()
            .iter()
            .filter(|wallpaper| wallpaper.thumbnail_path.is_some())
            .count(),
        0
    );

    let report = core.warm_thumbnail_cache(2)?;
    let warmed = core
        .list_wallpapers()
        .into_iter()
        .filter(|wallpaper| {
            wallpaper
                .thumbnail_path
                .as_ref()
                .is_some_and(|path| path.exists())
        })
        .count();

    assert_eq!(report.regenerated_thumbnails, 2);
    assert_eq!(warmed, 2);
    assert_eq!(fs::read_dir(thumbnail_dir)?.count(), 2);

    Ok(())
}

#[test]
fn thumbnail_warm_cache_zero_limit_is_noop() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let root = temp_root("thumbnail-warm-zero");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 34)?;

    let paths = AppPaths::new(root.join("data"));
    let thumbnail_dir = paths.thumbnail_dir.clone();
    let mut core = SpotlitCore::open(paths, vec![source_dir])?;

    core.scan_spotlight_wallpapers_deferred_thumbnails()?;
    let report = core.warm_thumbnail_cache(0)?;

    assert_eq!(report.regenerated_thumbnails, 0);
    assert_eq!(fs::read_dir(thumbnail_dir)?.count(), 0);
    assert!(
        core.current_wallpaper()
            .expect("wallpaper was scanned")
            .thumbnail_path
            .is_none()
    );

    Ok(())
}

#[test]
fn lightweight_maintenance_does_not_regenerate_missing_thumbnails()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("lightweight-maintain-thumbnail");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 18)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file(&source_image)?
        .expect("wallpaper was imported");
    let original_thumbnail = wallpaper.thumbnail_path.expect("thumbnail exists");
    fs::remove_file(&original_thumbnail)?;

    let report = core.maintain_library_lightweight()?;
    let maintained = core
        .wallpaper(&wallpaper.id)
        .expect("wallpaper remains after maintenance");

    assert_eq!(report.cleared_missing_thumbnails, 1);
    assert_eq!(report.regenerated_thumbnails, 0);
    assert!(maintained.thumbnail_path.is_none());
    assert!(!original_thumbnail.exists());

    Ok(())
}

#[test]
fn trim_unretained_cache_keeps_current_and_favorites()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("trim-manual-cache");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let favorite_image = source_dir.join("favorite.png");
    let removable_image = source_dir.join("removable.png");
    let current_image = source_dir.join("current.png");
    write_test_wallpaper(&favorite_image, 8)?;
    write_test_wallpaper(&removable_image, 9)?;
    write_test_wallpaper(&current_image, 10)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let favorite = core
        .import_wallpaper_file(&favorite_image)?
        .expect("favorite wallpaper was imported");
    let removable = core
        .import_wallpaper_file(&removable_image)?
        .expect("removable wallpaper was imported");
    let current = core
        .import_wallpaper_file(&current_image)?
        .expect("current wallpaper was imported");
    core.set_favorite(&favorite.id, true)?;

    let report = core.trim_unretained_cache()?;

    assert_eq!(report.removed_unretained_wallpapers, 1);
    assert!(favorite.cached_path.exists());
    assert!(!removable.cached_path.exists());
    assert!(current.cached_path.exists());
    assert!(core.wallpaper(&favorite.id).is_some());
    assert!(core.wallpaper(&removable.id).is_none());
    assert!(core.wallpaper(&current.id).is_some());

    Ok(())
}

#[test]
fn removing_wallpaper_deletes_cached_thumbnail_and_favorite_files()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("remove-wallpaper");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 11)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file(&source_image)?
        .expect("wallpaper was imported");
    core.set_favorite(&wallpaper.id, true)?;

    let favorite = core
        .wallpaper(&wallpaper.id)
        .expect("favorited wallpaper remains");
    let cached_path = favorite.cached_path.clone();
    let thumbnail_path = favorite.thumbnail_path.clone().expect("thumbnail exists");
    let favorite_path = favorite
        .favorite_path
        .clone()
        .expect("favorite copy exists");

    core.remove_wallpaper(&wallpaper.id)?;

    assert!(core.wallpaper(&wallpaper.id).is_none());
    assert!(!cached_path.exists());
    assert!(!thumbnail_path.exists());
    assert!(!favorite_path.exists());
    assert!(source_image.exists());

    Ok(())
}

#[test]
fn exporting_wallpaper_copies_cached_image_to_target()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("export-wallpaper");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 13)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file(&source_image)?
        .expect("wallpaper was imported");
    let export_path = root.join("exports").join("wallpaper-copy.png");

    let exported_path = core.export_wallpaper(&wallpaper.id, &export_path)?;

    assert_eq!(exported_path, export_path);
    assert!(export_path.exists());
    assert_eq!(fs::read(&export_path)?, fs::read(&wallpaper.cached_path)?);

    Ok(())
}

#[test]
fn spotlight_metadata_is_saved_with_wallpaper()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("spotlight-metadata");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 14)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths.clone(), Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file(&source_image)?
        .expect("wallpaper was imported");
    core.update_wallpaper_spotlight_metadata(
        &wallpaper.id,
        SpotlightMetadata {
            spotlight_id: Some("DS_ArchwaySpitzkoppe".to_string()),
            title: Some("Spitzkoppe, Namibia".to_string()),
            caption: Some("'Eye of Spitzkoppe,' Namibia".to_string()),
            copyright: Some(
                "\u{00a9} Simon Phelps Photography / Moment / Getty Images".to_string(),
            ),
            info_url: Some(
                "https://www.bing.com/spotlight?spotlightid=DS_ArchwaySpitzkoppe&q=Spitzkoppe%2C+Namibia&FORM=MC13ER"
                    .to_string(),
            ),
            content_id: Some("128000000004965589".to_string()),
        },
    )?;

    let reopened = SpotlitCore::open(paths, Vec::new())?;
    let saved = reopened
        .wallpaper(&wallpaper.id)
        .expect("wallpaper was reopened");

    assert_eq!(saved.display_title(), "Spitzkoppe, Namibia");
    assert_eq!(
        saved.spotlight.spotlight_id.as_deref(),
        Some("DS_ArchwaySpitzkoppe")
    );
    assert_eq!(
        saved.spotlight.content_id.as_deref(),
        Some("128000000004965589")
    );

    Ok(())
}

#[test]
fn spotlight_metadata_backfill_adds_missing_fields_without_overwriting()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("spotlight-metadata-backfill");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 17)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths.clone(), Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file(&source_image)?
        .expect("wallpaper was imported");
    let original_cached_path = wallpaper.cached_path.clone();

    let backfilled = core
        .backfill_wallpaper_spotlight_metadata(
            &wallpaper.id,
            SpotlightMetadata {
                title: Some("Spitzkoppe, Namibia".to_string()),
                ..SpotlightMetadata::default()
            },
        )?
        .expect("metadata was backfilled");

    assert_eq!(backfilled.display_title(), "Spitzkoppe, Namibia");
    assert!(backfilled.cached_path.exists());
    assert!(!original_cached_path.exists());

    let merged = core
        .backfill_wallpaper_spotlight_metadata(
            &wallpaper.id,
            SpotlightMetadata {
                spotlight_id: Some("DS_ArchwaySpitzkoppe".to_string()),
                title: Some("Replacement title should not win".to_string()),
                content_id: Some("128000000004965589".to_string()),
                ..SpotlightMetadata::default()
            },
        )?
        .expect("missing fields were backfilled");

    assert_eq!(
        merged.spotlight.title.as_deref(),
        Some("Spitzkoppe, Namibia")
    );
    assert_eq!(
        merged.spotlight.spotlight_id.as_deref(),
        Some("DS_ArchwaySpitzkoppe")
    );
    assert_eq!(
        merged.spotlight.content_id.as_deref(),
        Some("128000000004965589")
    );

    let unchanged = core.backfill_wallpaper_spotlight_metadata(
        &wallpaper.id,
        SpotlightMetadata {
            title: Some("Replacement title should still not win".to_string()),
            spotlight_id: Some("DS_ArchwaySpitzkoppe".to_string()),
            ..SpotlightMetadata::default()
        },
    )?;
    assert!(unchanged.is_none());

    let reopened = SpotlitCore::open(paths, Vec::new())?;
    let saved = reopened
        .wallpaper(&wallpaper.id)
        .expect("wallpaper was reopened");
    assert_eq!(
        saved.spotlight.title.as_deref(),
        Some("Spitzkoppe, Namibia")
    );
    assert_eq!(
        saved.spotlight.spotlight_id.as_deref(),
        Some("DS_ArchwaySpitzkoppe")
    );

    Ok(())
}

#[test]
fn spotlight_metadata_renames_cached_thumbnail_and_favorite_files()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("spotlight-readable-files");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 15)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file(&source_image)?
        .expect("wallpaper was imported");
    let original_cached_path = wallpaper.cached_path.clone();
    let original_thumbnail_path = wallpaper.thumbnail_path.clone().expect("thumbnail exists");
    core.set_favorite(&wallpaper.id, true)?;
    let original_favorite_path = core
        .wallpaper(&wallpaper.id)
        .expect("favorited wallpaper remains")
        .favorite_path
        .expect("favorite path exists");

    let renamed = core.update_wallpaper_spotlight_metadata(
        &wallpaper.id,
        SpotlightMetadata {
            spotlight_id: Some("DS_ArchwaySpitzkoppe".to_string()),
            title: Some("Spitzkoppe, Namibia".to_string()),
            ..SpotlightMetadata::default()
        },
    )?;

    let expected_image_name = format!("spitzkoppe-namibia-{}.png", wallpaper.id);
    let expected_thumbnail_name = format!("spitzkoppe-namibia-{}.jpg", wallpaper.id);

    assert_eq!(
        renamed
            .cached_path
            .file_name()
            .and_then(|value| value.to_str()),
        Some(expected_image_name.as_str())
    );
    assert_eq!(
        renamed
            .thumbnail_path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|value| value.to_str()),
        Some(expected_thumbnail_name.as_str())
    );
    assert_eq!(
        renamed
            .favorite_path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|value| value.to_str()),
        Some(expected_image_name.as_str())
    );
    assert!(renamed.cached_path.exists());
    assert!(
        renamed
            .thumbnail_path
            .as_ref()
            .is_some_and(|path| path.exists())
    );
    assert!(
        renamed
            .favorite_path
            .as_ref()
            .is_some_and(|path| path.exists())
    );
    assert!(!original_cached_path.exists());
    assert!(!original_thumbnail_path.exists());
    assert!(!original_favorite_path.exists());

    Ok(())
}

#[test]
fn scan_preserves_metadata_named_cached_path() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let root = temp_root("spotlight-preserve-readable-files");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image, 16)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths.clone(), vec![source_dir])?;
    core.scan_spotlight_wallpapers()?;
    let wallpaper = core.current_wallpaper().expect("wallpaper was scanned");
    let hash_cache_path = wallpaper.cached_path.clone();

    let renamed = core.update_wallpaper_spotlight_metadata(
        &wallpaper.id,
        SpotlightMetadata {
            spotlight_id: Some("DS_ArchwaySpitzkoppe".to_string()),
            title: Some("Spitzkoppe, Namibia".to_string()),
            ..SpotlightMetadata::default()
        },
    )?;
    let readable_cache_path = renamed.cached_path.clone();

    core.scan_spotlight_wallpapers()?;
    let refreshed = core
        .wallpaper(&wallpaper.id)
        .expect("wallpaper remains after refresh");

    assert_eq!(refreshed.cached_path, readable_cache_path);
    assert!(readable_cache_path.exists());
    assert!(!hash_cache_path.exists());

    Ok(())
}

#[test]
fn library_load_normalizes_legacy_last_seen_at()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("library-normalize");
    let paths = AppPaths::new(root.join("data"));
    paths.ensure_dirs()?;

    let cached_path = paths.wallpaper_dir.join("legacy.jpg");
    let source_path = root.join("source").join("legacy.jpg");
    fs::create_dir_all(source_path.parent().expect("source parent"))?;
    fs::write(&cached_path, b"cached image bytes")?;
    fs::write(&source_path, b"source image bytes")?;

    let library_json = serde_json::json!({
        "wallpapers": {
            "legacy-id": {
                "id": "legacy-id",
                "source_path": source_path,
                "cached_path": cached_path,
                "thumbnail_path": null,
                "width": 1920,
                "height": 1080,
                "sha256": "legacy-sha",
                "discovered_at": "2024-01-02T03:04:05Z",
                "favorited_at": null,
                "last_synced_at": null
            }
        }
    });
    fs::write(
        &paths.library_file,
        serde_json::to_vec_pretty(&library_json)?,
    )?;

    let library = WallpaperLibrary::load(paths.library_file.clone())?;
    let wallpaper = library
        .get(&WallpaperId::new("legacy-id"))
        .expect("legacy wallpaper was loaded");

    assert_eq!(wallpaper.last_seen_at, Some(wallpaper.discovered_at));
    assert!(wallpaper.spotlight.is_empty());
    assert!(fs::read_to_string(&paths.library_file)?.contains("last_seen_at"));

    Ok(())
}

fn write_test_wallpaper(path: &Path, seed: u32) -> image::ImageResult<()> {
    let image = RgbImage::from_fn(1400, 900, |x, y| {
        Rgb([
            ((x * 17 + y * 3 + seed) % 251) as u8,
            ((x * 5 + y * 29 + seed * 7) % 241) as u8,
            ((x * 13 + y * 11 + seed * 13) % 233) as u8,
        ])
    });
    image.save(path)
}

fn temp_root(name: &str) -> TempRoot {
    TempRoot::new(name)
}

struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("spotlit-{name}-{}-{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        Self { path }
    }
}

impl Deref for TempRoot {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
