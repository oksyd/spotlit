use std::error::Error;

use slint::ComponentHandle;
use slint_snapshot::{
    SnapshotRuntime,
    comparison::ComparisonPolicy,
    runtime::ClockMode,
    testing::{SnapshotAssertion, SnapshotMode, SnapshotStore},
};

slint::slint! {
    import "fixtures/Tuffy.ttf";
    import { MainWindow } from "../src/ui/app-window.slint";

    export component SnapshotHarness inherits MainWindow {
        default-font-family: "Tuffy";
        has-current: true;
        current-id: "bing-us-2026-08-07";
        current-title: "Lake Tahoe at Sunrise";
        current-details: "Sierra Nevada, California, United States - Bing Wallpaper";
        current-preview-source-path: "fixture-wallpaper.jpg";
        library-summary: "7 wallpapers";
        favorite-summary: "2 favorites";
        gnome-integration-visible: true;
        gnome-integration-state-text: "Ready";
        lock-screen-display-text: "Plugged In";
        lock-screen-apply-available: false;
        update-status-text: root.snapshot-update-status;
        update-release-available: root.snapshot-update-available;

        in-out property <bool> snapshot-show-settings: false;
        in-out property <string> snapshot-settings-page: "wallpaper";
        in-out property <bool> snapshot-dark-theme: false;
        in-out property <string> snapshot-update-status: "Up to date (v0.1.0)";
        in-out property <bool> snapshot-update-available: false;

        show-settings: root.snapshot-show-settings;
        settings-page: root.snapshot-settings-page;
        dark-theme: root.snapshot-dark-theme;
    }
}

const SNAPSHOT_SIZE: (u32, u32) = (1180, 680);

#[test]
fn main_window_visual_snapshots() -> Result<(), Box<dyn Error>> {
    let runtime = SnapshotRuntime::builder()
        .clock_mode(ClockMode::Manual)
        .build()?;
    assert_bundled_translations_preserve_wallpaper_metadata()?;

    let ui = SnapshotHarness::new()?;
    runtime.set_size(ui.window(), SNAPSHOT_SIZE, 1.0)?;

    let mode = SnapshotMode::from_env()?.unwrap_or(SnapshotMode::Verify);
    let store = SnapshotStore::new("tests/snapshots", "target/slint-snapshots");

    check_snapshot(
        &runtime,
        &ui,
        "main/default-light",
        SNAPSHOT_SIZE,
        mode,
        &store,
    )?;

    ui.set_snapshot_show_settings(true);
    for page in ["wallpaper", "app", "storage"] {
        ui.set_snapshot_settings_page(page.into());
        check_snapshot(
            &runtime,
            &ui,
            &format!("settings/{page}-light"),
            SNAPSHOT_SIZE,
            mode,
            &store,
        )?;

        if page == "app" {
            ui.set_snapshot_update_status("Version v0.2.0 available".into());
            ui.set_snapshot_update_available(true);
            check_snapshot(
                &runtime,
                &ui,
                "settings/app-update-light",
                SNAPSHOT_SIZE,
                mode,
                &store,
            )?;
            ui.set_snapshot_update_status("Up to date (v0.1.0)".into());
            ui.set_snapshot_update_available(false);
        }
    }

    ui.set_snapshot_settings_page("storage".into());
    ui.set_snapshot_dark_theme(true);
    check_snapshot(
        &runtime,
        &ui,
        "settings/storage-dark",
        SNAPSHOT_SIZE,
        mode,
        &store,
    )?;

    Ok(())
}

fn assert_bundled_translations_preserve_wallpaper_metadata() -> Result<(), Box<dyn Error>> {
    let tray = spotlit::SpotlitTray::new()?;

    slint::select_bundled_translation("zh-CN")?;
    assert_eq!(
        tray.global::<spotlit::I18n>()
            .invoke_message("Ready".into()),
        "就绪"
    );

    let ui = spotlit::MainWindow::new()?;
    ui.set_current_title("Lake Tahoe at Sunrise".into());

    assert_eq!(
        ui.global::<spotlit::I18n>().invoke_message("Ready".into()),
        "就绪"
    );
    assert_eq!(ui.get_apply_target_text(), "锁屏");
    assert_eq!(ui.get_current_title(), "Lake Tahoe at Sunrise");
    assert_eq!(
        ui.global::<spotlit::I18n>()
            .invoke_message("Lock screen display saved".into()),
        "锁屏显示设置已保存"
    );

    slint::select_bundled_translation("de")?;
    assert_eq!(
        ui.global::<spotlit::I18n>().invoke_message("Ready".into()),
        "Bereit"
    );
    assert_eq!(ui.get_apply_target_text(), "Sperrbildschirm");
    assert_eq!(ui.get_current_title(), "Lake Tahoe at Sunrise");
    assert_eq!(
        ui.global::<spotlit::I18n>()
            .invoke_message("Lock screen display saved".into()),
        "Sperrbildschirm-Anzeige gespeichert"
    );

    slint::select_bundled_translation("en")?;
    Ok(())
}

fn check_snapshot(
    runtime: &SnapshotRuntime,
    ui: &SnapshotHarness,
    name: &str,
    expected_size: (u32, u32),
    mode: SnapshotMode,
    store: &SnapshotStore,
) -> Result<(), Box<dyn Error>> {
    let frame = runtime.render(ui.window())?;
    assert_eq!(frame.dimensions(), expected_size);

    SnapshotAssertion::try_new(name, frame)?
        .store(store.clone())
        .policy(ComparisonPolicy::Exact)
        .mode(mode)
        .check()?;
    Ok(())
}
