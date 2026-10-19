use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Idle,
    Pending {
        first_event: Instant,
        last_event: Instant,
    },
    Refreshing {
        dirty: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshStatus {
    Live,
    Pending,
    Refreshing,
}

#[derive(Debug)]
pub struct RefreshScheduler {
    quiet_period: Duration,
    maximum_delay: Duration,
    state: State,
}

impl RefreshScheduler {
    pub fn new(quiet_period: Duration, maximum_delay: Duration) -> Self {
        Self {
            quiet_period,
            maximum_delay,
            state: State::Idle,
        }
    }

    pub fn filesystem_hint(&mut self, now: Instant) {
        match &mut self.state {
            State::Idle => {
                self.state = State::Pending {
                    first_event: now,
                    last_event: now,
                };
            }
            State::Pending { last_event, .. } => *last_event = now,
            State::Refreshing { dirty } => *dirty = true,
        }
    }

    pub fn polling_hint(&mut self, now: Instant) {
        match &mut self.state {
            State::Idle => {
                self.state = State::Pending {
                    first_event: now,
                    last_event: now,
                };
            }
            State::Pending { .. } => {}
            State::Refreshing { dirty } => *dirty = true,
        }
    }

    pub fn manual_refresh(&mut self) -> bool {
        match &mut self.state {
            State::Refreshing { dirty } => {
                *dirty = true;
                false
            }
            State::Idle | State::Pending { .. } => {
                self.state = State::Refreshing { dirty: false };
                true
            }
        }
    }

    pub fn tick(&mut self, now: Instant) -> bool {
        let State::Pending {
            first_event,
            last_event,
        } = self.state
        else {
            return false;
        };
        if now.duration_since(last_event) >= self.quiet_period
            || now.duration_since(first_event) >= self.maximum_delay
        {
            self.state = State::Refreshing { dirty: false };
            return true;
        }
        false
    }

    pub fn refresh_finished(&mut self) -> bool {
        let State::Refreshing { dirty } = self.state else {
            return false;
        };
        if dirty {
            self.state = State::Refreshing { dirty: false };
            true
        } else {
            self.state = State::Idle;
            false
        }
    }

    pub fn status(&self) -> RefreshStatus {
        match self.state {
            State::Idle => RefreshStatus::Live,
            State::Pending { .. } => RefreshStatus::Pending,
            State::Refreshing { .. } => RefreshStatus::Refreshing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_scheduler() -> RefreshScheduler {
        RefreshScheduler::new(Duration::from_secs(2), Duration::from_secs(10))
    }

    #[test]
    fn isolated_event_refreshes_after_quiet_period() {
        let start = Instant::now();
        let mut scheduler = new_scheduler();
        scheduler.filesystem_hint(start);
        assert_eq!(scheduler.status(), RefreshStatus::Pending);
        assert!(!scheduler.tick(start + Duration::from_millis(1_999)));
        assert!(scheduler.tick(start + Duration::from_secs(2)));
        assert_eq!(scheduler.status(), RefreshStatus::Refreshing);
    }

    #[test]
    fn burst_uses_trailing_edge_but_continuous_events_hit_cap() {
        let start = Instant::now();
        let mut scheduler = new_scheduler();
        scheduler.filesystem_hint(start);
        scheduler.filesystem_hint(start + Duration::from_secs(1));
        assert!(!scheduler.tick(start + Duration::from_secs(2)));
        assert!(scheduler.tick(start + Duration::from_secs(3)));

        let mut scheduler = new_scheduler();
        scheduler.filesystem_hint(start);
        for second in 1..10 {
            scheduler.filesystem_hint(start + Duration::from_secs(second));
        }
        assert!(scheduler.tick(start + Duration::from_secs(10)));
    }

    #[test]
    fn polling_does_not_extend_existing_deadline() {
        let start = Instant::now();
        let mut scheduler = new_scheduler();
        scheduler.filesystem_hint(start);
        scheduler.polling_hint(start + Duration::from_millis(1_500));
        assert!(scheduler.tick(start + Duration::from_secs(2)));
    }

    #[test]
    fn manual_refresh_bypasses_pending_delay() {
        let start = Instant::now();
        let mut scheduler = new_scheduler();
        scheduler.filesystem_hint(start);
        assert!(scheduler.manual_refresh());
        assert_eq!(scheduler.status(), RefreshStatus::Refreshing);
    }

    #[test]
    fn events_during_refresh_coalesce_into_one_follow_up() {
        let start = Instant::now();
        let mut scheduler = new_scheduler();
        assert!(scheduler.manual_refresh());
        scheduler.filesystem_hint(start);
        scheduler.filesystem_hint(start + Duration::from_secs(1));
        scheduler.polling_hint(start + Duration::from_secs(2));
        assert!(scheduler.refresh_finished());
        assert_eq!(scheduler.status(), RefreshStatus::Refreshing);
        assert!(!scheduler.refresh_finished());
        assert_eq!(scheduler.status(), RefreshStatus::Live);
    }
}
