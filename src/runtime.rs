use std::{env, path::PathBuf, rc::Rc, sync::Arc, thread, time::Duration};

use anyhow::Context;

use crate::{
    app_state,
    logging::init_tracing,
    options::AppOptions,
    platform::{self, PlatformServices},
    tray::{InstalledTray, TrayFolders, install_tray},
    ui_events::UiSink,
    window_manager,
};

type RuntimeHandle = Rc<RuntimeHandles>;
const LOGGING_STARTUP_DELAY: Duration = Duration::from_millis(160);
const ACTIVATION_LISTENER_STARTUP_DELAY: Duration = Duration::from_millis(180);
const SCHEDULER_STARTUP_DELAY: Duration = Duration::from_millis(260);
const BACKGROUND_STARTUP_THREAD_STACK_SIZE: usize = 256 * 1024;

struct RuntimeHandles {
    single_instance: platform::SingleInstanceGuard,
    _tray: Option<InstalledTray>,
}

impl RuntimeHandles {
    fn has_tray(&self) -> bool {
        self._tray.is_some()
    }
}

pub fn run_from_env() -> anyhow::Result<()> {
    run(env::args().skip(1))
}

pub fn run(args: impl IntoIterator<Item = String>) -> anyhow::Result<()> {
    let options = AppOptions::parse(args);
    crate::diagnostics::initialize();
    crate::i18n::initialize_system_language();

    let Some(single_instance) =
        platform::acquire_single_instance().context("failed to acquire single instance guard")?
    else {
        return Ok(());
    };

    let paths = platform::app_paths().context("failed to resolve app paths")?;

    let log_dir = paths.log_dir.clone();
    let tray_folders = TrayFolders::new(paths.data_dir.clone());
    let sources = platform::wallpaper_source_dirs(&paths);

    let lock_screen = platform::wallpaper_target();
    let services = platform::platform_services(&paths);
    let ui_sink = UiSink::default();
    let app_state = app_state::AppState::open_lazy(
        ui_sink.clone(),
        paths,
        sources,
        lock_screen,
        Arc::clone(&services),
    )
    .context("failed to start app worker")?;
    window_manager::install(app_state.clone(), ui_sink);

    let runtime_handle = start_non_visual_services(
        single_instance,
        app_state.clone(),
        Arc::clone(&services),
        tray_folders,
        log_dir,
    );
    window_manager::set_tray_available(runtime_handle.has_tray());

    if !options.background {
        window_manager::present_window();
    }

    if options.background {
        let startup_state = app_state.clone();
        slint::Timer::single_shot(Duration::from_millis(0), move || {
            startup_state.refresh_async();
        });
    }

    let result = slint::run_event_loop().context("failed to run UI event loop");
    drop(runtime_handle);
    result
}

fn start_non_visual_services(
    single_instance: platform::SingleInstanceGuard,
    app_state: app_state::AppState,
    services: Arc<dyn PlatformServices>,
    tray_folders: TrayFolders,
    log_dir: PathBuf,
) -> RuntimeHandle {
    let tray = start_tray_runtime(services, tray_folders);
    let runtime_handle: RuntimeHandle = Rc::new(RuntimeHandles {
        single_instance,
        _tray: tray,
    });

    slint::Timer::single_shot(LOGGING_STARTUP_DELAY, move || {
        if let Err(error) = spawn_background_startup_task("spotlit-logging-startup", move || {
            if let Err(error) = init_tracing(&log_dir).context("failed to initialize tracing") {
                eprintln!("spotlit: {error:#}");
            } else {
                tracing::info!("spotlit runtime services are starting");
            }
        }) {
            eprintln!("spotlit: {error:#}");
        }
    });

    let scheduler_state = app_state.clone();
    slint::Timer::single_shot(SCHEDULER_STARTUP_DELAY, move || {
        if let Err(error) = scheduler_state.start_scheduler() {
            tracing::warn!(%error, "failed to start scheduler");
        }
    });

    let activation_runtime = Rc::clone(&runtime_handle);
    slint::Timer::single_shot(ACTIVATION_LISTENER_STARTUP_DELAY, move || {
        if let Err(error) = activation_runtime
            .single_instance
            .start_activation_listener(|| {
                tracing::info!("activation request received");
                if let Err(error) = slint::invoke_from_event_loop(move || {
                    tracing::info!("presenting window from activation request");
                    window_manager::present_window();
                }) {
                    tracing::warn!(%error, "failed to queue activation request on UI event loop");
                }
            })
        {
            tracing::warn!(%error, "failed to start activation listener");
        }
    });

    runtime_handle
}

fn start_tray_runtime(
    services: Arc<dyn PlatformServices>,
    tray_folders: TrayFolders,
) -> Option<InstalledTray> {
    match install_tray(services, tray_folders) {
        Ok(tray) => Some(tray),
        Err(error) => {
            tracing::warn!(%error, "failed to create tray icon");
            None
        }
    }
}

fn spawn_background_startup_task(
    name: &'static str,
    task: impl FnOnce() + Send + 'static,
) -> anyhow::Result<()> {
    thread::Builder::new()
        .name(name.to_string())
        .stack_size(BACKGROUND_STARTUP_THREAD_STACK_SIZE)
        .spawn(task)
        .map(|_| ())
        .with_context(|| format!("failed to spawn {name}"))
}
