use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// How long an operation must run before the global progress bar appears.
pub const SHOW_DELAY: Duration = Duration::from_millis(150);

/// Counts running DB operations of one window. The guard lives inside the
/// spawned task, so early returns and panics cannot leave the counter raised.
#[derive(Clone, Default)]
pub struct ActivityCounter(Arc<AtomicUsize>);

impl ActivityCounter {
    pub fn start(&self) -> OpGuard {
        self.0.fetch_add(1, Ordering::SeqCst);
        OpGuard(self.0.clone())
    }

    pub fn active(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// Decrements the counter when dropped.
pub struct OpGuard(Arc<AtomicUsize>);

impl Drop for OpGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Decides when the progress bar is visible: only after activity lasted
/// `SHOW_DELAY` without a gap, hidden immediately when activity stops.
#[derive(Default)]
pub struct DelayedVisibility {
    busy_since: Option<Instant>,
}

impl DelayedVisibility {
    pub fn update(&mut self, active: usize, now: Instant) -> bool {
        if active == 0 {
            self.busy_since = None;
            return false;
        }
        let since = *self.busy_since.get_or_insert(now);
        now.saturating_duration_since(since) >= SHOW_DELAY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_follows_nested_operations() {
        let counter = ActivityCounter::default();
        assert_eq!(counter.active(), 0);
        let first = counter.start();
        assert_eq!(counter.active(), 1);
        let second = counter.start();
        assert_eq!(counter.active(), 2);
        drop(first);
        assert_eq!(counter.active(), 1);
        drop(second);
        assert_eq!(counter.active(), 0);
    }

    #[test]
    fn clone_shares_the_same_counter() {
        let counter = ActivityCounter::default();
        let other = counter.clone();
        let _guard = other.start();
        assert_eq!(counter.active(), 1);
    }

    #[test]
    fn panic_releases_the_guard() {
        let counter = ActivityCounter::default();
        let task_counter = counter.clone();
        let result = std::panic::catch_unwind(move || {
            let _guard = task_counter.start();
            panic!("task failed");
        });
        assert!(result.is_err());
        assert_eq!(counter.active(), 0);
    }

    #[test]
    fn bar_stays_hidden_before_delay() {
        let mut visibility = DelayedVisibility::default();
        let start = Instant::now();
        assert!(!visibility.update(1, start));
        assert!(!visibility.update(1, start + Duration::from_millis(149)));
    }

    #[test]
    fn bar_shows_after_delay() {
        let mut visibility = DelayedVisibility::default();
        let start = Instant::now();
        assert!(!visibility.update(1, start));
        assert!(visibility.update(1, start + SHOW_DELAY));
    }

    #[test]
    fn bar_hides_immediately_when_idle_and_delay_restarts() {
        let mut visibility = DelayedVisibility::default();
        let start = Instant::now();
        visibility.update(1, start);
        assert!(visibility.update(1, start + Duration::from_millis(300)));
        assert!(!visibility.update(0, start + Duration::from_millis(310)));
        assert!(!visibility.update(1, start + Duration::from_millis(320)));
        assert!(!visibility.update(1, start + Duration::from_millis(400)));
    }
}
