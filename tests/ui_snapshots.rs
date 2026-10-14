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
        daily-bing-available: true;
        wallpaper-source-text: root.snapshot-policy;
        gnome-integration-update-available: root.snapshot-extension-update;
        last-applied-id: root.snapshot-applied ? root.current-id : "";
        last-applied-title: root.snapshot-applied ? root.current-title : "";
        last-applied-time: root.snapshot-applied ? "2026-08-07 14:30" : "";
        gnome-integration-state-text: "Ready";
        lock-screen-display-text: "Plugged In";
        lock-screen-apply-available: false;
        update-status-text: root.snapshot-update-status;
        update-action-kind: root.snapshot-update-action;

        in-out property <bool> snapshot-show-settings: false;
        in-out property <string> snapshot-settings-page: "wallpaper";
        in-out property <bool> snapshot-dark-theme: false;
        in-out property <string> snapshot-policy: "Daily Bing";
        in-out property <bool> snapshot-extension-update: false;
        in-out property <bool> snapshot-applied: false;
        in-out property <length> snapshot-width: 1180px;
        in-out property <length> snapshot-height: 680px;
        in-out property <string> snapshot-update-status: "Up to date (v0.1.0)";
        in-out property <string> snapshot-update-action: "check";

        show-settings: root.snapshot-show-settings;
        settings-page: root.snapshot-settings-page;
        dark-theme: root.snapshot-dark-theme;
        width: root.snapshot-width;
        height: root.snapshot-height;
    }
}

const SNAPSHOT_SIZE: (u32, u32) = (1180, 680);

#[test]
fn main_window_visual_snapshots() -> Result<(), Box<dyn Error>> {
    let runtime = SnapshotRuntime::builder()
        .clock_mode(ClockMode::Manual)
        .build()?;
    assert_bundled_translations_preserve_wallpaper_metadata()?;
    assert_manual_apply_targets_preview(&runtime)?;

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
            ui.set_snapshot_update_action("download".into());
            check_snapshot(
                &runtime,
                &ui,
                "settings/app-update-light",
                SNAPSHOT_SIZE,
                mode,
                &store,
            )?;
            ui.set_snapshot_update_status("Up to date (v0.1.0)".into());
            ui.set_snapshot_update_action("check".into());
        }
        if page == "wallpaper" {
            ui.set_snapshot_policy("Library".into());
            ui.set_snapshot_extension_update(true);
            check_snapshot(
                &runtime,
                &ui,
                "settings/wallpaper-update-light",
                SNAPSHOT_SIZE,
                mode,
                &store,
            )?;
            ui.set_snapshot_extension_update(false);
            ui.set_snapshot_policy("Daily Bing".into());
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

    ui.set_snapshot_settings_page("wallpaper".into());
    check_snapshot(
        &runtime,
        &ui,
        "settings/wallpaper-dark",
        SNAPSHOT_SIZE,
        mode,
        &store,
    )?;
    ui.set_snapshot_dark_theme(false);
    ui.set_snapshot_extension_update(true);
    ui.set_snapshot_policy("Library".into());
    ui.set_snapshot_width(820.0);
    ui.set_snapshot_height(620.0);
    runtime.set_size(ui.window(), (820, 620), 1.0)?;
    check_snapshot(
        &runtime,
        &ui,
        "settings/wallpaper-compact-light",
        (820, 620),
        mode,
        &store,
    )?;
    render_localized_previews(&runtime)?;
    ui.set_snapshot_width(1180.0);
    ui.set_snapshot_height(680.0);
    runtime.set_size(ui.window(), SNAPSHOT_SIZE, 1.0)?;
    ui.set_snapshot_show_settings(false);
    ui.set_snapshot_applied(true);
    check_snapshot(
        &runtime,
        &ui,
        "main/applied-light",
        SNAPSHOT_SIZE,
        mode,
        &store,
    )?;

    Ok(())
}

fn assert_manual_apply_targets_preview(runtime: &SnapshotRuntime) -> Result<(), Box<dyn Error>> {
    let ui = spotlit::MainWindow::new()?;
    ui.set_has_current(true);
    ui.set_current_id("preview-image".into());
    ui.set_current_image_path("fixture-preview.png".into());
    let applied_ids = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded_ids = std::rc::Rc::clone(&applied_ids);
    ui.on_wallpaper_sync_requested(move |id| recorded_ids.borrow_mut().push(id.to_string()));
    ui.on_sync_requested(|| panic!("manual apply must not resolve a different desktop image"));
    ui.window().show()?;
    runtime.set_size(ui.window(), SNAPSHOT_SIZE, 1.0)?;
    runtime.render(ui.window())?;
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "s".into() });
    assert_eq!(*applied_ids.borrow(), ["preview-image"]);
    ui.window().hide()?;
    Ok(())
}

fn render_localized_previews(runtime: &SnapshotRuntime) -> Result<(), Box<dyn Error>> {
    for language in ["de", "zh-CN"] {
        slint::select_bundled_translation(language)?;
        let ui = spotlit::MainWindow::new()?;
        ui.set_show_settings(true);
        ui.set_daily_bing_available(true);
        ui.set_wallpaper_source_text("Library".into());
        ui.set_gnome_integration_visible(true);
        ui.set_gnome_integration_state_text("Ready".into());
        ui.set_gnome_integration_update_available(true);
        ui.window().show()?;
        runtime.set_size(ui.window(), SNAPSHOT_SIZE, 1.0)?;
        runtime.advance_time(std::time::Duration::from_millis(300))?;
        runtime.render(ui.window())?.write_png(
            &std::path::Path::new("target/slint-snapshots/settings")
                .join(format!("wallpaper-{language}.png")),
        )?;
        ui.window().hide()?;
    }
    slint::select_bundled_translation("en")?;
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
    runtime.advance_time(std::time::Duration::from_millis(300))?;
    let frame = runtime.render(ui.window())?;
    assert_eq!(frame.dimensions(), expected_size);

    SnapshotAssertion::try_new(name, frame)?
        .store(store.clone())
        .policy(ComparisonPolicy::Exact)
        .mode(mode)
        .check()?;
    Ok(())
}
