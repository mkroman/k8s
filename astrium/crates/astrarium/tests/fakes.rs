#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The reusable fakes implement the public Astrarium traits.

use std::sync::Arc;
use std::time::Duration;

use astrarium_core::clock::Clock;
use astrarium_core::command::{CommandRunner, CommandSpec, Tool};
use astrarium_test_support::{FakeClock, FakeCommandRunner};

#[tokio::test]
async fn fake_command_runner_implements_command_runner() {
    let runner = FakeCommandRunner::new();
    runner.push_output_for("version", "v1.2.3");
    let runner: Arc<dyn CommandRunner> = Arc::new(runner);

    let output = runner
        .run(CommandSpec::new(Tool::Helm).arg("version"))
        .await
        .expect("scripted output");

    assert_eq!(output.stdout, "v1.2.3");
    assert!(output.success());
}

#[tokio::test]
async fn fake_clock_implements_clock() {
    let clock = Arc::new(FakeClock::default());
    let clock_trait: Arc<dyn Clock> = clock.clone();

    clock_trait.sleep(Duration::from_secs(7)).await;

    assert_eq!(clock.sleep_count(), 1);
    assert_eq!(clock.total_slept(), Duration::from_secs(7));
    assert_eq!(clock_trait.now().as_second(), 7);
}
