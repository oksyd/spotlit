use std::sync::Mutex;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::core::{AppPaths, DesktopSpotlightCreative, Result, SpotlightMetadata, SpotlitError};

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(windows)]
pub(crate) mod windows;

#[cfg(target_os = "linux")]
use self::linux::{
    GnomeLockScreen, StartupRegistration as LinuxStartupRegistration,
    StartupState as LinuxStartupState, SystemTheme as LinuxSystemTheme,
};
#[cfg(windows)]
use self::windows::{
    StartupRegistration, StartupState as WindowsStartupState, SystemTheme as WindowsSystemTheme,
    WindowsLockScreen,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StartupState {
    Enabled,
    Disabled,
}

impl StartupState {
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[cfg(windows)]
impl From<WindowsStartupState> for StartupState {
    fn from(value: WindowsStartupState) -> Self {
        match value {
            WindowsStartupState::Enabled => Self::Enabled,
            WindowsStartupState::Disabled => Self::Disabled,
        }
    }
}

#[cfg(target_os = "linux")]
impl From<LinuxStartupState> for StartupState {
    fn from(value: LinuxStartupState) -> Self {
        match value {
            LinuxStartupState::Enabled => Self::Enabled,
            LinuxStartupState::Disabled => Self::Disabled,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SystemTheme {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Default, Eq, Hash, PartialEq)]
pub enum LockScreenIntegrationState {
    #[default]
    Unsupported,
    Unavailable,
    NotInstalled,
    RestartRequired,
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, Default, Eq, Hash, PartialEq)]
pub enum LockScreenBlurMode {
    #[default]
    System,
    Soft,
    Clear,
}

#[derive(Debug, Clone, Copy, Default, Eq, Hash, PartialEq)]
pub enum LockScreenDisplayMode {
    #[default]
    System,
    PluggedIn,
    Always,
}

#[derive(Debug, Clone, Copy, Default, Eq, Hash, PartialEq)]
pub struct LockScreenIntegration {
    pub state: LockScreenIntegrationState,
    pub blur_mode: LockScreenBlurMode,
    pub display_mode: LockScreenDisplayMode,
}

#[cfg(windows)]
impl From<WindowsSystemTheme> for SystemTheme {
    fn from(value: WindowsSystemTheme) -> Self {
        match value {
            WindowsSystemTheme::Light => Self::Light,
            WindowsSystemTheme::Dark => Self::Dark,
        }
    }
}

#[cfg(target_os = "linux")]
impl From<LinuxSystemTheme> for SystemTheme {
    fn from(value: LinuxSystemTheme) -> Self {
        match value {
            LinuxSystemTheme::Light => Self::Light,
            LinuxSystemTheme::Dark => Self::Dark,
        }
    }
}

pub trait LockScreenService: Send + Sync {
    fn set_lock_screen_wallpaper(&self, image_path: &Path) -> Result<()>;
}

pub trait PlatformServices: Send + Sync {
    fn current_desktop_wallpaper(&self) -> Result<Option<PathBuf>>;
    fn desktop_spotlight_creatives(&self) -> Result<Vec<DesktopSpotlightCreative>>;
    fn current_desktop_spotlight_metadata(&self) -> Result<Option<SpotlightMetadata>>;
    fn open_path(&self, path: &Path) -> Result<()>;
    fn open_url_in_chrome(&self, url: &str) -> Result<()>;
    fn reveal_path(&self, path: &Path) -> Result<()>;
    #[allow(dead_code)]
    fn lock_workstation(&self) -> Result<()>;
    fn pick_wallpaper_image(&self) -> Result<Option<PathBuf>>;
    fn pick_export_image_path(&self, default_file_name: &str) -> Result<Option<PathBuf>>;
    fn startup_state(&self) -> Result<StartupState>;
    fn set_startup_enabled(&self, enabled: bool) -> Result<StartupState>;
    fn system_theme(&self) -> Result<SystemTheme>;
    fn lock_screen_integration(&self) -> Result<LockScreenIntegration> {
        Ok(LockScreenIntegration::default())
    }
    fn install_lock_screen_integration(&self) -> Result<LockScreenIntegration> {
        Err(SpotlitError::platform(
            "lock screen integration is unavailable on this platform",
        ))
    }
    fn set_lock_screen_integration_enabled(&self, _enabled: bool) -> Result<LockScreenIntegration> {
        Err(SpotlitError::platform(
            "lock screen integration is unavailable on this platform",
        ))
    }
    fn set_lock_screen_blur_mode(
        &self,
        _mode: LockScreenBlurMode,
    ) -> Result<LockScreenIntegration> {
        Err(SpotlitError::platform(
            "lock screen integration is unavailable on this platform",
        ))
    }
    fn set_lock_screen_display_mode(
        &self,
        _mode: LockScreenDisplayMode,
    ) -> Result<LockScreenIntegration> {
        Err(SpotlitError::platform(
            "lock screen integration is unavailable on this platform",
        ))
    }
}

pub fn app_paths() -> Result<AppPaths> {
    #[cfg(windows)]
    {
        windows::app_paths()
    }

    #[cfg(target_os = "linux")]
    {
        linux::app_paths()
    }
}

pub fn wallpaper_source_dirs(paths: &AppPaths) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let _ = paths;
        windows::spotlight_source_dirs()
    }

    #[cfg(target_os = "linux")]
    {
        linux::wallpaper_source_dirs(paths)
    }
}

pub fn wallpaper_target() -> Arc<dyn LockScreenService> {
    #[cfg(windows)]
    {
        Arc::new(WindowsLockScreen)
    }

    #[cfg(target_os = "linux")]
    {
        Arc::new(GnomeLockScreen)
    }
}

pub fn wallpaper_apply_target_label() -> &'static str {
    #[cfg(windows)]
    {
        "Lock Screen"
    }

    #[cfg(target_os = "linux")]
    {
        "Lock Screen"
    }
}

pub fn platform_services(paths: &AppPaths) -> Arc<dyn PlatformServices> {
    #[cfg(windows)]
    {
        let _ = paths;
        Arc::new(WindowsServices::new())
    }

    #[cfg(target_os = "linux")]
    {
        Arc::new(LinuxServices::new(
            linux::bing_wallpaper_dir(paths),
            paths.data_dir.join("gnome-extension"),
        ))
    }
}

pub fn acquire_single_instance() -> Result<Option<SingleInstanceGuard>> {
    SingleInstanceGuard::acquire()
}

pub fn enter_background_thread_mode() {
    #[cfg(windows)]
    {
        windows::enter_background_thread_mode();
    }
}

pub struct SingleInstanceGuard {
    #[cfg(windows)]
    inner: windows::SingleInstanceGuard,
    #[cfg(target_os = "linux")]
    inner: linux::SingleInstanceGuard,
}

impl SingleInstanceGuard {
    fn acquire() -> Result<Option<Self>> {
        #[cfg(windows)]
        {
            return windows::SingleInstanceGuard::acquire()
                .map(|inner| inner.map(|inner| Self { inner }));
        }

        #[cfg(target_os = "linux")]
        {
            linux::SingleInstanceGuard::acquire().map(|inner| inner.map(|inner| Self { inner }))
        }
    }

    pub fn start_activation_listener<F>(&self, on_activate: F) -> Result<()>
    where
        F: Fn() + Send + Sync + 'static,
    {
        #[cfg(windows)]
        {
            self.inner.start_activation_listener(on_activate)
        }

        #[cfg(target_os = "linux")]
        {
            self.inner.start_activation_listener(on_activate)
        }
    }
}

#[cfg(windows)]
impl LockScreenService for WindowsLockScreen {
    fn set_lock_screen_wallpaper(&self, image_path: &Path) -> Result<()> {
        WindowsLockScreen::set_lock_screen_wallpaper(self, image_path)
    }
}

#[cfg(target_os = "linux")]
impl LockScreenService for GnomeLockScreen {
    fn set_lock_screen_wallpaper(&self, image_path: &Path) -> Result<()> {
        GnomeLockScreen::set_lock_screen_wallpaper(self, image_path)
    }
}

#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct WindowsServices {
    startup: Arc<Mutex<Option<StartupRegistration>>>,
}

#[cfg(windows)]
impl WindowsServices {
    pub fn new() -> Self {
        Self {
            startup: Arc::new(Mutex::new(None)),
        }
    }

    fn startup_registration(&self) -> Result<StartupRegistration> {
        let mut startup = self
            .startup
            .lock()
            .map_err(crate::core::SpotlitError::platform)?;
        if let Some(startup) = startup.as_ref() {
            return Ok(startup.clone());
        }

        let registration = StartupRegistration::for_current_exe()?;
        *startup = Some(registration.clone());
        Ok(registration)
    }
}

#[cfg(windows)]
impl PlatformServices for WindowsServices {
    fn current_desktop_wallpaper(&self) -> Result<Option<PathBuf>> {
        windows::current_desktop_wallpaper()
    }

    fn desktop_spotlight_creatives(&self) -> Result<Vec<DesktopSpotlightCreative>> {
        windows::desktop_spotlight_creatives()
    }

    fn current_desktop_spotlight_metadata(&self) -> Result<Option<SpotlightMetadata>> {
        windows::current_desktop_spotlight_metadata()
    }

    fn open_path(&self, path: &Path) -> Result<()> {
        windows::open_path(path)
    }

    fn open_url_in_chrome(&self, url: &str) -> Result<()> {
        windows::open_url_in_chrome(url)
    }

    fn reveal_path(&self, path: &Path) -> Result<()> {
        windows::reveal_path(path)
    }

    fn lock_workstation(&self) -> Result<()> {
        windows::lock_workstation()
    }

    fn pick_wallpaper_image(&self) -> Result<Option<PathBuf>> {
        windows::pick_wallpaper_image()
    }

    fn pick_export_image_path(&self, default_file_name: &str) -> Result<Option<PathBuf>> {
        windows::pick_export_image_path(default_file_name)
    }

    fn startup_state(&self) -> Result<StartupState> {
        self.startup_registration()?.state().map(Into::into)
    }

    fn set_startup_enabled(&self, enabled: bool) -> Result<StartupState> {
        self.startup_registration()?
            .set_enabled(enabled)
            .map(Into::into)
    }

    fn system_theme(&self) -> Result<SystemTheme> {
        windows::system_theme().map(Into::into)
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct LinuxServices {
    bing_dir: PathBuf,
    extension_work_dir: PathBuf,
    startup: Arc<Mutex<Option<LinuxStartupRegistration>>>,
}

#[cfg(target_os = "linux")]
impl LinuxServices {
    pub fn new(bing_dir: PathBuf, extension_work_dir: PathBuf) -> Self {
        Self {
            bing_dir,
            extension_work_dir,
            startup: Arc::new(Mutex::new(None)),
        }
    }

    fn startup_registration(&self) -> Result<LinuxStartupRegistration> {
        let mut startup = self
            .startup
            .lock()
            .map_err(crate::core::SpotlitError::platform)?;
        if let Some(startup) = startup.as_ref() {
            return Ok(startup.clone());
        }

        let registration = LinuxStartupRegistration::for_current_exe()?;
        *startup = Some(registration.clone());
        Ok(registration)
    }
}

#[cfg(target_os = "linux")]
impl PlatformServices for LinuxServices {
    fn current_desktop_wallpaper(&self) -> Result<Option<PathBuf>> {
        linux::current_desktop_wallpaper()
    }

    fn desktop_spotlight_creatives(&self) -> Result<Vec<DesktopSpotlightCreative>> {
        linux::refresh_bing_wallpapers(&self.bing_dir)
    }

    fn current_desktop_spotlight_metadata(&self) -> Result<Option<SpotlightMetadata>> {
        Ok(None)
    }

    fn open_path(&self, path: &Path) -> Result<()> {
        linux::open_path(path)
    }

    fn open_url_in_chrome(&self, url: &str) -> Result<()> {
        linux::open_url_in_chrome(url)
    }

    fn reveal_path(&self, path: &Path) -> Result<()> {
        linux::reveal_path(path)
    }

    fn lock_workstation(&self) -> Result<()> {
        linux::lock_workstation()
    }

    fn pick_wallpaper_image(&self) -> Result<Option<PathBuf>> {
        linux::pick_wallpaper_image()
    }

    fn pick_export_image_path(&self, default_file_name: &str) -> Result<Option<PathBuf>> {
        linux::pick_export_image_path(default_file_name)
    }

    fn startup_state(&self) -> Result<StartupState> {
        self.startup_registration()?.state().map(Into::into)
    }

    fn set_startup_enabled(&self, enabled: bool) -> Result<StartupState> {
        self.startup_registration()?
            .set_enabled(enabled)
            .map(Into::into)
    }

    fn system_theme(&self) -> Result<SystemTheme> {
        linux::system_theme().map(Into::into)
    }

    fn lock_screen_integration(&self) -> Result<LockScreenIntegration> {
        linux::lock_screen_integration()
    }

    fn install_lock_screen_integration(&self) -> Result<LockScreenIntegration> {
        linux::install_lock_screen_integration(&self.extension_work_dir)
    }

    fn set_lock_screen_integration_enabled(&self, enabled: bool) -> Result<LockScreenIntegration> {
        linux::set_lock_screen_integration_enabled(enabled)
    }

    fn set_lock_screen_blur_mode(&self, mode: LockScreenBlurMode) -> Result<LockScreenIntegration> {
        linux::set_lock_screen_blur_mode(mode)
    }

    fn set_lock_screen_display_mode(
        &self,
        mode: LockScreenDisplayMode,
    ) -> Result<LockScreenIntegration> {
        linux::set_lock_screen_display_mode(mode)
    }
}
