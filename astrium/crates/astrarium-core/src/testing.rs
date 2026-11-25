//! Reusable test fakes for Astrarium.
//!
//! The module is compiled for this crate's unit tests and when the `testing`
//! feature is enabled. [`astrarium-test-support`](https://docs.rs/astrarium-test-support)
//! depends on that feature and re-exports the fakes, so external crates can use
//! them without a dependency cycle.
//!
//! The fakes implement the same traits as the production [`CommandRunner`] and
//! [`Clock`], which lets a test pass them to the real orchestrator code.

use std::collections::VecDeque;
use std::process::ExitStatus;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jiff::Timestamp;

use crate::BoxFuture;
use crate::clock::Clock;
use crate::command::{CommandError, CommandOutput, CommandRunner, CommandSpec};

/// A scripted, in-memory command runner that records every [`CommandSpec`].
///
/// Responses are matched by a substring of the command arguments and consumed
/// in the order they were queued. A command with no matching queued response
/// succeeds with empty output, which keeps fixtures that only care about the
/// recorded invocations short.
///
/// The runner is safe to share between tasks; every method takes `&self`.
#[derive(Debug, Default)]
pub struct FakeCommandRunner {
    state: Mutex<FakeCommandState>,
}

#[derive(Debug, Default)]
struct FakeCommandState {
    calls: Vec<CommandSpec>,
    responses: VecDeque<QueuedResponse>,
}

#[derive(Debug)]
struct QueuedResponse {
    matcher: Option<String>,
    response: FakeResponse,
}

#[derive(Debug)]
enum FakeResponse {
    Output(String),
    Error(CommandError),
}

impl FakeCommandRunner {
    /// Creates a runner with no queued responses.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a successful `stdout` response for the next invocation whose
    /// arguments contain `needle`.
    pub fn push_output_for(&self, needle: impl Into<String>, stdout: impl Into<String>) {
        self.push(Some(needle.into()), FakeResponse::Output(stdout.into()));
    }

    /// Queues a successful `stdout` response for the next invocation,
    /// regardless of its arguments.
    pub fn push_output(&self, stdout: impl Into<String>) {
        self.push(None, FakeResponse::Output(stdout.into()));
    }

    /// Queues an error for the next invocation whose arguments contain
    /// `needle`.
    pub fn push_error_for(&self, needle: impl Into<String>, error: CommandError) {
        self.push(Some(needle.into()), FakeResponse::Error(error));
    }

    /// Queues an error for the next invocation, regardless of its arguments.
    pub fn push_error(&self, error: CommandError) {
        self.push(None, FakeResponse::Error(error));
    }

    /// Returns every recorded invocation in order.
    #[must_use]
    pub fn calls(&self) -> Vec<CommandSpec> {
        self.lock().calls.clone()
    }

    /// Returns the number of recorded invocations.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.lock().calls.len()
    }

    /// Counts recorded invocations whose arguments contain `needle`.
    #[must_use]
    pub fn count_calls_containing(&self, needle: &str) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|call| call.args.iter().any(|argument| argument.contains(needle)))
            .count()
    }

    /// Clears recorded invocations and queued responses.
    pub fn clear(&self) {
        let mut state = self.lock();
        state.calls.clear();
        state.responses.clear();
    }

    fn push(&self, matcher: Option<String>, response: FakeResponse) {
        self.lock()
            .responses
            .push_back(QueuedResponse { matcher, response });
    }

    fn lock(&self) -> MutexGuard<'_, FakeCommandState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl CommandRunner for FakeCommandRunner {
    fn run(&self, spec: CommandSpec) -> BoxFuture<'_, Result<CommandOutput, CommandError>> {
        Box::pin(async move {
            let response = {
                let mut state = self.lock();
                let position = state.responses.iter().position(|queued| {
                    queued.matcher.as_ref().is_none_or(|matcher| {
                        spec.args.iter().any(|argument| argument.contains(matcher))
                    })
                });
                let response = position.and_then(|position| state.responses.remove(position));
                state.calls.push(spec);
                response.map(|queued| queued.response)
            };
            match response {
                Some(FakeResponse::Output(stdout)) => Ok(fake_success(stdout)),
                Some(FakeResponse::Error(error)) => Err(error),
                None => Ok(fake_success(String::new())),
            }
        })
    }
}

/// Returns a retryable command error suitable for retry tests.
#[must_use]
pub fn transient_error() -> CommandError {
    CommandError::Timeout {
        command: "fake command".to_owned(),
        timeout: Duration::from_secs(1),
    }
}

fn fake_success(stdout: String) -> CommandOutput {
    CommandOutput {
        status: ExitStatus::default(),
        stdout,
        stderr: String::new(),
        command: "fake".to_owned(),
    }
}

/// A fixed clock that records every sleep and returns immediately.
///
/// [`Clock::sleep`] advances the fake wall clock by the requested duration
/// before the returned future completes. Deadline loops that compare
/// [`Clock::now`] against a computed deadline therefore terminate without real
/// waiting, while still observing the same number and length of sleeps as the
/// production clock would.
#[derive(Debug)]
pub struct FakeClock {
    state: Mutex<FakeClockState>,
}

#[derive(Debug)]
struct FakeClockState {
    now: Timestamp,
    sleeps: Vec<Duration>,
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new(Timestamp::UNIX_EPOCH)
    }
}

impl FakeClock {
    /// Creates a clock fixed at `now`.
    #[must_use]
    pub fn new(now: Timestamp) -> Self {
        Self {
            state: Mutex::new(FakeClockState {
                now,
                sleeps: Vec::new(),
            }),
        }
    }

    /// Returns the current fake timestamp.
    #[must_use]
    pub fn current(&self) -> Timestamp {
        self.lock().now
    }

    /// Moves the clock forward by `duration` without recording a sleep.
    pub fn advance(&self, duration: Duration) {
        let mut state = self.lock();
        state.now = state.now.saturating_add(duration).unwrap_or(Timestamp::MAX);
    }

    /// Sets the current fake timestamp.
    pub fn set(&self, now: Timestamp) {
        self.lock().now = now;
    }

    /// Returns every recorded sleep duration in order.
    #[must_use]
    pub fn sleeps(&self) -> Vec<Duration> {
        self.lock().sleeps.clone()
    }

    /// Returns the number of recorded sleeps.
    #[must_use]
    pub fn sleep_count(&self) -> usize {
        self.lock().sleeps.len()
    }

    /// Returns the sum of all recorded sleep durations.
    #[must_use]
    pub fn total_slept(&self) -> Duration {
        self.lock()
            .sleeps
            .iter()
            .copied()
            .fold(Duration::ZERO, Duration::saturating_add)
    }

    fn lock(&self) -> MutexGuard<'_, FakeClockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        self.current()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        {
            let mut state = self.lock();
            state.sleeps.push(duration);
            state.now = state.now.saturating_add(duration).unwrap_or(Timestamp::MAX);
        }
        Box::pin(async {})
    }
}

#[cfg(test)]
mod tests {
    use crate::command::Tool;

    use super::*;

    #[test]
    fn queues_and_records_responses() {
        let runner = FakeCommandRunner::new();
        runner.push_output_for("template", "rendered");
        runner.push_error_for("package", transient_error());

        let template = CommandSpec::new(Tool::Helm).args(["template", "demo"]);
        let package = CommandSpec::new(Tool::Helm).args(["package", "demo"]);
        let unscripted = CommandSpec::new(Tool::Kind).arg("get");

        let template_result = futures::executor::block_on(runner.run(template));
        let package_result = futures::executor::block_on(runner.run(package));
        let unscripted_result = futures::executor::block_on(runner.run(unscripted));

        assert_eq!(template_result.expect("template").stdout, "rendered");
        assert!(matches!(package_result, Err(CommandError::Timeout { .. })));
        let unscripted = unscripted_result.expect("unscripted");
        assert_eq!(unscripted.stdout, "");
        assert_eq!(runner.call_count(), 3);
        assert_eq!(runner.count_calls_containing("template"), 1);
        assert_eq!(runner.count_calls_containing("demo"), 2);
    }

    #[test]
    fn clear_removes_responses_and_calls() {
        let runner = FakeCommandRunner::new();
        runner.push_output("first");
        let _ =
            futures::executor::block_on(runner.run(CommandSpec::new(Tool::Helm).arg("template")));
        runner.clear();

        assert_eq!(runner.call_count(), 0);
        let output =
            futures::executor::block_on(runner.run(CommandSpec::new(Tool::Helm).arg("template")))
                .expect("default response");
        assert_eq!(output.stdout, "");
    }

    #[test]
    fn sleeps_advance_time_and_are_recorded() {
        let clock = FakeClock::new(Timestamp::UNIX_EPOCH);
        futures::executor::block_on(clock.sleep(Duration::from_secs(3)));
        futures::executor::block_on(clock.sleep(Duration::from_secs(2)));

        assert_eq!(clock.sleep_count(), 2);
        assert_eq!(clock.total_slept(), Duration::from_secs(5));
        assert_eq!(
            clock.current(),
            Timestamp::UNIX_EPOCH
                .checked_add(Duration::from_secs(5))
                .expect("timestamp")
        );
    }

    #[test]
    fn advance_is_not_a_sleep() {
        let clock = FakeClock::default();
        clock.advance(Duration::from_secs(10));
        assert_eq!(clock.current().as_second(), 10);
        assert_eq!(clock.sleeps().len(), 0);
    }
}
