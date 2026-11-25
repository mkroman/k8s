//! Time and sleep abstractions.

use std::time::Duration;

use jiff::Timestamp;

use crate::BoxFuture;

/// A source of time that can be replaced in tests.
pub trait Clock: Send + Sync {
    /// Returns the current wall-clock time.
    fn now(&self) -> Timestamp;

    /// Sleeps for the given duration.
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()>;
}

/// The production clock backed by the operating system and Tokio.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            tokio::time::sleep(duration).await;
        })
    }
}
