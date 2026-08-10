use std::{path::PathBuf, sync::Arc, thread};

use crate::core::{Result, SpotlitError};

use crate::{SpotlitTray, platform::PlatformServices, window_manager};

const TRAY_THREAD_STACK_SIZE: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct TrayFolders {
    data: PathBuf,
}

impl TrayFolders {
    pub(crate) fn new(data: PathBuf) -> Self {
        Self { data }
    }
}

pub(crate) struct InstalledTray {
    _tray: SpotlitTray,
}

pub(crate) fn install_tray(
    services: Arc<dyn PlatformServices>,
    folders: TrayFolders,
) -> Result<InstalledTray> {
    let tray = SpotlitTray::new().map_err(SpotlitError::platform)?;

    tray.on_activate_requested(present_window());
    tray.on_open_data_folder_requested(open_folder(Arc::clone(&services), folders.data, "spotlit"));
    tray.on_lock_workstation_requested(lock_workstation(Arc::clone(&services)));
    tray.on_quit_requested(quit_app());
    tray.show().map_err(SpotlitError::platform)?;

    Ok(InstalledTray { _tray: tray })
}

fn present_window() -> impl Fn() + 'static {
    || {
        window_manager::present_window();
    }
}

fn lock_workstation(services: Arc<dyn PlatformServices>) -> impl Fn() + 'static {
    move || {
        let services = Arc::clone(&services);
        spawn_tray_action("spotlit-tray-lock-workstation", move || {
            if let Err(error) = services.lock_workstation() {
                tracing::warn!(
                    error = %error,
                    "failed to lock workstation from tray"
                );
            }
        });
    }
}

fn open_folder(
    services: Arc<dyn PlatformServices>,
    path: PathBuf,
    folder: &'static str,
) -> impl Fn() + 'static {
    move || {
        let services = Arc::clone(&services);
        let path = path.clone();
        spawn_tray_action("spotlit-tray-open-folder", move || {
            if let Err(error) = services.open_path(&path) {
                tracing::warn!(
                    error = %error,
                    folder,
                    "failed to open folder from tray"
                );
            }
        });
    }
}

fn quit_app() -> impl Fn() + 'static {
    || {
        let _ = slint::quit_event_loop();
    }
}

fn spawn_tray_action(name: &'static str, action: impl FnOnce() + Send + 'static) {
    if let Err(error) = thread::Builder::new()
        .name(name.to_string())
        .stack_size(TRAY_THREAD_STACK_SIZE)
        .spawn(action)
    {
        tracing::warn!(%error, name, "failed to spawn tray action");
    }
}
