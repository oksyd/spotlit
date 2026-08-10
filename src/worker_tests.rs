use std::{
    fs,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::core::{
    AppPaths, DesktopSpotlightCreative, LanguageMode, Result, SpotlightMetadata, SpotlitCore,
    SpotlitError, WallpaperId, WallpaperSource,
};
use image::{Rgb, RgbImage};

use crate::{
    command::Command,
    platform::{
        LockScreenBlurMode, LockScreenDisplayMode, LockScreenIntegration,
        LockScreenIntegrationState, LockScreenService, PlatformServices, StartupState, SystemTheme,
    },
    worker::{Worker, WorkerEvent},
};

#[test]
fn startup_setting_uses_platform_abstraction_and_updates_settings()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = test_worker("worker-startup")?;

    let event = fixture.worker.handle(Command::SetStartAtLogin(true));

    let WorkerEvent::ConfigUpdated(_, settings) = event else {
        panic!("expected settings update");
    };
    assert!(settings.config.start_at_login);
    Ok(())
}

#[test]
fn app_behavior_settings_are_persisted() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = test_worker("worker-app-behavior")?;

    let background_event = fixture
        .worker
        .handle(Command::SetKeepRunningInBackground(false));
    let WorkerEvent::ConfigUpdated(message, settings) = background_event else {
        panic!("expected background settings update");
    };
    assert_eq!(message, "Background setting saved");
    assert!(!settings.config.keep_running_in_background);

    let update_event = fixture
        .worker
        .handle(Command::SetAutomaticUpdateChecks(false));
    let WorkerEvent::ConfigUpdated(message, settings) = update_event else {
        panic!("expected update settings update");
    };
    assert_eq!(message, "Update setting saved");
    assert!(!settings.config.automatic_update_checks);
    assert!(!settings.config.keep_running_in_background);
    Ok(())
}

#[test]
fn interface_language_is_persisted() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = test_worker("worker-language")?;

    let event = fixture
        .worker
        .handle(Command::SetLanguage("de".to_string()));
    let WorkerEvent::ConfigUpdated(message, settings) = event else {
        panic!("expected language settings update");
    };

    assert_eq!(message, "Language setting saved");
    assert_eq!(settings.config.language, LanguageMode::German);
    Ok(())
}

#[test]
fn loading_snapshot_queries_gnome_integration_without_changing_it()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture =
        test_worker_with_lock_screen("worker-gnome-integration-query", Arc::new(NoopLockScreen))?;
    fixture
        .platform
        .set_lock_screen_integration(LockScreenIntegration {
            state: LockScreenIntegrationState::NotInstalled,
            blur_mode: LockScreenBlurMode::System,
            display_mode: LockScreenDisplayMode::System,
        })?;

    let event = fixture.worker.handle(Command::LoadSnapshot);

    let WorkerEvent::Snapshot(snapshot) = event else {
        panic!("expected snapshot");
    };
    assert_eq!(
        snapshot.lock_screen_integration.state,
        LockScreenIntegrationState::NotInstalled
    );
    assert!(fixture.platform.integration_actions()?.is_empty());
    Ok(())
}

#[test]
fn gnome_integration_changes_only_after_explicit_worker_command()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture =
        test_worker_with_lock_screen("worker-gnome-integration-install", Arc::new(NoopLockScreen))?;
    fixture
        .platform
        .set_lock_screen_integration(LockScreenIntegration {
            state: LockScreenIntegrationState::NotInstalled,
            blur_mode: LockScreenBlurMode::System,
            display_mode: LockScreenDisplayMode::System,
        })?;

    let event = fixture.worker.handle(Command::InstallLockScreenIntegration);

    let WorkerEvent::ConfigUpdated(message, settings) = event else {
        panic!("expected settings update");
    };
    assert_eq!(message, "GNOME extension installed");
    assert_eq!(
        settings.lock_screen_integration.state,
        LockScreenIntegrationState::Disabled
    );
    assert_eq!(fixture.platform.integration_actions()?, ["install"]);
    Ok(())
}

#[test]
fn disabling_gnome_integration_also_disables_auto_sync()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture =
        test_worker_with_lock_screen("worker-gnome-integration-disable", Arc::new(NoopLockScreen))?;
    {
        let mut core = fixture.core.lock().expect("core lock");
        let mut config = core.config().clone();
        config.auto_sync_lock_screen = true;
        core.update_config(config)?;
    }
    fixture
        .platform
        .set_lock_screen_integration(LockScreenIntegration {
            state: LockScreenIntegrationState::Enabled,
            blur_mode: LockScreenBlurMode::Soft,
            display_mode: LockScreenDisplayMode::PluggedIn,
        })?;

    let event = fixture
        .worker
        .handle(Command::SetLockScreenIntegrationEnabled(false));

    let WorkerEvent::ConfigUpdated(_, settings) = event else {
        panic!("expected settings update");
    };
    assert!(!settings.config.auto_sync_lock_screen);
    assert_eq!(
        settings.lock_screen_integration.state,
        LockScreenIntegrationState::Disabled
    );
    assert_eq!(fixture.platform.integration_actions()?, ["disable"]);
    Ok(())
}

#[test]
fn lock_screen_display_mode_setting_updates_integration()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture =
        test_worker_with_lock_screen("worker-lock-screen-display", Arc::new(NoopLockScreen))?;
    fixture
        .platform
        .set_lock_screen_integration(LockScreenIntegration {
            state: LockScreenIntegrationState::Enabled,
            blur_mode: LockScreenBlurMode::Clear,
            display_mode: LockScreenDisplayMode::System,
        })?;

    let event = fixture
        .worker
        .handle(Command::SetLockScreenDisplayMode("keep-on-ac".to_string()));

    let WorkerEvent::ConfigUpdated(message, settings) = event else {
        panic!("expected settings update");
    };
    assert_eq!(message, "Lock screen display saved");
    assert_eq!(
        settings.lock_screen_integration.display_mode,
        LockScreenDisplayMode::PluggedIn
    );
    assert_eq!(
        settings.lock_screen_integration.blur_mode,
        LockScreenBlurMode::Clear
    );
    assert_eq!(fixture.platform.integration_actions()?, ["display"]);
    Ok(())
}

#[test]
fn wallpaper_source_setting_updates_config() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let fixture = test_worker("worker-wallpaper-source")?;

    let event = fixture
        .worker
        .handle(Command::SetWallpaperSource("random_favorites".to_string()));

    let WorkerEvent::ConfigUpdated(_, settings) = event else {
        panic!("expected settings update");
    };
    assert_eq!(
        settings.config.wallpaper_source,
        WallpaperSource::RandomFavorites
    );
    Ok(())
}

#[test]
fn history_limit_setting_prunes_old_history() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let root = temp_root("worker-history-limit");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let first_image = source_dir.join("first.png");
    let second_image = source_dir.join("second.png");
    let third_image = source_dir.join("third.png");
    write_test_wallpaper_variant(&first_image, 1)?;
    write_test_wallpaper_variant(&second_image, 2)?;
    write_test_wallpaper_variant(&third_image, 3)?;

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
    let core = Arc::new(Mutex::new(core));
    let worker = Worker::new(
        Arc::clone(&core),
        Arc::new(NoopLockScreen),
        Arc::new(FakePlatform::default()),
    );

    let event = worker.handle(Command::SetHistoryLimit(Some(1)));

    let WorkerEvent::SettingsUpdated(message, snapshot) = event else {
        panic!("expected settings update");
    };
    assert_eq!(message, "History limit saved: 1 wallpapers removed");
    assert_eq!(
        snapshot
            .config
            .max_history_wallpapers
            .map(|limit| limit.get()),
        Some(1)
    );
    assert_eq!(snapshot.wallpapers.len(), 2);
    assert!(
        snapshot
            .wallpapers
            .iter()
            .any(|wallpaper| wallpaper.id == second.id)
    );
    assert!(
        snapshot
            .wallpapers
            .iter()
            .any(|wallpaper| wallpaper.id == third.id)
    );
    assert!(!first.cached_path.exists());
    assert!(second.cached_path.exists());
    assert!(third.cached_path.exists());

    Ok(())
}

#[test]
fn export_missing_wallpaper_fails_before_opening_dialog()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = test_worker("worker-export-cancel")?;

    let event = fixture.worker.handle(Command::ExportWallpaper {
        id: "missing".to_string(),
    });

    assert!(matches!(event, WorkerEvent::Failed(_)));
    Ok(())
}

#[test]
fn sync_records_after_lock_screen_succeeds() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let fixture = test_worker_with_lock_screen("worker-sync-ok", Arc::new(NoopLockScreen))?;

    let event = fixture.worker.handle(Command::SyncWallpaper {
        id: fixture.wallpaper_id.to_string(),
    });

    assert!(matches!(event, WorkerEvent::Synced(_, _)));
    assert!(
        fixture
            .core
            .lock()
            .expect("core lock")
            .wallpaper(&fixture.wallpaper_id)
            .expect("wallpaper remains")
            .last_synced_at
            .is_some()
    );

    Ok(())
}

#[test]
fn sync_uses_unique_lock_screen_staging_files()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let lock_screen = Arc::new(RecordingLockScreen::default());
    let fixture = test_worker_with_lock_screen("worker-sync-staging", lock_screen.clone())?;

    let first = fixture.worker.handle(Command::SyncWallpaper {
        id: fixture.wallpaper_id.to_string(),
    });
    let second = fixture.worker.handle(Command::SyncWallpaper {
        id: fixture.wallpaper_id.to_string(),
    });

    assert!(matches!(first, WorkerEvent::Synced(_, _)));
    assert!(matches!(second, WorkerEvent::Synced(_, _)));

    let synced_paths = lock_screen.synced_paths()?;
    assert_eq!(synced_paths.len(), 2);
    assert_ne!(synced_paths[0], synced_paths[1]);

    let core = fixture.core.lock().expect("core lock");
    let lock_screen_dir = core.paths().lock_screen_dir.clone();
    let library_image_path = core
        .wallpaper(&fixture.wallpaper_id)
        .expect("wallpaper remains")
        .best_image_path()
        .to_path_buf();
    drop(core);

    for synced_path in synced_paths {
        assert_eq!(synced_path.parent(), Some(lock_screen_dir.as_path()));
        assert_ne!(synced_path, library_image_path);
        assert!(synced_path.exists());
    }

    Ok(())
}

#[test]
fn sync_failure_does_not_record_synced_at() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = test_worker_with_lock_screen("worker-sync-fail", Arc::new(FailingLockScreen))?;

    let event = fixture.worker.handle(Command::SyncWallpaper {
        id: fixture.wallpaper_id.to_string(),
    });

    assert!(
        matches!(event, WorkerEvent::Failed(failure) if failure.message.contains("sync failed"))
    );
    assert!(
        fixture
            .core
            .lock()
            .expect("core lock")
            .wallpaper(&fixture.wallpaper_id)
            .expect("wallpaper remains")
            .last_synced_at
            .is_none()
    );

    Ok(())
}

#[test]
fn enabling_auto_sync_syncs_immediately() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let lock_screen = Arc::new(RecordingLockScreen::default());
    let fixture = test_worker_with_lock_screen("worker-auto-sync-enable", lock_screen.clone())?;

    let event = fixture.worker.handle(Command::SetAutoSync(true));

    let WorkerEvent::SettingsUpdated(message, snapshot) = event else {
        panic!("expected settings update with snapshot");
    };
    assert_eq!(message, "Auto sync enabled and synced");
    assert!(snapshot.config.auto_sync_lock_screen);
    assert_eq!(lock_screen.synced_paths()?.len(), 1);
    assert!(
        fixture
            .core
            .lock()
            .expect("core lock")
            .wallpaper(&fixture.wallpaper_id)
            .expect("wallpaper remains")
            .last_synced_at
            .is_some()
    );

    Ok(())
}

#[test]
fn enabling_auto_sync_when_already_enabled_only_saves_setting()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let lock_screen = Arc::new(RecordingLockScreen::default());
    let fixture =
        test_worker_with_lock_screen("worker-auto-sync-already-enabled", lock_screen.clone())?;
    {
        let mut core = fixture.core.lock().expect("core lock");
        let mut config = core.config().clone();
        config.auto_sync_lock_screen = true;
        core.update_config(config)?;
    }

    let event = fixture.worker.handle(Command::SetAutoSync(true));

    let WorkerEvent::ConfigUpdated(message, settings) = event else {
        panic!("expected config update");
    };
    assert_eq!(message, "Settings saved");
    assert!(settings.config.auto_sync_lock_screen);
    assert!(lock_screen.synced_paths()?.is_empty());

    Ok(())
}

#[test]
fn disabling_auto_sync_does_not_sync() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let lock_screen = Arc::new(RecordingLockScreen::default());
    let fixture = test_worker_with_lock_screen("worker-auto-sync-disable", lock_screen.clone())?;

    let event = fixture.worker.handle(Command::SetAutoSync(false));

    let WorkerEvent::ConfigUpdated(message, settings) = event else {
        panic!("expected config update");
    };
    assert_eq!(message, "Settings saved");
    assert!(!settings.config.auto_sync_lock_screen);
    assert!(lock_screen.synced_paths()?.is_empty());

    Ok(())
}

#[test]
fn auto_sync_enable_failure_preserves_disabled_config()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture =
        test_worker_with_lock_screen("worker-auto-sync-enable-fail", Arc::new(FailingLockScreen))?;

    let event = fixture.worker.handle(Command::SetAutoSync(true));

    assert!(
        matches!(event, WorkerEvent::Failed(failure) if failure.message.contains("sync failed"))
    );
    let core = fixture.core.lock().expect("core lock");
    assert!(!core.config().auto_sync_lock_screen);
    assert!(
        core.wallpaper(&fixture.wallpaper_id)
            .expect("wallpaper remains")
            .last_synced_at
            .is_none()
    );

    Ok(())
}

#[test]
fn auto_sync_library_rotation_respects_global_interval()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("worker-auto-sync-library-rotation");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;

    let first_image = source_dir.join("first.png");
    let second_image = source_dir.join("second.png");
    write_test_wallpaper_variant(&first_image, 11)?;
    write_test_wallpaper_variant(&second_image, 12)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    core.import_wallpaper_file(&first_image)?
        .expect("first wallpaper was imported");
    core.import_wallpaper_file(&second_image)?
        .expect("second wallpaper was imported");
    let mut config = core.config().clone();
    config.auto_sync_lock_screen = true;
    config.wallpaper_source = WallpaperSource::RandomLibrary;
    core.update_config(config)?;

    let core = Arc::new(Mutex::new(core));
    let lock_screen = Arc::new(RecordingLockScreen::default());
    let worker = Worker::new(core, lock_screen.clone(), Arc::new(FakePlatform::default()));

    let event = worker.handle(Command::AutoSyncTick);
    assert!(matches!(event, WorkerEvent::Synced(_, _)));
    assert_eq!(lock_screen.synced_paths()?.len(), 1);

    let event = worker.handle(Command::AutoSyncTick);
    assert!(matches!(event, WorkerEvent::AutoSyncIdle));
    assert_eq!(lock_screen.synced_paths()?.len(), 1);

    Ok(())
}

#[test]
fn scan_imports_current_desktop_spotlight_metadata()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("worker-current-metadata");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;
    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image)?;

    let paths = AppPaths::new(root.join("data"));
    let core = Arc::new(Mutex::new(SpotlitCore::open(paths, Vec::new())?));
    let platform = Arc::new(FakePlatform::default());
    platform.set_current_desktop_wallpaper(
        Some(source_image),
        Some(SpotlightMetadata {
            spotlight_id: Some("DS_ArchwaySpitzkoppe".to_string()),
            title: Some("Spitzkoppe, Namibia".to_string()),
            ..SpotlightMetadata::default()
        }),
    )?;
    let worker = Worker::new(core.clone(), Arc::new(NoopLockScreen), platform);

    let event = worker.handle(Command::Scan);

    let WorkerEvent::Snapshot(snapshot) = event else {
        panic!("expected snapshot");
    };
    let current = snapshot.current.expect("current wallpaper was imported");
    assert_eq!(current.display_title(), "Spitzkoppe, Namibia");
    assert_eq!(
        core.lock()
            .expect("core lock")
            .current_wallpaper()
            .expect("current wallpaper remains")
            .spotlight
            .spotlight_id
            .as_deref(),
        Some("DS_ArchwaySpitzkoppe")
    );

    Ok(())
}

#[test]
fn scan_imports_desktop_spotlight_creative_batch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("worker-creative-batch");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;
    let spitzkoppe = source_dir.join("spitzkoppe.png");
    let portovenere = source_dir.join("portovenere.png");
    write_test_wallpaper_variant(&spitzkoppe, 1)?;
    write_test_wallpaper_variant(&portovenere, 2)?;

    let paths = AppPaths::new(root.join("data"));
    let core = Arc::new(Mutex::new(SpotlitCore::open(paths, Vec::new())?));
    let platform = Arc::new(FakePlatform::default());
    platform.set_current_desktop_wallpaper(Some(portovenere.clone()), None)?;
    platform.set_desktop_spotlight_creatives(vec![
        DesktopSpotlightCreative {
            landscape_path: spitzkoppe,
            portrait_path: None,
            metadata: SpotlightMetadata {
                spotlight_id: Some("DS_ArchwaySpitzkoppe".to_string()),
                title: Some("Spitzkoppe, Namibia".to_string()),
                ..SpotlightMetadata::default()
            },
            is_current: false,
        },
        DesktopSpotlightCreative {
            landscape_path: portovenere,
            portrait_path: None,
            metadata: SpotlightMetadata {
                spotlight_id: Some("DS_SanPietroPortovenere".to_string()),
                title: Some("Porto Venere, Italy".to_string()),
                ..SpotlightMetadata::default()
            },
            is_current: true,
        },
    ])?;
    let worker = Worker::new(core.clone(), Arc::new(NoopLockScreen), platform);

    let event = worker.handle(Command::Scan);

    let WorkerEvent::Snapshot(snapshot) = event else {
        panic!("expected snapshot");
    };
    let titles = snapshot
        .wallpapers
        .iter()
        .map(|wallpaper| wallpaper.display_title().to_string())
        .collect::<Vec<_>>();
    assert_eq!(snapshot.wallpapers.len(), 2);
    assert!(titles.contains(&"Spitzkoppe, Namibia".to_string()));
    assert!(titles.contains(&"Porto Venere, Italy".to_string()));
    assert_eq!(
        snapshot
            .current
            .expect("current wallpaper")
            .spotlight
            .spotlight_id
            .as_deref(),
        Some("DS_SanPietroPortovenere")
    );

    Ok(())
}

#[test]
fn scan_backfills_existing_desktop_spotlight_metadata()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("worker-current-metadata-backfill");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;
    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let imported = core
        .import_wallpaper_file_deferred_thumbnail(&source_image)?
        .expect("wallpaper was imported before metadata existed");
    assert!(imported.spotlight.is_empty());

    let core = Arc::new(Mutex::new(core));
    let platform = Arc::new(FakePlatform::default());
    platform.set_current_desktop_wallpaper(
        Some(source_image),
        Some(SpotlightMetadata {
            spotlight_id: Some("DS_ArchwaySpitzkoppe".to_string()),
            title: Some("Spitzkoppe, Namibia".to_string()),
            ..SpotlightMetadata::default()
        }),
    )?;
    let worker = Worker::new(core.clone(), Arc::new(NoopLockScreen), platform);

    let event = worker.handle(Command::Scan);

    let WorkerEvent::Snapshot(snapshot) = event else {
        panic!("expected snapshot");
    };
    assert_eq!(snapshot.wallpapers.len(), 1);
    assert_eq!(
        snapshot.current.expect("current wallpaper").display_title(),
        "Spitzkoppe, Namibia"
    );

    let saved = core
        .lock()
        .expect("core lock")
        .wallpaper(&imported.id)
        .expect("existing wallpaper remains");
    assert_eq!(saved.display_title(), "Spitzkoppe, Namibia");
    assert_eq!(
        saved.spotlight.spotlight_id.as_deref(),
        Some("DS_ArchwaySpitzkoppe")
    );

    Ok(())
}

#[test]
fn scan_backfill_preserves_existing_spotlight_metadata()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("worker-current-metadata-preserve");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;
    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let imported = core
        .import_wallpaper_file_deferred_thumbnail(&source_image)?
        .expect("wallpaper was imported");
    core.update_wallpaper_spotlight_metadata(
        &imported.id,
        SpotlightMetadata {
            title: Some("Existing title".to_string()),
            ..SpotlightMetadata::default()
        },
    )?;

    let core = Arc::new(Mutex::new(core));
    let platform = Arc::new(FakePlatform::default());
    platform.set_current_desktop_wallpaper(
        Some(source_image),
        Some(SpotlightMetadata {
            spotlight_id: Some("DS_ArchwaySpitzkoppe".to_string()),
            title: Some("New partial title".to_string()),
            content_id: Some("128000000004965589".to_string()),
            ..SpotlightMetadata::default()
        }),
    )?;
    let worker = Worker::new(core.clone(), Arc::new(NoopLockScreen), platform);

    let event = worker.handle(Command::Scan);

    let WorkerEvent::Snapshot(_) = event else {
        panic!("expected snapshot");
    };
    let saved = core
        .lock()
        .expect("core lock")
        .wallpaper(&imported.id)
        .expect("wallpaper remains");
    assert_eq!(saved.spotlight.title.as_deref(), Some("Existing title"));
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
fn warm_thumbnails_generates_deferred_preview_cache()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("worker-warm-thumbnails");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;
    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file_deferred_thumbnail(&source_image)?
        .expect("wallpaper was imported");
    assert!(wallpaper.thumbnail_path.is_none());

    let core = Arc::new(Mutex::new(core));
    let worker = Worker::new(
        core.clone(),
        Arc::new(NoopLockScreen),
        Arc::new(FakePlatform::default()),
    );

    let event = worker.handle(Command::WarmThumbnails);

    let WorkerEvent::SettingsUpdated(_, snapshot) = event else {
        panic!("expected thumbnail warm-up snapshot");
    };
    let warmed = snapshot
        .wallpapers
        .iter()
        .find(|wallpaper| wallpaper.id == snapshot.current.as_ref().expect("current").id)
        .expect("wallpaper remains");
    let thumbnail_path = warmed
        .thumbnail_path
        .as_ref()
        .expect("thumbnail was generated");
    assert!(thumbnail_path.exists());
    assert!(
        core.lock()
            .expect("core lock")
            .wallpaper(&wallpaper.id)
            .expect("wallpaper remains")
            .thumbnail_path
            .is_some()
    );

    Ok(())
}

#[test]
fn importing_image_defers_thumbnail_generation()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("worker-import-deferred-thumbnail");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;
    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image)?;

    let paths = AppPaths::new(root.join("data"));
    let core = Arc::new(Mutex::new(SpotlitCore::open(paths, Vec::new())?));
    let platform = Arc::new(FakePlatform::default());
    platform.set_picked_wallpaper_image(Some(source_image))?;
    let worker = Worker::new(core.clone(), Arc::new(NoopLockScreen), platform);

    let event = worker.handle(Command::ImportImage);

    let WorkerEvent::SettingsUpdated(_, snapshot) = event else {
        panic!("expected imported snapshot");
    };
    let imported = snapshot.current.expect("current wallpaper was imported");
    assert!(imported.thumbnail_path.is_none());
    assert!(
        core.lock()
            .expect("core lock")
            .wallpaper(&imported.id)
            .expect("wallpaper remains")
            .thumbnail_path
            .is_none()
    );

    Ok(())
}

#[test]
fn load_snapshot_does_not_scan_or_warm_thumbnails()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("worker-load-snapshot");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;
    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file_deferred_thumbnail(&source_image)?
        .expect("wallpaper was imported");
    assert!(wallpaper.thumbnail_path.is_none());

    let core = Arc::new(Mutex::new(core));
    let worker = Worker::new(
        core.clone(),
        Arc::new(NoopLockScreen),
        Arc::new(FakePlatform::default()),
    );

    let event = worker.handle(Command::LoadSnapshot);

    let WorkerEvent::Snapshot(snapshot) = event else {
        panic!("expected lightweight snapshot");
    };
    assert_eq!(snapshot.wallpapers.len(), 1);
    assert!(snapshot.wallpapers[0].thumbnail_path.is_none());
    assert!(
        core.lock()
            .expect("core lock")
            .wallpaper(&wallpaper.id)
            .expect("wallpaper remains")
            .thumbnail_path
            .is_none()
    );

    Ok(())
}

#[test]
fn lazy_worker_opens_core_when_snapshot_is_requested()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("worker-lazy-load-snapshot");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;
    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image)?;

    let paths = AppPaths::new(root.join("data"));
    let imported_id = {
        let mut core = SpotlitCore::open(paths.clone(), Vec::new())?;
        core.import_wallpaper_file_deferred_thumbnail(&source_image)?
            .expect("wallpaper was imported")
            .id
    };

    let worker = Worker::open_lazy(
        paths,
        Vec::new(),
        Arc::new(NoopLockScreen),
        Arc::new(FakePlatform::default()),
    );

    let event = worker.handle(Command::LoadSnapshot);

    let WorkerEvent::Snapshot(snapshot) = event else {
        panic!("expected lazy snapshot");
    };
    assert_eq!(snapshot.current.expect("current wallpaper").id, imported_id);
    assert_eq!(snapshot.wallpapers.len(), 1);

    Ok(())
}

#[test]
fn export_default_name_uses_spotlight_metadata()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = test_worker_with_lock_screen("worker-export-name", Arc::new(NoopLockScreen))?;
    fixture
        .core
        .lock()
        .expect("core lock")
        .update_wallpaper_spotlight_metadata(
            &fixture.wallpaper_id,
            SpotlightMetadata {
                spotlight_id: Some("DS_ArchwaySpitzkoppe".to_string()),
                title: Some("Spitzkoppe, Namibia".to_string()),
                ..SpotlightMetadata::default()
            },
        )?;

    let event = fixture.worker.handle(Command::ExportWallpaper {
        id: fixture.wallpaper_id.to_string(),
    });

    assert!(matches!(event, WorkerEvent::OpenedPath(message) if message == "Export canceled"));
    assert_eq!(
        fixture.platform.last_export_default_name()?,
        Some(format!("spitzkoppe-namibia-{}.png", fixture.wallpaper_id))
    );

    Ok(())
}

#[test]
fn open_wallpaper_info_uses_spotlight_url() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = test_worker_with_lock_screen("worker-open-info", Arc::new(NoopLockScreen))?;
    let info_url = "https://www.bing.com/spotlight?spotlightid=DS_ArchwaySpitzkoppe&q=Spitzkoppe%2C+Namibia&FORM=MC13ER";
    fixture
        .core
        .lock()
        .expect("core lock")
        .update_wallpaper_spotlight_metadata(
            &fixture.wallpaper_id,
            SpotlightMetadata {
                info_url: Some(info_url.to_string()),
                ..SpotlightMetadata::default()
            },
        )?;

    let event = fixture.worker.handle(Command::OpenWallpaperInfo {
        id: fixture.wallpaper_id.to_string(),
    });

    assert!(
        matches!(event, WorkerEvent::OpenedPath(message) if message == "Opened wallpaper info")
    );
    assert_eq!(fixture.platform.opened_urls()?, vec![info_url.to_string()]);

    Ok(())
}

#[test]
fn open_release_page_uses_official_spotlit_url()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = test_worker_with_lock_screen("worker-open-release", Arc::new(NoopLockScreen))?;

    let event = fixture.worker.handle(Command::OpenReleasePage);

    assert!(
        matches!(event, WorkerEvent::OpenedPath(message) if message == "Opened Spotlit release page")
    );
    assert_eq!(
        fixture.platform.opened_urls()?,
        vec![crate::update::RELEASES_URL.to_string()]
    );
    Ok(())
}

fn test_worker(name: &str) -> std::result::Result<WorkerFixture, Box<dyn std::error::Error>> {
    let root = temp_root(name);
    let paths = AppPaths::new(root.join("data"));
    let core = Arc::new(Mutex::new(SpotlitCore::open(paths, Vec::new())?));
    let lock_screen = Arc::new(NoopLockScreen);
    let platform = Arc::new(FakePlatform::default());
    Ok(WorkerFixture {
        worker: Worker::new(core, lock_screen, platform),
        _root: root,
    })
}

fn test_worker_with_lock_screen(
    name: &str,
    lock_screen: Arc<dyn LockScreenService>,
) -> std::result::Result<WallpaperWorkerFixture, Box<dyn std::error::Error>> {
    let root = temp_root(name);
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir)?;
    let source_image = source_dir.join("wallpaper.png");
    write_test_wallpaper(&source_image)?;

    let paths = AppPaths::new(root.join("data"));
    let mut core = SpotlitCore::open(paths, Vec::new())?;
    let wallpaper = core
        .import_wallpaper_file(&source_image)?
        .expect("wallpaper was imported");
    let core = Arc::new(Mutex::new(core));
    let platform = Arc::new(FakePlatform::default());
    let worker = Worker::new(core.clone(), lock_screen, platform.clone());

    Ok(WallpaperWorkerFixture {
        worker,
        core,
        platform,
        wallpaper_id: wallpaper.id,
        _root: root,
    })
}

struct WorkerFixture {
    worker: Worker,
    _root: TempRoot,
}

struct WallpaperWorkerFixture {
    worker: Worker,
    core: Arc<Mutex<SpotlitCore>>,
    platform: Arc<FakePlatform>,
    wallpaper_id: WallpaperId,
    _root: TempRoot,
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
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
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

fn write_test_wallpaper(path: &Path) -> image::ImageResult<()> {
    write_test_wallpaper_variant(path, 0)
}

fn write_test_wallpaper_variant(path: &Path, seed: u32) -> image::ImageResult<()> {
    let image = RgbImage::from_fn(1400, 900, |x, y| {
        Rgb([
            ((x * 17 + y * 3 + seed) % 251) as u8,
            ((x * 5 + y * 29 + seed * 3) % 241) as u8,
            ((x * 13 + y * 11 + seed * 7) % 233) as u8,
        ])
    });
    image.save(path)
}

struct NoopLockScreen;

impl LockScreenService for NoopLockScreen {
    fn set_lock_screen_wallpaper(&self, _image_path: &Path) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingLockScreen {
    synced_paths: Mutex<Vec<PathBuf>>,
}

impl RecordingLockScreen {
    fn synced_paths(&self) -> Result<Vec<PathBuf>> {
        self.synced_paths
            .lock()
            .map(|paths| paths.clone())
            .map_err(|_| SpotlitError::platform("recording lock screen lock poisoned"))
    }
}

impl LockScreenService for RecordingLockScreen {
    fn set_lock_screen_wallpaper(&self, image_path: &Path) -> Result<()> {
        self.synced_paths
            .lock()
            .map_err(|_| SpotlitError::platform("recording lock screen lock poisoned"))?
            .push(image_path.to_path_buf());
        Ok(())
    }
}

struct FailingLockScreen;

impl LockScreenService for FailingLockScreen {
    fn set_lock_screen_wallpaper(&self, _image_path: &Path) -> Result<()> {
        Err(SpotlitError::platform("sync failed"))
    }
}

struct FakePlatform {
    current_desktop_wallpaper: Mutex<Option<PathBuf>>,
    desktop_spotlight_creatives: Mutex<Vec<DesktopSpotlightCreative>>,
    current_spotlight_metadata: Mutex<Option<SpotlightMetadata>>,
    export_default_name: Mutex<Option<String>>,
    opened_urls: Mutex<Vec<String>>,
    picked_wallpaper_image: Mutex<Option<PathBuf>>,
    startup_state: Mutex<StartupState>,
    lock_screen_integration: Mutex<LockScreenIntegration>,
    integration_actions: Mutex<Vec<String>>,
}

impl Default for FakePlatform {
    fn default() -> Self {
        Self {
            current_desktop_wallpaper: Mutex::new(None),
            desktop_spotlight_creatives: Mutex::new(Vec::new()),
            current_spotlight_metadata: Mutex::new(None),
            export_default_name: Mutex::new(None),
            opened_urls: Mutex::new(Vec::new()),
            picked_wallpaper_image: Mutex::new(None),
            startup_state: Mutex::new(StartupState::Disabled),
            lock_screen_integration: Mutex::new(LockScreenIntegration::default()),
            integration_actions: Mutex::new(Vec::new()),
        }
    }
}

impl FakePlatform {
    fn set_current_desktop_wallpaper(
        &self,
        path: Option<PathBuf>,
        metadata: Option<SpotlightMetadata>,
    ) -> Result<()> {
        *self
            .current_desktop_wallpaper
            .lock()
            .map_err(|_| SpotlitError::platform("fake desktop wallpaper lock poisoned"))? = path;
        *self
            .current_spotlight_metadata
            .lock()
            .map_err(|_| SpotlitError::platform("fake spotlight metadata lock poisoned"))? =
            metadata;
        Ok(())
    }

    fn set_desktop_spotlight_creatives(
        &self,
        creatives: Vec<DesktopSpotlightCreative>,
    ) -> Result<()> {
        *self
            .desktop_spotlight_creatives
            .lock()
            .map_err(|_| SpotlitError::platform("fake creative batch lock poisoned"))? = creatives;
        Ok(())
    }

    fn last_export_default_name(&self) -> Result<Option<String>> {
        self.export_default_name
            .lock()
            .map(|name| name.clone())
            .map_err(|_| SpotlitError::platform("fake export default name lock poisoned"))
    }

    fn opened_urls(&self) -> Result<Vec<String>> {
        self.opened_urls
            .lock()
            .map(|urls| urls.clone())
            .map_err(|_| SpotlitError::platform("fake opened URLs lock poisoned"))
    }

    fn set_picked_wallpaper_image(&self, path: Option<PathBuf>) -> Result<()> {
        *self
            .picked_wallpaper_image
            .lock()
            .map_err(|_| SpotlitError::platform("fake picked wallpaper lock poisoned"))? = path;
        Ok(())
    }

    fn set_lock_screen_integration(&self, integration: LockScreenIntegration) -> Result<()> {
        *self
            .lock_screen_integration
            .lock()
            .map_err(|_| SpotlitError::platform("fake integration lock poisoned"))? = integration;
        Ok(())
    }

    fn integration_actions(&self) -> Result<Vec<String>> {
        self.integration_actions
            .lock()
            .map(|actions| actions.clone())
            .map_err(|_| SpotlitError::platform("fake integration actions lock poisoned"))
    }

    fn record_integration_action(&self, action: &str) -> Result<()> {
        self.integration_actions
            .lock()
            .map_err(|_| SpotlitError::platform("fake integration actions lock poisoned"))?
            .push(action.to_string());
        Ok(())
    }
}

impl PlatformServices for FakePlatform {
    fn current_desktop_wallpaper(&self) -> Result<Option<PathBuf>> {
        self.current_desktop_wallpaper
            .lock()
            .map(|path| path.clone())
            .map_err(|_| SpotlitError::platform("fake desktop wallpaper lock poisoned"))
    }

    fn desktop_spotlight_creatives(&self) -> Result<Vec<DesktopSpotlightCreative>> {
        self.desktop_spotlight_creatives
            .lock()
            .map(|creatives| creatives.clone())
            .map_err(|_| SpotlitError::platform("fake creative batch lock poisoned"))
    }

    fn current_desktop_spotlight_metadata(&self) -> Result<Option<SpotlightMetadata>> {
        self.current_spotlight_metadata
            .lock()
            .map(|metadata| metadata.clone())
            .map_err(|_| SpotlitError::platform("fake spotlight metadata lock poisoned"))
    }

    fn open_path(&self, _path: &Path) -> Result<()> {
        Ok(())
    }

    fn open_url_in_chrome(&self, url: &str) -> Result<()> {
        self.opened_urls
            .lock()
            .map_err(|_| SpotlitError::platform("fake opened URLs lock poisoned"))?
            .push(url.to_string());
        Ok(())
    }

    fn reveal_path(&self, _path: &Path) -> Result<()> {
        Ok(())
    }

    fn lock_workstation(&self) -> Result<()> {
        Ok(())
    }

    fn pick_wallpaper_image(&self) -> Result<Option<PathBuf>> {
        self.picked_wallpaper_image
            .lock()
            .map(|path| path.clone())
            .map_err(|_| SpotlitError::platform("fake picked wallpaper lock poisoned"))
    }

    fn pick_export_image_path(&self, default_file_name: &str) -> Result<Option<PathBuf>> {
        *self
            .export_default_name
            .lock()
            .map_err(|_| SpotlitError::platform("fake export default name lock poisoned"))? =
            Some(default_file_name.to_string());
        Ok(None)
    }

    fn startup_state(&self) -> Result<StartupState> {
        self.startup_state
            .lock()
            .map(|state| *state)
            .map_err(|_| SpotlitError::platform("fake startup lock poisoned"))
    }

    fn set_startup_enabled(&self, enabled: bool) -> Result<StartupState> {
        let state = if enabled {
            StartupState::Enabled
        } else {
            StartupState::Disabled
        };
        *self
            .startup_state
            .lock()
            .map_err(|_| SpotlitError::platform("fake startup lock poisoned"))? = state;
        Ok(state)
    }

    fn system_theme(&self) -> Result<SystemTheme> {
        Ok(SystemTheme::Light)
    }

    fn lock_screen_integration(&self) -> Result<LockScreenIntegration> {
        self.lock_screen_integration
            .lock()
            .map(|integration| *integration)
            .map_err(|_| SpotlitError::platform("fake integration lock poisoned"))
    }

    fn install_lock_screen_integration(&self) -> Result<LockScreenIntegration> {
        self.record_integration_action("install")?;
        let integration = LockScreenIntegration {
            state: LockScreenIntegrationState::Disabled,
            blur_mode: LockScreenBlurMode::System,
            display_mode: LockScreenDisplayMode::System,
        };
        self.set_lock_screen_integration(integration)?;
        Ok(integration)
    }

    fn set_lock_screen_integration_enabled(&self, enabled: bool) -> Result<LockScreenIntegration> {
        self.record_integration_action(if enabled { "enable" } else { "disable" })?;
        let mut integration = self.lock_screen_integration()?;
        integration.state = if enabled {
            LockScreenIntegrationState::Enabled
        } else {
            LockScreenIntegrationState::Disabled
        };
        self.set_lock_screen_integration(integration)?;
        Ok(integration)
    }

    fn set_lock_screen_blur_mode(&self, mode: LockScreenBlurMode) -> Result<LockScreenIntegration> {
        self.record_integration_action("blur")?;
        let mut integration = self.lock_screen_integration()?;
        integration.blur_mode = mode;
        self.set_lock_screen_integration(integration)?;
        Ok(integration)
    }

    fn set_lock_screen_display_mode(
        &self,
        mode: LockScreenDisplayMode,
    ) -> Result<LockScreenIntegration> {
        self.record_integration_action("display")?;
        let mut integration = self.lock_screen_integration()?;
        integration.display_mode = mode;
        self.set_lock_screen_integration(integration)?;
        Ok(integration)
    }
}
