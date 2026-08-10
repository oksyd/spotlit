use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::core::{Result, SpotlitError};

const APP_NAME: &str = "Spotlit";
const AUTOSTART_FILE_NAME: &str = "spotlit.desktop";
const BACKGROUND_FLAG: &str = "--background";
const XDG_CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";
const HOME_ENV: &str = "HOME";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StartupState {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone)]
pub struct StartupRegistration {
    app_name: String,
    command_path: PathBuf,
    desktop_entry_path: PathBuf,
}

impl StartupRegistration {
    pub fn for_current_exe() -> Result<Self> {
        let command_path = env::current_exe()
            .map_err(|source| SpotlitError::io("current executable path", source))?;
        Ok(Self::new(command_path, autostart_file_path()?))
    }

    fn new(command_path: PathBuf, desktop_entry_path: PathBuf) -> Self {
        Self {
            app_name: APP_NAME.to_string(),
            command_path,
            desktop_entry_path,
        }
    }

    pub fn state(&self) -> Result<StartupState> {
        let contents = match fs::read_to_string(&self.desktop_entry_path) {
            Ok(contents) => contents,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(StartupState::Disabled);
            }
            Err(source) => return Err(SpotlitError::io(&self.desktop_entry_path, source)),
        };

        if desktop_entry_bool(&contents, "Hidden") == Some(true)
            || desktop_entry_bool(&contents, "X-GNOME-Autostart-enabled") == Some(false)
        {
            return Ok(StartupState::Disabled);
        }

        if desktop_entry_exec_line(&contents)
            .is_some_and(|exec| exec == self.command_line() || exec == self.legacy_command_line())
        {
            Ok(StartupState::Enabled)
        } else {
            Ok(StartupState::Disabled)
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<StartupState> {
        if enabled {
            write_desktop_entry(&self.desktop_entry_path, &self.desktop_entry())?;
        } else {
            match fs::remove_file(&self.desktop_entry_path) {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(SpotlitError::io(&self.desktop_entry_path, source)),
            }
        }

        self.state()
    }

    fn desktop_entry(&self) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName={}\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            self.app_name,
            self.command_line()
        )
    }

    fn command_line(&self) -> String {
        format!(
            "{} {BACKGROUND_FLAG}",
            quote_desktop_exec_path(&self.command_path)
        )
    }

    fn legacy_command_line(&self) -> String {
        quote_desktop_exec_path(&self.command_path)
    }
}

fn autostart_file_path() -> Result<PathBuf> {
    Ok(config_home()?.join("autostart").join(AUTOSTART_FILE_NAME))
}

fn config_home() -> Result<PathBuf> {
    if let Some(path) = env_path(XDG_CONFIG_HOME_ENV) {
        return Ok(path);
    }

    if let Some(home) = env_path(HOME_ENV) {
        return Ok(home.join(".config"));
    }

    Err(SpotlitError::platform(
        "neither XDG_CONFIG_HOME nor HOME is set",
    ))
}

fn env_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os(name).map(PathBuf::from)?;
    (!path.as_os_str().is_empty()).then_some(path)
}

fn write_desktop_entry(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SpotlitError::io(parent, source))?;
    }

    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, contents).map_err(|source| SpotlitError::io(&tmp_path, source))?;
    fs::rename(&tmp_path, path).map_err(|source| SpotlitError::io(path, source))
}

fn desktop_entry_exec_line(contents: &str) -> Option<&str> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("Exec=").map(str::trim))
}

fn desktop_entry_bool(contents: &str, key: &str) -> Option<bool> {
    let prefix = format!("{key}=");
    let value = contents
        .lines()
        .find_map(|line| line.strip_prefix(&prefix).map(str::trim))?;
    if value.eq_ignore_ascii_case("true") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn quote_desktop_exec_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for character in path.chars() {
        if matches!(character, '"' | '\\' | '$' | '`') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_contains_background_exec() {
        let registration = StartupRegistration::new(
            PathBuf::from("/opt/Spot Lit/spotlit"),
            PathBuf::from("/tmp/spotlit.desktop"),
        );

        let entry = registration.desktop_entry();

        assert!(entry.contains("Type=Application"));
        assert!(entry.contains("Name=Spotlit"));
        assert!(entry.contains("Exec=\"/opt/Spot Lit/spotlit\" --background"));
        assert!(entry.contains("X-GNOME-Autostart-enabled=true"));
    }

    #[test]
    fn desktop_exec_path_escapes_special_characters() {
        assert_eq!(
            quote_desktop_exec_path(Path::new("/opt/Spot\"Lit/$app`/spotlit")),
            "\"/opt/Spot\\\"Lit/\\$app\\`/spotlit\""
        );
    }

    #[test]
    fn desktop_entry_exec_line_reads_exec_value() {
        assert_eq!(
            desktop_entry_exec_line(
                "[Desktop Entry]\nName=Spotlit\nExec=\"/bin/spotlit\" --background\n"
            ),
            Some("\"/bin/spotlit\" --background")
        );
    }

    #[test]
    fn desktop_entry_bool_reads_case_insensitive_values() {
        let entry = "[Desktop Entry]\nHidden=TRUE\nX-GNOME-Autostart-enabled=false\n";

        assert_eq!(desktop_entry_bool(entry, "Hidden"), Some(true));
        assert_eq!(
            desktop_entry_bool(entry, "X-GNOME-Autostart-enabled"),
            Some(false)
        );
    }
}
