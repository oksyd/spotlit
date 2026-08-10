use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone, Copy)]
pub struct SchedulerSettings {
    pub auto_sync_lock_screen: bool,
    pub sync_interval_minutes: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SchedulerDecision {
    SyncNow,
    Wait,
    Disabled,
}

impl SchedulerSettings {
    pub fn decide(&self, last_synced_at: Option<DateTime<Utc>>) -> SchedulerDecision {
        if !self.auto_sync_lock_screen {
            return SchedulerDecision::Disabled;
        }

        let Some(last_synced_at) = last_synced_at else {
            return SchedulerDecision::SyncNow;
        };

        let interval = Duration::minutes(self.sync_interval_minutes.max(1) as i64);
        if Utc::now() - last_synced_at >= interval {
            SchedulerDecision::SyncNow
        } else {
            SchedulerDecision::Wait
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::{SchedulerDecision, SchedulerSettings};

    #[test]
    fn scheduler_is_disabled_when_auto_sync_is_off() {
        let settings = SchedulerSettings {
            auto_sync_lock_screen: false,
            sync_interval_minutes: 30,
        };

        assert_eq!(settings.decide(None), SchedulerDecision::Disabled);
    }

    #[test]
    fn scheduler_syncs_when_no_previous_sync_exists() {
        let settings = SchedulerSettings {
            auto_sync_lock_screen: true,
            sync_interval_minutes: 30,
        };

        assert_eq!(settings.decide(None), SchedulerDecision::SyncNow);
    }

    #[test]
    fn scheduler_waits_until_interval_has_elapsed() {
        let settings = SchedulerSettings {
            auto_sync_lock_screen: true,
            sync_interval_minutes: 30,
        };

        assert_eq!(
            settings.decide(Some(Utc::now() - Duration::minutes(5))),
            SchedulerDecision::Wait
        );
        assert_eq!(
            settings.decide(Some(Utc::now() - Duration::minutes(31))),
            SchedulerDecision::SyncNow
        );
    }
}
