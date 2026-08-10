use std::{
    fmt, io,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    time::{Duration, Instant},
};

use crate::{
    command::Command,
    diagnostics::{self, Metric},
    worker::Worker,
    worker_event::{Snapshot, WorkerEvent},
};

const WORKER_THREAD_STACK_SIZE: usize = 512 * 1024;
const THUMBNAIL_WARM_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
pub struct WorkerStopped;

impl fmt::Display for WorkerStopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("worker thread stopped")
    }
}

#[derive(Clone)]
pub struct WorkerHandle {
    inner: Arc<Mutex<WorkerHandleState>>,
}

enum WorkerHandleState {
    Pending(WorkerStarter),
    Running(Sender<Command>),
    Stopped,
}

struct WorkerStarter {
    worker: Worker,
    events: WorkerEvents,
}

impl WorkerHandle {
    pub fn deferred(worker: Worker, events: impl Fn(WorkerEvent) + Send + 'static) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WorkerHandleState::Pending(WorkerStarter {
                worker,
                events: Box::new(events),
            }))),
        }
    }

    pub fn dispatch(&self, command: Command) -> Result<(), WorkerStopped> {
        let commands = self.commands()?;
        if commands.send(command).is_ok() {
            return Ok(());
        }

        self.mark_stopped();
        Err(WorkerStopped)
    }

    fn commands(&self) -> Result<Sender<Command>, WorkerStopped> {
        let mut state = self.inner.lock().map_err(|_| WorkerStopped)?;
        match &*state {
            WorkerHandleState::Running(commands) => return Ok(commands.clone()),
            WorkerHandleState::Stopped => return Err(WorkerStopped),
            WorkerHandleState::Pending(_) => {}
        }

        let WorkerHandleState::Pending(starter) =
            std::mem::replace(&mut *state, WorkerHandleState::Stopped)
        else {
            return Err(WorkerStopped);
        };

        match spawn_worker_thread(starter.worker, starter.events) {
            Ok(commands) => {
                *state = WorkerHandleState::Running(commands.clone());
                Ok(commands)
            }
            Err(error) => {
                tracing::warn!(%error, "failed to start worker thread");
                Err(WorkerStopped)
            }
        }
    }

    fn mark_stopped(&self) {
        if let Ok(mut state) = self.inner.lock() {
            *state = WorkerHandleState::Stopped;
        }
    }
}

type WorkerEvents = Box<dyn Fn(WorkerEvent) + Send + 'static>;

fn snapshot_from_event(event: &WorkerEvent) -> Option<&Snapshot> {
    match event {
        WorkerEvent::Snapshot(snapshot)
        | WorkerEvent::Synced(_, snapshot)
        | WorkerEvent::FavoriteUpdated(_, snapshot)
        | WorkerEvent::SettingsUpdated(_, snapshot) => Some(snapshot),
        WorkerEvent::AutoSyncIdle
        | WorkerEvent::ConfigUpdated(_, _)
        | WorkerEvent::OpenedPath(_)
        | WorkerEvent::Failed(_) => None,
    }
}

fn spawn_worker_thread(worker: Worker, events: WorkerEvents) -> io::Result<Sender<Command>> {
    let (commands, requests) = mpsc::channel::<Command>();
    std::thread::Builder::new()
        .name("spotlit-worker".to_string())
        .stack_size(WORKER_THREAD_STACK_SIZE)
        .spawn(move || {
            crate::platform::enter_background_thread_mode();
            let mut pending_thumbnail_warmup = None;
            loop {
                let had_pending_thumbnail_warmup = pending_thumbnail_warmup.is_some();
                let Some(command) = next_worker_command(&requests, &mut pending_thumbnail_warmup)
                else {
                    break;
                };
                let command_preempted_thumbnail_warmup =
                    had_pending_thumbnail_warmup && !matches!(command, Command::WarmThumbnails);
                let command_name = command.name();
                let started_at = Instant::now();
                let event = worker.handle(command);
                let can_warm_preview_cache = snapshot_from_event(&event).is_some();
                events(event);
                let elapsed = started_at.elapsed();
                tracing::info!(
                    command = command_name,
                    elapsed_ms = elapsed.as_millis(),
                    "worker command completed"
                );
                if command_name == Command::Scan.name() {
                    diagnostics::record(Metric::FirstScan, elapsed);
                }
                match update_pending_thumbnail_warmup(
                    command_name,
                    can_warm_preview_cache,
                    command_preempted_thumbnail_warmup,
                    &mut pending_thumbnail_warmup,
                    Instant::now(),
                ) {
                    ThumbnailWarmupDecision::Scheduled => {
                        tracing::debug!(
                            delay_ms = THUMBNAIL_WARM_DELAY.as_millis(),
                            "thumbnail warm-up scheduled"
                        );
                    }
                    ThumbnailWarmupDecision::Postponed => {
                        tracing::debug!(
                            delay_ms = THUMBNAIL_WARM_DELAY.as_millis(),
                            "thumbnail warm-up postponed after user command"
                        );
                    }
                    ThumbnailWarmupDecision::Unchanged => {}
                }
            }
        })
        .map(|_| commands)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ThumbnailWarmupDecision {
    Unchanged,
    Scheduled,
    Postponed,
}

fn update_pending_thumbnail_warmup(
    command_name: &'static str,
    can_warm_preview_cache: bool,
    command_preempted_thumbnail_warmup: bool,
    pending_thumbnail_warmup: &mut Option<Instant>,
    now: Instant,
) -> ThumbnailWarmupDecision {
    if can_warm_preview_cache
        && (command_name == Command::ImportImage.name() || command_name == Command::Scan.name())
    {
        *pending_thumbnail_warmup = Some(now + THUMBNAIL_WARM_DELAY);
        return ThumbnailWarmupDecision::Scheduled;
    }

    if command_preempted_thumbnail_warmup {
        *pending_thumbnail_warmup = Some(now + THUMBNAIL_WARM_DELAY);
        return ThumbnailWarmupDecision::Postponed;
    }

    ThumbnailWarmupDecision::Unchanged
}

fn next_worker_command(
    requests: &Receiver<Command>,
    pending_thumbnail_warmup: &mut Option<Instant>,
) -> Option<Command> {
    let Some(warmup_at) = *pending_thumbnail_warmup else {
        return requests.recv().ok();
    };

    let now = Instant::now();
    if now >= warmup_at {
        match requests.try_recv() {
            Ok(command) => return Some(command),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }

        *pending_thumbnail_warmup = None;
        return Some(Command::WarmThumbnails);
    }

    match requests.recv_timeout(warmup_at.saturating_duration_since(now)) {
        Ok(command) => Some(command),
        Err(RecvTimeoutError::Timeout) => {
            *pending_thumbnail_warmup = None;
            Some(Command::WarmThumbnails)
        }
        Err(RecvTimeoutError::Disconnected) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    use crate::command::Command;

    use super::{
        THUMBNAIL_WARM_DELAY, ThumbnailWarmupDecision, next_worker_command,
        update_pending_thumbnail_warmup,
    };

    #[test]
    fn worker_command_preempts_pending_thumbnail_warmup() {
        let (commands, requests) = mpsc::channel();
        commands.send(Command::Scan).expect("command is queued");
        let mut pending_thumbnail_warmup = Some(Instant::now() + Duration::from_secs(30));

        let command = next_worker_command(&requests, &mut pending_thumbnail_warmup);

        assert!(matches!(command, Some(Command::Scan)));
        assert!(pending_thumbnail_warmup.is_some());
    }

    #[test]
    fn due_thumbnail_warmup_becomes_worker_command() {
        let (_commands, requests) = mpsc::channel();
        let mut pending_thumbnail_warmup = Some(Instant::now());

        let command = next_worker_command(&requests, &mut pending_thumbnail_warmup);

        assert!(matches!(command, Some(Command::WarmThumbnails)));
        assert!(pending_thumbnail_warmup.is_none());
    }

    #[test]
    fn queued_user_command_preempts_due_thumbnail_warmup() {
        let (commands, requests) = mpsc::channel();
        commands
            .send(Command::SyncCurrent)
            .expect("user command is queued");
        let mut pending_thumbnail_warmup = Some(Instant::now());

        let command = next_worker_command(&requests, &mut pending_thumbnail_warmup);

        assert!(matches!(command, Some(Command::SyncCurrent)));
        assert!(pending_thumbnail_warmup.is_some());
    }

    #[test]
    fn scan_snapshot_schedules_thumbnail_warmup() {
        let now = Instant::now();
        let mut pending_thumbnail_warmup = None;

        let decision = update_pending_thumbnail_warmup(
            Command::Scan.name(),
            true,
            false,
            &mut pending_thumbnail_warmup,
            now,
        );

        assert_eq!(decision, ThumbnailWarmupDecision::Scheduled);
        assert_eq!(pending_thumbnail_warmup, Some(now + THUMBNAIL_WARM_DELAY));
    }

    #[test]
    fn failed_scan_does_not_schedule_thumbnail_warmup() {
        let mut pending_thumbnail_warmup = None;

        let decision = update_pending_thumbnail_warmup(
            Command::Scan.name(),
            false,
            false,
            &mut pending_thumbnail_warmup,
            Instant::now(),
        );

        assert_eq!(decision, ThumbnailWarmupDecision::Unchanged);
        assert!(pending_thumbnail_warmup.is_none());
    }

    #[test]
    fn user_command_postpones_pending_thumbnail_warmup() {
        let now = Instant::now();
        let mut pending_thumbnail_warmup = Some(now);

        let decision = update_pending_thumbnail_warmup(
            Command::SetFavorite {
                id: "id".to_string(),
                favorite: true,
            }
            .name(),
            false,
            true,
            &mut pending_thumbnail_warmup,
            now,
        );

        assert_eq!(decision, ThumbnailWarmupDecision::Postponed);
        assert_eq!(pending_thumbnail_warmup, Some(now + THUMBNAIL_WARM_DELAY));
    }

    #[test]
    fn thumbnail_warmup_command_does_not_reschedule_itself() {
        let mut pending_thumbnail_warmup = None;

        let decision = update_pending_thumbnail_warmup(
            Command::WarmThumbnails.name(),
            false,
            false,
            &mut pending_thumbnail_warmup,
            Instant::now(),
        );

        assert_eq!(decision, ThumbnailWarmupDecision::Unchanged);
        assert!(pending_thumbnail_warmup.is_none());
    }
}
