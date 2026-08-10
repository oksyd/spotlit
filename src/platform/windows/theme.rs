use crate::core::Result;

use super::registry::read_hkcu_dword;

const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const APPS_USE_LIGHT_THEME: &str = "AppsUseLightTheme";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SystemTheme {
    Light,
    Dark,
}

pub fn system_theme() -> Result<SystemTheme> {
    match read_hkcu_dword(PERSONALIZE_KEY, APPS_USE_LIGHT_THEME)? {
        Some(0) => Ok(SystemTheme::Dark),
        _ => Ok(SystemTheme::Light),
    }
}
