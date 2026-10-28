use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use crate::{
    core::{Result, SpotlitError},
    platform::{
        LockScreenBlurMode, LockScreenDisplayMode, LockScreenIntegration,
        LockScreenIntegrationState,
    },
};

pub const GNOME_EXTENSION_UUID: &str = "lock-screen@spotlit.app";

const EXTENSION_BUNDLE: &str = "lock-screen@spotlit.app.shell-extension.zip";
const EXTENSION_SCHEMA: &str = "org.gnome.shell.extensions.spotlit-lock-screen";
const INSTALL_SESSION_FILE: &str = ".spotlit-install-session.json";
const METADATA: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/metadata.json");
const EXTENSION: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/extension.js");
const RUNTIME_HOST: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/runtimeHost.js");
const RUNTIME: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/runtime.js");
const PREFERENCES: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/prefs.js");
const STYLESHEET: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/runtime.css");
const WALLPAPER_INFO: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/wallpaperInfo.js");
const SCHEMA: &str = include_str!(
    "../../../extensions/gnome-shell/lock-screen@spotlit.app/schemas/org.gnome.shell.extensions.spotlit-lock-screen.gschema.xml"
);

#[derive(Serialize, Deserialize)]
struct InstallSession {
    shell_session: String,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum RuntimeResult {
    Ready,
    Pending,
    Busy,
    Inactive,
    RestartRequired,
    Failed,
}

#[derive(Debug, Deserialize)]
struct RuntimeStatus {
    protocol: u32,
    result: RuntimeResult,
    active: bool,
    current: bool,
    revision: String,
}

pub fn lock_screen_integration() -> Result<LockScreenIntegration> {
    let installed_extensions = match extension_list(&[]) {
        Ok(extensions) => extensions,
        Err(error) => {
            tracing::warn!(%error, "GNOME extension status is unavailable");
            return Ok(integration(LockScreenIntegrationState::Unavailable));
        }
    };
    let extension_dir = installed_extension_dir();
    if !installed_extensions
        .iter()
        .any(|extension| extension == GNOME_EXTENSION_UUID)
    {
        return Ok(if extension_dir.is_some() {
            integration_with_preferences(
                LockScreenIntegrationState::RestartRequired,
                extension_dir.as_deref(),
            )
        } else {
            integration(LockScreenIntegrationState::NotInstalled)
        });
    }

    let enabled_extensions = match extension_list(&["--enabled"]) {
        Ok(extensions) => extensions,
        Err(error) => {
            tracing::warn!(%error, "GNOME enabled extension status is unavailable");
            return Ok(integration_with_preferences(
                LockScreenIntegrationState::Unavailable,
                extension_dir.as_deref(),
            ));
        }
    };
    let state = if enabled_extensions
        .iter()
        .any(|extension| extension == GNOME_EXTENSION_UUID)
    {
        LockScreenIntegrationState::Enabled
    } else {
        LockScreenIntegrationState::Disabled
    };

    Ok(integration_with_preferences(
        state,
        extension_dir.as_deref(),
    ))
}

pub fn install_lock_screen_integration(work_dir: &Path) -> Result<LockScreenIntegration> {
    let shell_session = current_shell_session()?;
    let source_dir = materialize_extension(work_dir)?;
    let schema = source_dir
        .join("schemas")
        .join("org.gnome.shell.extensions.spotlit-lock-screen.gschema.xml");

    fs::create_dir_all(work_dir).map_err(|source| SpotlitError::io(work_dir, source))?;
    run_checked(
        Command::new("gnome-extensions")
            .arg("pack")
            .arg("--force")
            .arg("--out-dir")
            .arg(work_dir)
            .arg("--schema")
            .arg(&schema)
            .arg("--extra-source")
            .arg("wallpaperInfo.js")
            .args(["--extra-source", "runtimeHost.js"])
            .args(["--extra-source", "runtime.js"])
            .args(["--extra-source", "runtime.css"])
            .arg(&source_dir),
        "package GNOME lock screen extension",
    )?;

    let bundle = work_dir.join(EXTENSION_BUNDLE);
    run_checked(
        Command::new("gnome-extensions")
            .arg("install")
            .arg("--force")
            .arg(&bundle),
        "install GNOME lock screen extension",
    )?;

    let extension_dir = installed_extension_dir()
        .ok_or_else(|| SpotlitError::platform("installed GNOME extension was not found"))?;
    crate::core::write_json_file(
        &extension_dir.join(INSTALL_SESSION_FILE),
        &InstallSession { shell_session },
    )?;
    if let Some(status) = runtime_call("Reload") {
        if status.active && status.current {
            if let Err(error) = fs::remove_file(extension_dir.join(INSTALL_SESSION_FILE)) {
                tracing::warn!(%error, "could not clear GNOME extension restart marker");
            }
        } else if status.result == RuntimeResult::Failed {
            return Err(SpotlitError::platform(if status.active {
                "GNOME extension update failed; previous version restored"
            } else {
                "GNOME extension update failed"
            }));
        }
    }
    lock_screen_integration()
}

pub fn set_lock_screen_integration_enabled(enabled: bool) -> Result<LockScreenIntegration> {
    let action = if enabled { "enable" } else { "disable" };
    run_checked(
        Command::new("gnome-extensions")
            .arg(action)
            .arg(GNOME_EXTENSION_UUID),
        if enabled {
            "enable GNOME lock screen extension"
        } else {
            "disable GNOME lock screen extension"
        },
    )?;
    lock_screen_integration()
}

pub fn set_lock_screen_blur_mode(mode: LockScreenBlurMode) -> Result<LockScreenIntegration> {
    set_extension_preference(
        "blur-mode",
        blur_mode_value(mode),
        "set GNOME lock screen blur",
    )
}

pub fn set_lock_screen_display_mode(mode: LockScreenDisplayMode) -> Result<LockScreenIntegration> {
    set_extension_preference(
        "display-mode",
        display_mode_value(mode),
        "set GNOME lock screen display policy",
    )
}

pub fn set_lock_screen_wallpaper_info(enabled: bool) -> Result<LockScreenIntegration> {
    set_extension_preference(
        "show-wallpaper-info",
        if enabled { "true" } else { "false" },
        "set GNOME lock screen wallpaper information",
    )
}

fn set_extension_preference(
    key: &str,
    value: &str,
    description: &str,
) -> Result<LockScreenIntegration> {
    let extension_dir = installed_extension_dir()
        .ok_or_else(|| SpotlitError::platform("GNOME lock screen extension is not installed"))?;
    let schema_dir = extension_dir.join("schemas");
    run_checked(
        Command::new("gsettings")
            .arg("--schemadir")
            .arg(&schema_dir)
            .arg("set")
            .arg(EXTENSION_SCHEMA)
            .arg(key)
            .arg(value),
        description,
    )?;
    lock_screen_integration()
}

pub(super) fn ensure_extension_enabled() -> Result<()> {
    let integration = lock_screen_integration()?;
    if integration.state == LockScreenIntegrationState::Enabled {
        Ok(())
    } else {
        Err(SpotlitError::platform(format!(
            "GNOME extension {GNOME_EXTENSION_UUID} is not enabled"
        )))
    }
}

fn integration(state: LockScreenIntegrationState) -> LockScreenIntegration {
    LockScreenIntegration {
        state,
        blur_mode: LockScreenBlurMode::System,
        display_mode: LockScreenDisplayMode::System,
        wallpaper_info_enabled: false,
        update_available: false,
    }
}

fn integration_with_preferences(
    mut state: LockScreenIntegrationState,
    extension_dir: Option<&Path>,
) -> LockScreenIntegration {
    let mut update_available = extension_dir.is_some_and(extension_needs_update);
    let runtime = (state == LockScreenIntegrationState::Enabled)
        .then(|| runtime_call("GetStatus"))
        .flatten();
    let mut installed_this_session = false;
    if let Some(dir) = extension_dir
        && !update_available
        && let Ok(contents) = fs::read(dir.join(INSTALL_SESSION_FILE))
        && let Ok(installed) = serde_json::from_slice::<InstallSession>(&contents)
        && let Ok(session) = current_shell_session()
        && installed.shell_session == session
    {
        installed_this_session = true;
    }
    (state, update_available) = runtime_integration_state(
        state,
        update_available,
        installed_this_session,
        runtime.as_ref(),
    );
    let schema_dir = extension_dir.map(|dir| dir.join("schemas"));
    let blur_mode = schema_dir
        .as_deref()
        .and_then(|dir| read_blur_mode(dir).ok())
        .unwrap_or_default();
    let display_mode = schema_dir
        .as_deref()
        .and_then(|dir| read_display_mode(dir).ok())
        .unwrap_or_default();
    let wallpaper_info_enabled = schema_dir
        .as_deref()
        .and_then(|dir| {
            read_extension_preference(dir, "show-wallpaper-info", "wallpaper information").ok()
        })
        .is_some_and(|value| value.trim() == "true");
    LockScreenIntegration {
        state,
        blur_mode,
        display_mode,
        wallpaper_info_enabled,
        update_available,
    }
}

fn extension_needs_update(dir: &Path) -> bool {
    let files = [
        ("extension.js", EXTENSION),
        ("runtimeHost.js", RUNTIME_HOST),
        ("runtime.js", RUNTIME),
        ("prefs.js", PREFERENCES),
        ("runtime.css", STYLESHEET),
        ("wallpaperInfo.js", WALLPAPER_INFO),
        (
            "schemas/org.gnome.shell.extensions.spotlit-lock-screen.gschema.xml",
            SCHEMA,
        ),
    ];
    let installed_metadata = fs::read(dir.join("metadata.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let bundled_metadata = serde_json::from_str::<serde_json::Value>(METADATA)
        .expect("bundled extension metadata is valid");
    if installed_metadata
        .as_ref()
        .and_then(|metadata| metadata["version"].as_u64())
        > bundled_metadata["version"].as_u64()
    {
        return false;
    }
    let metadata_matches = installed_metadata.as_ref() == Some(&bundled_metadata);
    !metadata_matches
        || files.iter().any(|(path, source)| {
            fs::read_to_string(dir.join(path))
                .ok()
                .is_none_or(|installed| installed != *source)
        })
}

fn runtime_integration_state(
    state: LockScreenIntegrationState,
    update_available: bool,
    installed_this_session: bool,
    runtime: Option<&RuntimeStatus>,
) -> (LockScreenIntegrationState, bool) {
    if let Some(runtime) = runtime {
        if runtime.active && runtime.current {
            return (state, update_available);
        }
        if !runtime.active || runtime.result == RuntimeResult::RestartRequired {
            return (
                LockScreenIntegrationState::RestartRequired,
                update_available,
            );
        }
        return (state, true);
    }
    if installed_this_session {
        (
            LockScreenIntegrationState::RestartRequired,
            update_available,
        )
    } else {
        (state, update_available)
    }
}

fn runtime_call(method: &str) -> Option<RuntimeStatus> {
    let output = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--timeout",
            "3",
            "--dest",
            "org.gnome.Shell",
            "--object-path",
            "/io/github/oksyd/Spotlit/LockScreen",
            "--method",
        ])
        .arg(format!("io.github.oksyd.Spotlit.LockScreen1.{method}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_runtime_status(&String::from_utf8_lossy(&output.stdout))
}

fn parse_runtime_status(value: &str) -> Option<RuntimeStatus> {
    let json = parse_dbus_string(value)?;
    let status: RuntimeStatus = serde_json::from_str(&json).ok()?;
    let valid_revision =
        status.revision.len() == 64 && status.revision.bytes().all(|byte| byte.is_ascii_hexdigit());
    (status.protocol == 1
        && (if status.active {
            valid_revision
        } else {
            status.revision.is_empty()
        })
        && (!status.current || (status.active && status.result == RuntimeResult::Ready)))
        .then_some(status)
}

fn current_shell_session() -> Result<String> {
    let bus_id = dbus_string_call("org.freedesktop.DBus.GetId", None)?;
    let owner = dbus_string_call("org.freedesktop.DBus.GetNameOwner", Some("org.gnome.Shell"))?;
    Ok(format!("{bus_id}/{owner}"))
}

fn dbus_string_call(method: &str, argument: Option<&str>) -> Result<String> {
    let mut command = Command::new("gdbus");
    command.args([
        "call",
        "--session",
        "--timeout",
        "3",
        "--dest",
        "org.freedesktop.DBus",
        "--object-path",
        "/org/freedesktop/DBus",
        "--method",
        method,
    ]);
    if let Some(argument) = argument {
        command.arg(argument);
    }
    let output = command
        .output()
        .map_err(|error| SpotlitError::platform(format!("query GNOME session: {error}")))?;
    if !output.status.success() {
        return Err(command_error("query GNOME session", &output));
    }
    parse_dbus_string(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| SpotlitError::platform("GNOME session response is invalid"))
}

fn parse_dbus_string(value: &str) -> Option<String> {
    value
        .trim()
        .strip_prefix("('")?
        .strip_suffix("',)")
        .map(str::to_string)
}

fn extension_list(extra_args: &[&str]) -> Result<Vec<String>> {
    let output = Command::new("gnome-extensions")
        .arg("list")
        .args(extra_args)
        .output()
        .map_err(|source| SpotlitError::platform(format!("query GNOME extensions: {source}")))?;
    if !output.status.success() {
        return Err(command_error("query GNOME extensions", &output));
    }

    Ok(parse_extension_list(&output.stdout))
}

fn parse_extension_list(output: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn installed_extension_dir() -> Option<PathBuf> {
    extension_data_dirs()
        .into_iter()
        .map(|dir| {
            dir.join("gnome-shell")
                .join("extensions")
                .join(GNOME_EXTENSION_UUID)
        })
        .find(|dir| dir.join("metadata.json").is_file())
}

fn extension_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data_home) = env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        dirs.push(PathBuf::from(data_home));
    } else if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share"));
    }

    if let Some(data_dirs) = env::var_os("XDG_DATA_DIRS").filter(|value| !value.is_empty()) {
        dirs.extend(env::split_paths(&data_dirs));
    } else {
        dirs.extend([
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ]);
    }
    dirs
}

fn read_blur_mode(schema_dir: &Path) -> Result<LockScreenBlurMode> {
    let value = read_extension_preference(schema_dir, "blur-mode", "lock screen blur")?;
    parse_blur_mode(&value).ok_or_else(|| {
        SpotlitError::platform("GNOME lock screen blur setting has an invalid value")
    })
}

fn read_display_mode(schema_dir: &Path) -> Result<LockScreenDisplayMode> {
    let value =
        read_extension_preference(schema_dir, "display-mode", "lock screen display policy")?;
    parse_display_mode(&value).ok_or_else(|| {
        SpotlitError::platform("GNOME lock screen display setting has an invalid value")
    })
}

fn read_extension_preference(schema_dir: &Path, key: &str, description: &str) -> Result<String> {
    let output = Command::new("gsettings")
        .arg("--schemadir")
        .arg(schema_dir)
        .arg("get")
        .arg(EXTENSION_SCHEMA)
        .arg(key)
        .output()
        .map_err(|source| SpotlitError::platform(format!("query {description}: {source}")))?;
    if !output.status.success() {
        return Err(command_error(&format!("query {description}"), &output));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_blur_mode(value: &str) -> Option<LockScreenBlurMode> {
    match value.trim().trim_matches(['\'', '"']) {
        "system" => Some(LockScreenBlurMode::System),
        "soft" => Some(LockScreenBlurMode::Soft),
        "clear" => Some(LockScreenBlurMode::Clear),
        _ => None,
    }
}

fn blur_mode_value(mode: LockScreenBlurMode) -> &'static str {
    match mode {
        LockScreenBlurMode::System => "system",
        LockScreenBlurMode::Soft => "soft",
        LockScreenBlurMode::Clear => "clear",
    }
}

fn parse_display_mode(value: &str) -> Option<LockScreenDisplayMode> {
    match value.trim().trim_matches(['\'', '"']) {
        "system" => Some(LockScreenDisplayMode::System),
        "keep-on-ac" => Some(LockScreenDisplayMode::PluggedIn),
        "keep-on" => Some(LockScreenDisplayMode::Always),
        _ => None,
    }
}

fn display_mode_value(mode: LockScreenDisplayMode) -> &'static str {
    match mode {
        LockScreenDisplayMode::System => "system",
        LockScreenDisplayMode::PluggedIn => "keep-on-ac",
        LockScreenDisplayMode::Always => "keep-on",
    }
}

fn materialize_extension(work_dir: &Path) -> Result<PathBuf> {
    let source_dir = work_dir.join(GNOME_EXTENSION_UUID);
    let schema_dir = source_dir.join("schemas");
    fs::create_dir_all(&schema_dir).map_err(|source| SpotlitError::io(&schema_dir, source))?;

    write_source(&source_dir.join("metadata.json"), METADATA)?;
    write_source(&source_dir.join("extension.js"), EXTENSION)?;
    write_source(&source_dir.join("runtimeHost.js"), RUNTIME_HOST)?;
    write_source(&source_dir.join("runtime.js"), RUNTIME)?;
    write_source(&source_dir.join("prefs.js"), PREFERENCES)?;
    write_source(&source_dir.join("runtime.css"), STYLESHEET)?;
    // Older development bundles used an automatically loaded Shell stylesheet.
    let legacy_stylesheet = source_dir.join("stylesheet.css");
    if legacy_stylesheet.exists() {
        fs::remove_file(&legacy_stylesheet)
            .map_err(|error| SpotlitError::io(&legacy_stylesheet, error))?;
    }
    write_source(&source_dir.join("wallpaperInfo.js"), WALLPAPER_INFO)?;
    write_source(
        &schema_dir.join("org.gnome.shell.extensions.spotlit-lock-screen.gschema.xml"),
        SCHEMA,
    )?;
    Ok(source_dir)
}

fn write_source(path: &Path, source: &str) -> Result<()> {
    fs::write(path, source).map_err(|error| SpotlitError::io(path, error))
}

fn run_checked(command: &mut Command, description: &str) -> Result<()> {
    let output = command
        .output()
        .map_err(|source| SpotlitError::platform(format!("{description}: {source}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error(description, &output))
    }
}

fn command_error(description: &str, output: &Output) -> SpotlitError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        SpotlitError::platform(format!("{description} failed"))
    } else {
        SpotlitError::platform(format!("{description} failed: {stderr}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        METADATA, RuntimeResult, RuntimeStatus, extension_needs_update, materialize_extension,
        parse_blur_mode, parse_dbus_string, parse_display_mode, parse_extension_list,
        parse_runtime_status, runtime_integration_state,
    };
    use crate::platform::{LockScreenBlurMode, LockScreenDisplayMode, LockScreenIntegrationState};

    #[test]
    fn extension_update_detection_checks_payload_without_downgrading_newer_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        let work_dir = std::env::temp_dir().join(format!(
            "spotlit-extension-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let dir = materialize_extension(&work_dir)?;
        assert!(!extension_needs_update(&dir));
        for file in [
            "runtimeHost.js",
            "runtime.js",
            "runtime.css",
            "wallpaperInfo.js",
        ] {
            assert!(dir.join(file).is_file(), "missing runtime payload: {file}");
            std::fs::write(dir.join(file), "outdated payload")?;
            assert!(
                extension_needs_update(&dir),
                "did not detect changed {file}"
            );
            materialize_extension(&work_dir)?;
        }
        std::fs::write(dir.join("stylesheet.css"), "legacy stylesheet")?;
        materialize_extension(&work_dir)?;
        assert!(!dir.join("stylesheet.css").exists());
        let mut metadata: serde_json::Value = serde_json::from_str(METADATA)?;
        std::fs::write(dir.join("metadata.json"), serde_json::to_vec(&metadata)?)?;
        assert!(!extension_needs_update(&dir));
        std::fs::write(dir.join("extension.js"), "old extension source")?;
        assert!(extension_needs_update(&dir));
        metadata["version"] =
            serde_json::json!(metadata["version"].as_u64().expect("bundled version") + 1);
        std::fs::write(dir.join("metadata.json"), serde_json::to_vec(&metadata)?)?;
        assert!(!extension_needs_update(&dir));
        std::fs::remove_dir_all(work_dir)?;
        Ok(())
    }

    #[test]
    fn session_identity_parser_accepts_only_single_string_responses() {
        assert_eq!(parse_dbus_string("(':1.42',)\n"), Some(":1.42".to_string()));
        assert_eq!(parse_dbus_string("('bus-id',)"), Some("bus-id".to_string()));
        assert!(parse_dbus_string("()").is_none());
        assert!(parse_dbus_string("unstructured response").is_none());
    }

    #[test]
    fn runtime_protocol_validates_acknowledgements() {
        let ready = serde_json::json!({
            "protocol": 1, "result": "ready", "active": true,
            "current": true, "revision": "a".repeat(64),
        });
        let parse = |value: &serde_json::Value| parse_runtime_status(&format!("('{value}',)\n"));
        assert_eq!(
            parse(&ready).expect("valid reply").result,
            RuntimeResult::Ready
        );
        for (field, value) in [
            ("protocol", serde_json::json!(2)),
            ("result", serde_json::json!("unknown")),
            ("result", serde_json::json!("pending")),
            ("active", serde_json::json!(false)),
            ("revision", serde_json::json!("a".repeat(63))),
            ("revision", serde_json::json!("z".repeat(64))),
        ] {
            let mut invalid = ready.clone();
            invalid[field] = value;
            assert!(parse(&invalid).is_none(), "accepted invalid {field}");
        }
        for result in ["pending", "busy", "failed", "restart-required"] {
            let mut reply = ready.clone();
            reply["result"] = serde_json::json!(result);
            reply["current"] = serde_json::json!(false);
            assert!(parse(&reply).is_some(), "rejected {result}");
        }
        assert!(
            parse(&serde_json::json!({
                "protocol": 1, "result": "inactive", "active": false,
                "current": false, "revision": "",
            }))
            .is_some()
        );
        assert!(parse_runtime_status("('not json',)").is_none());
        assert!(parse_runtime_status("()").is_none());
    }

    #[test]
    fn runtime_acknowledgement_supersedes_stale_install_marker() {
        use LockScreenIntegrationState::{Enabled, RestartRequired};
        let current = RuntimeStatus {
            protocol: 1,
            result: RuntimeResult::Ready,
            active: true,
            current: true,
            revision: "a".repeat(64),
        };
        for update_available in [false, true] {
            assert_eq!(
                runtime_integration_state(Enabled, update_available, true, Some(&current)),
                (Enabled, update_available)
            );
        }
        assert_eq!(
            runtime_integration_state(Enabled, false, true, None),
            (RestartRequired, false)
        );
        assert_eq!(
            runtime_integration_state(Enabled, false, false, None),
            (Enabled, false)
        );
    }

    #[test]
    fn runtime_state_keeps_pending_updates_retryable_without_claiming_failed_runtime_is_ready() {
        use LockScreenIntegrationState::{Enabled, RestartRequired};
        for result in [
            RuntimeResult::Pending,
            RuntimeResult::Busy,
            RuntimeResult::Failed,
        ] {
            let runtime = RuntimeStatus {
                protocol: 1,
                result,
                active: true,
                current: false,
                revision: "a".repeat(64),
            };
            assert_eq!(
                runtime_integration_state(Enabled, false, true, Some(&runtime)),
                (Enabled, true)
            );
        }
        for active in [false, true] {
            let runtime = RuntimeStatus {
                protocol: 1,
                result: RuntimeResult::RestartRequired,
                active,
                current: false,
                revision: if active {
                    "a".repeat(64)
                } else {
                    String::new()
                },
            };
            assert_eq!(
                runtime_integration_state(Enabled, false, false, Some(&runtime)),
                (RestartRequired, false)
            );
        }
        let inactive = RuntimeStatus {
            protocol: 1,
            result: RuntimeResult::Failed,
            active: false,
            current: false,
            revision: String::new(),
        };
        assert_eq!(
            runtime_integration_state(Enabled, false, false, Some(&inactive)),
            (RestartRequired, false)
        );
    }

    #[test]
    fn extension_list_uses_exact_non_empty_lines() {
        assert_eq!(
            parse_extension_list(b"ubuntu-dock@ubuntu.com\n lock-screen@spotlit.app \n\n"),
            ["ubuntu-dock@ubuntu.com", "lock-screen@spotlit.app"]
        );
    }

    #[test]
    fn blur_mode_parser_accepts_gsettings_strings() {
        assert_eq!(
            parse_blur_mode("'system'\n"),
            Some(LockScreenBlurMode::System)
        );
        assert_eq!(parse_blur_mode("'soft'"), Some(LockScreenBlurMode::Soft));
        assert_eq!(
            parse_blur_mode("\"clear\""),
            Some(LockScreenBlurMode::Clear)
        );
        assert_eq!(parse_blur_mode("'unknown'"), None);
    }

    #[test]
    fn display_mode_parser_accepts_gsettings_strings() {
        assert_eq!(
            parse_display_mode("'system'\n"),
            Some(LockScreenDisplayMode::System)
        );
        assert_eq!(
            parse_display_mode("'keep-on-ac'"),
            Some(LockScreenDisplayMode::PluggedIn)
        );
        assert_eq!(
            parse_display_mode("\"keep-on\""),
            Some(LockScreenDisplayMode::Always)
        );
        assert_eq!(parse_display_mode("'unknown'"), None);
    }
}
