//! Test doubles shared by the render modules.

use std::collections::VecDeque;
use std::process::ExitStatus;
use std::sync::Mutex;
use std::time::Duration;

use crate::BoxFuture;
use crate::command::{CommandError, CommandOutput, CommandRunner, CommandSpec};
use crate::types::ChartVersion;

/// A scriptable [`CommandRunner`] that records every invocation.
#[derive(Debug, Default)]
pub struct FakeCommandRunner {
    state: Mutex<FakeState>,
}

#[derive(Debug, Default)]
struct FakeState {
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
    /// Creates an empty runner. Invocations without a queued response succeed
    /// with empty output.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a successful stdout response for the next invocation whose
    /// arguments contain `needle`.
    pub fn push_output_for(&self, needle: &str, stdout: impl Into<String>) {
        self.push_matched(Some(needle.to_owned()), FakeResponse::Output(stdout.into()));
    }

    /// Queues a command error for the next invocation whose arguments contain
    /// `needle`.
    pub fn push_error_for(&self, needle: &str, error: CommandError) {
        self.push_matched(Some(needle.to_owned()), FakeResponse::Error(error));
    }

    fn push_matched(&self, matcher: Option<String>, response: FakeResponse) {
        self.lock()
            .responses
            .push_back(QueuedResponse { matcher, response });
    }

    /// Returns every recorded invocation in order.
    #[must_use]
    pub fn calls(&self) -> Vec<CommandSpec> {
        self.lock().calls.clone()
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

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().expect("fake command runner mutex")
    }
}

impl CommandRunner for FakeCommandRunner {
    fn run(&self, spec: CommandSpec) -> BoxFuture<'_, Result<CommandOutput, CommandError>> {
        Box::pin(async move {
            let response = {
                let mut state = self.state.lock().expect("fake command runner mutex");
                let position = state
                    .responses
                    .iter()
                    .position(|queued| match &queued.matcher {
                        Some(matcher) => {
                            spec.args.iter().any(|argument| argument.contains(matcher))
                        }
                        None => true,
                    });
                let response = position.and_then(|position| state.responses.remove(position));
                state.calls.push(spec);
                response.map(|queued| queued.response)
            };
            match response {
                Some(FakeResponse::Output(stdout)) => Ok(success(stdout)),
                Some(FakeResponse::Error(error)) => Err(error),
                None => Ok(success(String::new())),
            }
        })
    }
}

/// Returns a retryable command error for retry tests.
#[must_use]
pub fn transient_error() -> CommandError {
    CommandError::Timeout {
        command: "helm dependency build demo".to_owned(),
        timeout: Duration::from_secs(1),
    }
}

/// Returns a render request for a fixture app.
#[must_use]
pub fn request(
    app: &str,
    source: &std::path::Path,
    output: &std::path::Path,
) -> crate::render::RenderRequest {
    use crate::manifest::RendererKind;
    use crate::types::{AppName, NamespaceName};

    crate::render::RenderRequest {
        app: AppName::new(app).expect("app name"),
        source: source.to_path_buf(),
        renderer: RendererKind::Helm,
        common_values: None,
        values_files: Vec::new(),
        inline_values: serde_json::Value::Object(serde_json::Map::new()),
        output_dir: output.to_path_buf(),
        chart_version: None,
        release_name: app.to_owned(),
        namespace: NamespaceName::new("default").expect("namespace"),
        dry_run: true,
        helm_program: "helm".to_owned(),
        kustomize_program: "kustomize".to_owned(),
        kustomize_args: Vec::new(),
        repositories: Vec::new(),
        timeout: Duration::from_secs(30),
    }
}

/// Parses a chart version in tests without repeating the error handling.
#[must_use]
pub fn version(value: &str) -> ChartVersion {
    value.parse().expect("chart version")
}

fn success(stdout: String) -> CommandOutput {
    CommandOutput {
        status: success_status(),
        stdout,
        stderr: String::new(),
        command: "fake".to_owned(),
    }
}

#[cfg(unix)]
fn success_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

#[cfg(not(unix))]
fn success_status() -> ExitStatus {
    std::process::Command::new("true")
        .status()
        .expect("true command")
}
