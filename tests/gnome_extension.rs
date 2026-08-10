#![cfg(target_os = "linux")]

use std::{fs, path::Path, process::Command};

use serde_json::Value;

const EXTENSION_ROOT: &str = "extensions/gnome-shell/lock-screen@spotlit.app";
const PACKAGE_SCRIPT: &str = "extensions/gnome-shell/package.sh";
const EXTENSION_UUID: &str = "lock-screen@spotlit.app";

#[test]
fn gnome_extension_metadata_targets_shell_50_lock_screen_sessions()
-> Result<(), Box<dyn std::error::Error>> {
    let metadata: Value = serde_json::from_str(&fs::read_to_string(
        Path::new(EXTENSION_ROOT).join("metadata.json"),
    )?)?;

    assert_eq!(metadata["uuid"], EXTENSION_UUID);
    assert_eq!(metadata["version"], 5);
    assert_eq!(metadata["url"], "https://github.com/oksyd/spotlit");
    assert_eq!(metadata["shell-version"], serde_json::json!(["50"]));
    assert_eq!(
        metadata["session-modes"],
        serde_json::json!(["user", "unlock-dialog"])
    );
    assert_eq!(
        metadata["settings-schema"],
        "org.gnome.shell.extensions.spotlit-lock-screen"
    );
    Ok(())
}

#[test]
fn preferences_disconnect_standard_settings_signal_when_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let preferences = fs::read_to_string(Path::new(EXTENSION_ROOT).join("prefs.js"))?;

    assert!(!preferences.contains("settings.connectObject"));
    assert!(preferences.contains("settings.connect('changed::blur-mode'"));
    assert!(preferences.contains("settings.connect('changed::display-mode'"));
    assert!(preferences.contains("settings.disconnect(id)"));
    assert!(preferences.contains("window.connect('close-request'"));
    Ok(())
}

#[test]
fn gnome_extension_schema_is_valid() -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("glib-compile-schemas")
        .args(["--strict", "--dry-run"])
        .arg(Path::new(EXTENSION_ROOT).join("schemas"))
        .status()?;

    assert!(status.success());
    Ok(())
}

#[test]
fn display_policy_is_extension_local_and_defaults_to_system_behavior()
-> Result<(), Box<dyn std::error::Error>> {
    let schema = fs::read_to_string(
        Path::new(EXTENSION_ROOT)
            .join("schemas/org.gnome.shell.extensions.spotlit-lock-screen.gschema.xml"),
    )?;

    assert!(schema.contains("<key name=\"display-mode\" type=\"s\">"));
    assert!(schema.contains("<choice value=\"keep-on-ac\"/>"));
    assert!(schema.contains("<choice value=\"keep-on\"/>"));
    assert_eq!(schema.matches("<default>'system'</default>").count(), 2);
    Ok(())
}

#[test]
fn display_policy_tracks_lock_and_power_lifecycle() -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(Path::new(EXTENSION_ROOT).join("extension.js"))?;

    assert!(source.contains("import UPower from 'gi://UPowerGlib'"));
    assert!(source.contains("'active-changed', () => this._sync()"));
    assert!(source.contains("'locked-changed', () => this._sync()"));
    assert!(source.contains("'notify::on-battery', () => this._sync()"));
    assert!(source.contains("'notify::lid-is-closed', () => this._sync()"));
    assert!(source.contains("this._screenShield._wakeUpScreen()"));
    assert!(source.contains("this._screenShield.emit('wake-up-screen')"));
    assert!(source.contains("this._displayController?.destroy()"));
    assert!(source.contains("GLib.Source.remove(this._keepVisibleId)"));
    Ok(())
}

#[test]
fn clear_mode_styles_are_scoped_to_the_unlock_dialog() -> Result<(), Box<dyn std::error::Error>> {
    let stylesheet = fs::read_to_string(Path::new(EXTENSION_ROOT).join("stylesheet.css"))?;

    assert!(stylesheet.contains(".unlock-dialog.spotlit-clear-mode .unlock-dialog-clock"));
    assert!(stylesheet.contains(".unlock-dialog.spotlit-clear-mode .spotlit-unlock-prompt-card"));
    assert!(!stylesheet.contains(".login-dialog.spotlit-clear-mode"));
    Ok(())
}

#[test]
fn clear_mode_card_targets_the_stable_prompt_container() -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(Path::new(EXTENSION_ROOT).join("extension.js"))?;

    assert!(source.contains("dialog._promptBox?.add_style_class_name(PROMPT_CARD_CLASS)"));
    assert!(source.contains("dialog._promptBox?.remove_style_class_name(PROMPT_CARD_CLASS)"));
    assert!(!source.contains("dialog._authPrompt?.add_style_class_name(PROMPT_CARD_CLASS)"));
    Ok(())
}

#[test]
fn gnome_extension_has_no_install_or_system_configuration_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(Path::new(EXTENSION_ROOT).join("extension.js"))?;

    for forbidden in [
        "Gio.Subprocess",
        "GLib.spawn",
        "gnome-extensions",
        "gsettings set",
    ] {
        assert!(
            !source.contains(forbidden),
            "unexpected side effect: {forbidden}"
        );
    }
    Ok(())
}

#[test]
fn gnome_extension_packaging_has_no_install_or_configuration_side_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(PACKAGE_SCRIPT)?;

    for forbidden in [
        "gnome-extensions install",
        "gnome-extensions enable",
        "gsettings set",
    ] {
        assert!(
            !source.contains(forbidden),
            "unexpected packaging side effect: {forbidden}"
        );
    }
    Ok(())
}
