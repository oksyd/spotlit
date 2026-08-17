use std::sync::atomic::{AtomicU8, Ordering};

use slint::{ComponentHandle, SharedString};

use crate::{I18n, MainWindow, core::LanguageMode};

const ENGLISH: u8 = 0;
const SIMPLIFIED_CHINESE: u8 = 1;
const GERMAN: u8 = 2;
static ACTIVE_LANGUAGE: AtomicU8 = AtomicU8::new(ENGLISH);

pub(crate) fn initialize_system_language() {
    let language = effective_language(LanguageMode::System, sys_locale::get_locales());
    ACTIVE_LANGUAGE.store(language_id(language), Ordering::Release);
}

pub(crate) fn select_language(language: LanguageMode) {
    let language = effective_language(language, sys_locale::get_locales());
    ACTIVE_LANGUAGE.store(language_id(language), Ordering::Release);
    if let Err(error) = slint::select_bundled_translation(language) {
        tracing::warn!(%error, language, "failed to select bundled translation");
    }
}

const fn language_id(language: &str) -> u8 {
    match language.as_bytes() {
        b"zh-CN" => SIMPLIFIED_CHINESE,
        b"de" => GERMAN,
        _ => ENGLISH,
    }
}

pub(crate) fn file_dialog_images() -> &'static str {
    localized("Images", "图片", "Bilder")
}

#[cfg(windows)]
pub(crate) fn file_dialog_all_files() -> &'static str {
    localized("All Files", "所有文件", "Alle Dateien")
}

pub(crate) fn import_wallpaper_dialog_title() -> &'static str {
    localized(
        "Import Wallpaper",
        "导入壁纸",
        "Hintergrundbild importieren",
    )
}

pub(crate) fn export_wallpaper_dialog_title() -> &'static str {
    localized(
        "Export Wallpaper",
        "导出壁纸",
        "Hintergrundbild exportieren",
    )
}

fn localized(english: &'static str, chinese: &'static str, german: &'static str) -> &'static str {
    localized_for(
        ACTIVE_LANGUAGE.load(Ordering::Acquire),
        english,
        chinese,
        german,
    )
}

fn localized_for(
    language: u8,
    english: &'static str,
    chinese: &'static str,
    german: &'static str,
) -> &'static str {
    match language {
        SIMPLIFIED_CHINESE => chinese,
        GERMAN => german,
        _ => english,
    }
}

fn effective_language(
    language: LanguageMode,
    system_locales: impl IntoIterator<Item = String>,
) -> &'static str {
    match language {
        LanguageMode::English => "en",
        LanguageMode::SimplifiedChinese => "zh-CN",
        LanguageMode::German => "de",
        LanguageMode::System => system_locales
            .into_iter()
            .find_map(|locale| supported_language(&locale))
            .unwrap_or("en"),
    }
}

fn supported_language(locale: &str) -> Option<&'static str> {
    let language = locale
        .split(['-', '_', '@'])
        .next()
        .unwrap_or(locale)
        .to_ascii_lowercase();
    match language.as_str() {
        "de" => Some("de"),
        "zh" => Some("zh-CN"),
        "en" => Some("en"),
        _ => None,
    }
}

pub(crate) fn message(app: &MainWindow, value: &str) -> SharedString {
    let i18n = app.global::<I18n>();

    if let Some(error) = value.strip_prefix("Update check failed: ") {
        return i18n.invoke_update_check_failed(error.into());
    }
    if let Some(error) = value.strip_prefix("Update download failed: ") {
        return i18n.invoke_update_download_failed(error.into());
    }
    if let Some(error) = value.strip_prefix("Update install failed: ") {
        return i18n.invoke_update_install_failed(error.into());
    }
    if let Some(error) = value.strip_prefix("Failed: ") {
        return i18n.invoke_failed(error.into());
    }
    if let Some(count) = value
        .strip_prefix("Cache cleaned: ")
        .and_then(|value| value.strip_suffix(" wallpapers removed"))
    {
        return i18n.invoke_cache_cleaned(count.into());
    }
    if let Some(count) = value
        .strip_prefix("History limit saved: ")
        .and_then(|value| value.strip_suffix(" wallpapers removed"))
    {
        return i18n.invoke_history_limit_removed(count.into());
    }
    if let Some(value) = value.strip_prefix("Exported to ") {
        return i18n.invoke_exported(value.into());
    }
    if let Some(value) = value.strip_prefix("Imported ") {
        return i18n.invoke_imported(value.into());
    }
    if let Some(value) = value.strip_prefix("Removed ") {
        return i18n.invoke_removed(value.into());
    }
    if let Some(value) = value.strip_prefix("Opened ")
        && !matches!(
            value,
            "current image"
                | "data folder"
                | "favorites folder"
                | "logs folder"
                | "Spotlit release page"
                | "wallpaper info"
        )
    {
        return i18n.invoke_opened(value.into());
    }

    i18n.invoke_message(value.into())
}

pub(crate) fn wallpaper_count(app: &MainWindow, count: usize) -> SharedString {
    app.global::<I18n>()
        .invoke_wallpaper_count(i32::try_from(count).unwrap_or(i32::MAX))
}

pub(crate) fn favorite_count(app: &MainWindow, count: usize) -> SharedString {
    app.global::<I18n>()
        .invoke_favorite_count(i32::try_from(count).unwrap_or(i32::MAX))
}

pub(crate) fn sync_time(app: &MainWindow, timestamp: &str) -> SharedString {
    app.global::<I18n>().invoke_sync_time(timestamp.into())
}

#[cfg(test)]
mod tests {
    use super::{
        ENGLISH, GERMAN, SIMPLIFIED_CHINESE, effective_language, language_id, localized_for,
        supported_language,
    };
    use crate::core::LanguageMode;

    #[test]
    fn explicit_languages_do_not_depend_on_system_locale() {
        assert_eq!(
            effective_language(LanguageMode::English, ["de-DE".into()]),
            "en"
        );
        assert_eq!(
            effective_language(LanguageMode::SimplifiedChinese, ["de-DE".into()]),
            "zh-CN"
        );
        assert_eq!(
            effective_language(LanguageMode::German, ["zh-CN".into()]),
            "de"
        );
    }

    #[test]
    fn system_language_uses_supported_preference_or_english() {
        assert_eq!(
            effective_language(LanguageMode::System, ["fr-FR".into(), "de-DE".into()]),
            "de"
        );
        assert_eq!(
            effective_language(LanguageMode::System, ["zh_TW".into()]),
            "zh-CN"
        );
        assert_eq!(
            effective_language(LanguageMode::System, ["fr-FR".into()]),
            "en"
        );
    }

    #[test]
    fn locale_matching_uses_the_language_subtag() {
        assert_eq!(supported_language("de-DE"), Some("de"));
        assert_eq!(supported_language("zh_CN.UTF-8"), Some("zh-CN"));
        assert_eq!(supported_language("en_US"), Some("en"));
        assert_eq!(supported_language("fr-FR"), None);
    }

    #[test]
    fn native_dialog_text_uses_the_selected_interface_language() {
        assert_eq!(localized_for(ENGLISH, "Images", "图片", "Bilder"), "Images");
        assert_eq!(
            localized_for(SIMPLIFIED_CHINESE, "Images", "图片", "Bilder"),
            "图片"
        );
        assert_eq!(localized_for(GERMAN, "Images", "图片", "Bilder"), "Bilder");
        assert_eq!(language_id("zh-CN"), SIMPLIFIED_CHINESE);
    }
}
