//! Cancellation and publication have one linearization point.
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
#[derive(Default)]
pub struct Cancellation {
    state: AtomicU8,
    flag: AtomicBool,
}
impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn flag(&self) -> &AtomicBool {
        &self.flag
    }
    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == 1
    }
    /// False means publication already started; the UI must not promise cancellation.
    pub fn request(&self) -> bool {
        if self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.flag.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }
    pub fn publish<T>(&self, write: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.state
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Экспорт отменён или уже завершён".to_owned())?;
        write()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancel_and_publish_are_mutually_exclusive() {
        let cancel = Cancellation::new();
        assert!(cancel.request());
        assert!(cancel.publish(|| Ok(())).is_err());
        let cancel = Cancellation::new();
        cancel
            .publish(|| {
                assert!(!cancel.request());
                Ok(())
            })
            .unwrap();
        assert!(!cancel.request());
    }
}
