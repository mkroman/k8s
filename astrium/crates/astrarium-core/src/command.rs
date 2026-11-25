//! External command execution.
//!
//! Astrarium is deliberately restricted to `helm`, `kustomize` and `kind`.
//! Everything else — Git, Kubernetes API access and HTTP — goes through Rust
//! libraries. The [`Tool`] enum makes that restriction part of the API: there
//! is no way to construct a command for another executable.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use thiserror::Error;
use tokio::process::Command;
use tracing::{debug, warn};

use crate::BoxFuture;

/// The only external programs Astrarium may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    /// The Helm CLI.
    Helm,
    /// The Kustomize CLI.
    Kustomize,
    /// The kind CLI.
    Kind,
}

impl Tool {
    /// Returns the default executable name.
    #[must_use]
    pub const fn default_program(self) -> &'static str {
        match self {
            Self::Helm => "helm",
            Self::Kustomize => "kustomize",
            Self::Kind => "kind",
        }
    }

    /// Returns a stable lowercase label for telemetry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.default_program()
    }
}

/// A fully described external command.
#[derive(Clone, Debug, Default)]
pub struct CommandSpec {
    /// The tool being invoked.
    pub tool: Option<Tool>,
    /// The executable path or name. Defaults to the tool's program name.
    pub program: String,
    /// Arguments, passed without shell interpolation.
    pub args: Vec<String>,
    /// The working directory.
    pub cwd: Option<PathBuf>,
    /// Additional environment variables.
    pub env: BTreeMap<String, String>,
    /// An optional timeout.
    pub timeout: Option<Duration>,
    /// Optional standard input.
    pub stdin: Option<String>,
}

impl CommandSpec {
    /// Creates a command for a tool with the tool's default executable.
    #[must_use]
    pub fn new(tool: Tool) -> Self {
        Self {
            tool: Some(tool),
            program: tool.default_program().to_owned(),
            ..Self::default()
        }
    }

    /// Creates a command for a tool using an explicit executable path.
    #[must_use]
    pub fn with_program(tool: Tool, program: impl Into<String>) -> Self {
        Self {
            tool: Some(tool),
            program: program.into(),
            ..Self::default()
        }
    }

    /// Appends one argument.
    #[must_use]
    pub fn arg(mut self, argument: impl Into<String>) -> Self {
        self.args.push(argument.into());
        self
    }

    /// Appends several arguments.
    #[must_use]
    pub fn args<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(arguments.into_iter().map(Into::into));
        self
    }

    /// Sets the working directory.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Adds an environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Sets a timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets standard input.
    #[must_use]
    pub fn stdin(mut self, stdin: impl Into<String>) -> Self {
        self.stdin = Some(stdin.into());
        self
    }

    /// Returns a redacted, human-readable command line for logs.
    ///
    /// Helm and kind accept credentials or kubeconfig paths on the command
    /// line. Values that look like secrets are replaced.
    #[must_use]
    pub fn display_command(&self) -> String {
        let mut rendered = self.program.clone();
        let mut next_redaction: Option<&str> = None;
        for argument in &self.args {
            rendered.push(' ');
            if let Some(placeholder) = next_redaction.take() {
                rendered.push_str(placeholder);
                continue;
            }

            if argument.starts_with('-') {
                rendered.push_str(argument);
                let lower = argument.to_ascii_lowercase();
                if lower.contains("kubeconfig") {
                    next_redaction = Some("<kubeconfig>");
                } else if is_sensitive(&lower) {
                    next_redaction = Some("<redacted>");
                }
            } else {
                rendered.push_str(&redact_argument(argument));
            }
        }
        rendered
    }
}

/// The captured result of a successful process invocation.
#[derive(Clone, Debug)]
pub struct CommandOutput {
    /// The process exit status.
    pub status: ExitStatus,
    /// UTF-8 lossy standard output.
    pub stdout: String,
    /// UTF-8 lossy standard error.
    pub stderr: String,
    /// The command that produced the output, with arguments redacted.
    pub command: String,
}

impl CommandOutput {
    /// Returns true when the process exited successfully.
    #[must_use]
    pub fn success(&self) -> bool {
        self.status.success()
    }

    /// Returns a bounded tail of standard error, useful for diagnostics.
    #[must_use]
    pub fn stderr_tail(&self, max_lines: usize) -> String {
        tail_lines(&self.stderr, max_lines)
    }

    /// Returns a bounded tail of standard output, useful for diagnostics.
    #[must_use]
    pub fn stdout_tail(&self, max_lines: usize) -> String {
        tail_lines(&self.stdout, max_lines)
    }
}

/// An error while spawning, timing out or running an external command.
#[derive(Debug, Error)]
pub enum CommandError {
    /// The executable could not be spawned.
    #[error("could not start {command}: {source}")]
    Spawn {
        /// The redacted command line.
        command: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The command exceeded its timeout.
    #[error("command timed out after {timeout:?}: {command}")]
    Timeout {
        /// The redacted command line.
        command: String,
        /// The configured timeout.
        timeout: Duration,
    },
    /// The command exited with a non-zero status.
    #[error("command failed with {status}: {command}\n{stderr}")]
    Failed {
        /// The redacted command line.
        command: String,
        /// The exit status.
        status: ExitStatus,
        /// A bounded stderr tail.
        stderr: String,
    },
}

/// An abstraction over process execution, permitting deterministic tests.
pub trait CommandRunner: Send + Sync {
    /// Runs a command.
    fn run(&self, spec: CommandSpec) -> BoxFuture<'_, Result<CommandOutput, CommandError>>;
}

/// The production command runner.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, spec: CommandSpec) -> BoxFuture<'_, Result<CommandOutput, CommandError>> {
        Box::pin(run_real(spec))
    }
}

async fn run_real(spec: CommandSpec) -> Result<CommandOutput, CommandError> {
    let command_line = spec.display_command();
    debug!(command = %command_line, "running external command");

    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    command.kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    command.envs(&spec.env);
    if spec.stdin.is_some() {
        command.stdin(std::process::Stdio::piped());
    }
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut child = command.spawn().map_err(|source| CommandError::Spawn {
        command: command_line.clone(),
        source,
    })?;

    if let Some(stdin) = &spec.stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        use tokio::io::AsyncWriteExt;
        pipe.write_all(stdin.as_bytes())
            .await
            .map_err(|source| CommandError::Spawn {
                command: command_line.clone(),
                source,
            })?;
        pipe.shutdown().await.ok();
    }

    let wait = child.wait_with_output();
    let output = if let Some(timeout) = spec.timeout {
        match tokio::time::timeout(timeout, wait).await {
            Ok(result) => result.map_err(|source| CommandError::Spawn {
                command: command_line.clone(),
                source,
            })?,
            Err(_) => {
                return Err(CommandError::Timeout {
                    command: command_line,
                    timeout,
                });
            }
        }
    } else {
        wait.await.map_err(|source| CommandError::Spawn {
            command: command_line.clone(),
            source,
        })?
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !output.status.success() {
        warn!(command = %command_line, status = %output.status, "external command failed");
        return Err(CommandError::Failed {
            command: command_line,
            status: output.status,
            stderr: tail_lines(&stderr, 40),
        });
    }

    debug!(command = %command_line, status = %output.status, "external command succeeded");
    Ok(CommandOutput {
        status: output.status,
        stdout,
        stderr,
        command: command_line,
    })
}

fn is_sensitive(value: &str) -> bool {
    [
        "password",
        "token",
        "secret",
        "apikey",
        "api-key",
        "authorization",
    ]
    .iter()
    .any(|needle| value.contains(needle))
}

fn tail_lines(value: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = value.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

/// Redacts an argument that looks like a credential or a kubeconfig path.
#[must_use]
pub fn redact_argument(argument: &str) -> String {
    let lower = argument.to_ascii_lowercase();
    if is_sensitive(&lower) {
        return "<redacted>".to_owned();
    }
    if lower.contains("kubeconfig") {
        return "<kubeconfig>".to_owned();
    }
    argument.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_command_redacts_secrets() {
        let spec = CommandSpec::new(Tool::Helm)
            .args(["--kubeconfig", "/tmp/config", "--password", "hunter2"])
            .arg("--set")
            .arg("apiToken=abc");
        let display = spec.display_command();
        assert!(display.contains("<kubeconfig>"));
        assert!(!display.contains("hunter2"));
        assert!(!display.contains("abc"));
    }

    #[test]
    fn tail_lines_bounds_output() {
        let text = (1..=100)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let tail = tail_lines(&text, 3);
        assert_eq!(tail, "98\n99\n100");
    }
}
