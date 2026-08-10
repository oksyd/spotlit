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
const METADATA: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/metadata.json");
const EXTENSION: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/extension.js");
const PREFERENCES: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/prefs.js");
const STYLESHEET: &str =
    include_str!("../../../extensions/gnome-shell/lock-screen@spotlit.app/stylesheet.css");
const SCHEMA: &str = include_str!(
    "../../../extensions/gnome-shell/lock-screen@spotlit.app/schemas/org.gnome.shell.extensions.spotlit-lock-screen.gschema.xml"
);

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
    }
}

fn integration_with_preferences(
    state: LockScreenIntegrationState,
    extension_dir: Option<&Path>,
) -> LockScreenIntegration {
    let schema_dir = extension_dir.map(|dir| dir.join("schemas"));
    let blur_mode = schema_dir
        .as_deref()
        .and_then(|dir| read_blur_mode(dir).ok())
        .unwrap_or_default();
    let display_mode = schema_dir
        .as_deref()
        .and_then(|dir| read_display_mode(dir).ok())
        .unwrap_or_default();
    LockScreenIntegration {
        state,
        blur_mode,
        display_mode,
    }
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
    write_source(&source_dir.join("prefs.js"), PREFERENCES)?;
    write_source(&source_dir.join("stylesheet.css"), STYLESHEET)?;
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
    use super::{parse_blur_mode, parse_display_mode, parse_extension_list};
    use crate::platform::{LockScreenBlurMode, LockScreenDisplayMode};

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
