//! Cross-platform retry state and status classification for Windows PDH resources.

use std::time::{Duration, Instant};

pub(super) const PDH_INVALID_HANDLE_STATUS: u32 = 0xc000_0bbc;

const PDH_RETRY_INITIAL: Duration = Duration::from_secs(30);
const PDH_RETRY_MAX: Duration = Duration::from_secs(15 * 60);

pub(super) fn should_rebuild_pdh_query(status: u32) -> bool {
    status == PDH_INVALID_HANDLE_STATUS
}

pub(super) struct RetryingPdhResource<T> {
    value: Option<T>,
    last_error: Option<String>,
    next_retry_at: Option<Instant>,
    retry_delay: Duration,
    ever_succeeded: bool,
}

impl<T> RetryingPdhResource<T> {
    pub(super) fn new() -> Self {
        Self {
            value: None,
            last_error: None,
            next_retry_at: None,
            retry_delay: PDH_RETRY_INITIAL,
            ever_succeeded: false,
        }
    }

    pub(super) fn get_or_try_init<E>(
        &mut self,
        now: Instant,
        init: impl FnOnce() -> Result<T, E>,
    ) -> Result<&T, String>
    where
        E: ToString,
    {
        if self.value.is_none() {
            if self.next_retry_at.is_some_and(|retry_at| now < retry_at) {
                return Err(self
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "PDH initialization is waiting for retry".to_string()));
            }

            match init() {
                Ok(value) => {
                    self.value = Some(value);
                    self.last_error = None;
                    self.next_retry_at = None;
                    self.retry_delay = PDH_RETRY_INITIAL;
                    self.ever_succeeded = true;
                }
                Err(error) => {
                    let error = error.to_string();
                    self.record_failure(now, error.clone());
                    return Err(error);
                }
            }
        }

        self.value
            .as_ref()
            .ok_or_else(|| "PDH initialization completed without a resource".to_string())
    }

    pub(super) fn invalidate(&mut self, now: Instant, error: String) {
        // Dropping the resource closes the query before any later retry can replace it.
        drop(self.value.take());
        self.record_failure(now, error);
    }

    pub(super) fn ever_succeeded(&self) -> bool {
        self.ever_succeeded
    }

    fn record_failure(&mut self, now: Instant, error: String) {
        self.last_error = Some(error);
        self.next_retry_at = now.checked_add(self.retry_delay);
        self.retry_delay = self.retry_delay.saturating_mul(2).min(PDH_RETRY_MAX);
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use super::*;

    #[test]
    fn initialization_failure_retries_after_backoff_and_reuses_success() {
        let started_at = Instant::now();
        let mut attempts = 0;
        let mut state = RetryingPdhResource::new();

        state
            .get_or_try_init(started_at, || {
                attempts += 1;
                Err::<usize, _>("PDH unavailable")
            })
            .expect_err("the first initialization should fail");
        assert_eq!(attempts, 1);

        state
            .get_or_try_init(
                started_at + PDH_RETRY_INITIAL - Duration::from_millis(1),
                || {
                    attempts += 1;
                    Ok::<_, &str>(9)
                },
            )
            .expect_err("the resource must not initialize during backoff");
        assert_eq!(attempts, 1);

        let value = state
            .get_or_try_init(started_at + PDH_RETRY_INITIAL, || {
                attempts += 1;
                Ok::<_, &str>(9)
            })
            .expect("the resource should recover after backoff");
        assert_eq!(*value, 9);
        assert_eq!(attempts, 2);
        assert!(state.ever_succeeded());

        let value = state
            .get_or_try_init(
                started_at + PDH_RETRY_INITIAL * 2,
                || -> Result<usize, &str> { panic!("the successful resource must be reused") },
            )
            .expect("the successful resource should remain available");
        assert_eq!(*value, 9);
        assert_eq!(attempts, 2);
    }

    #[test]
    fn invalidation_drops_the_resource_and_rebuilds_after_backoff() {
        #[derive(Debug)]
        struct DropMarker(Rc<Cell<usize>>);

        impl Drop for DropMarker {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let started_at = Instant::now();
        let drops = Rc::new(Cell::new(0));
        let mut state = RetryingPdhResource::new();
        state
            .get_or_try_init(started_at, || Ok::<_, &str>(DropMarker(Rc::clone(&drops))))
            .expect("the initial resource should initialize");

        state.invalidate(started_at, "invalid query handle".to_string());
        assert_eq!(
            drops.get(),
            1,
            "the invalid resource must close immediately"
        );
        state
            .get_or_try_init(
                started_at + PDH_RETRY_INITIAL - Duration::from_millis(1),
                || Ok::<_, &str>(DropMarker(Rc::clone(&drops))),
            )
            .expect_err("an invalidated resource must observe backoff");

        state
            .get_or_try_init(started_at + PDH_RETRY_INITIAL, || {
                Ok::<_, &str>(DropMarker(Rc::clone(&drops)))
            })
            .expect("the invalidated resource should rebuild after backoff");
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn retry_delay_and_invalid_handle_classification_are_bounded_and_specific() {
        let mut now = Instant::now();
        let mut state = RetryingPdhResource::<()>::new();
        for _ in 0..16 {
            let attempted_delay = state.retry_delay;
            state
                .get_or_try_init(now, || Err::<(), _>("still unavailable"))
                .expect_err("the synthetic initialization should fail");
            assert!(state.retry_delay <= PDH_RETRY_MAX);
            now += attempted_delay;
        }
        assert_eq!(state.retry_delay, PDH_RETRY_MAX);

        assert!(should_rebuild_pdh_query(PDH_INVALID_HANDLE_STATUS));
        assert!(!should_rebuild_pdh_query(0));
        assert!(!should_rebuild_pdh_query(0xc000_0bc6)); // PDH_INVALID_DATA
    }
}
