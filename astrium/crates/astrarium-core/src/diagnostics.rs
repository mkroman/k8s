//! Failure diagnostics and redaction.
//!
//! Diagnostics are written as `diagnostics.json` and `summary.md`. Secret
//! values and credential-shaped environment variables are removed before
//! anything is written or appended to a CI step summary.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::AppName;

/// A diagnostics bundle for one run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiagnosticsReport {
    /// When the report was generated.
    pub generated_at: Timestamp,
    /// The run ID.
    pub run_id: String,
    /// A one-line result summary.
    pub summary: String,
    /// Per-app diagnostics.
    #[serde(default)]
    pub apps: Vec<AppDiagnostics>,
}

/// Diagnostics for one app.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppDiagnostics {
    /// The app name.
    pub app: Option<AppName>,
    /// The phase that failed or completed.
    pub phase: String,
    /// `success`, `failure`, `timeout`, `cancellation`, `error` or `skip`.
    pub result: String,
    /// A human-readable message.
    pub message: Option<String>,
    /// Argo CD health status.
    pub health: Option<String>,
    /// Argo CD sync status.
    pub sync: Option<String>,
    /// The revision Argo CD last synced.
    pub revision: Option<String>,
    /// The Argo CD operation phase.
    pub operation: Option<String>,
    /// Rendered resource identities that failed to apply.
    #[serde(default)]
    pub resources: Vec<ResourceDiagnostic>,
    /// Pods in the app namespace.
    #[serde(default)]
    pub pods: Vec<PodDiagnostic>,
    /// Kubernetes events, already redacted and sorted.
    #[serde(default)]
    pub events: Vec<EventDiagnostic>,
    /// Bounded container logs keyed by `namespace/pod/container`.
    #[serde(default)]
    pub logs: BTreeMap<String, String>,
}

/// A resource that could not be applied.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceDiagnostic {
    /// `apiVersion`.
    pub api_version: String,
    /// `kind`.
    pub kind: String,
    /// The namespace.
    pub namespace: Option<String>,
    /// The object name.
    pub name: String,
    /// The API error message.
    pub message: String,
}

/// A pod's state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PodDiagnostic {
    /// The namespace.
    pub namespace: String,
    /// The pod name.
    pub name: String,
    /// The pod phase.
    pub phase: Option<String>,
    /// The node name.
    pub node: Option<String>,
    /// Total restarts across containers.
    pub restarts: i32,
    /// Container states.
    #[serde(default)]
    pub containers: Vec<ContainerDiagnostic>,
}

/// A container's state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContainerDiagnostic {
    /// The container name.
    pub name: String,
    /// Whether the container is ready.
    pub ready: bool,
    /// The restart count.
    pub restart_count: i32,
    /// `running`, `waiting` or `terminated`.
    pub state: Option<String>,
    /// The state reason, for example `CrashLoopBackOff`.
    pub reason: Option<String>,
    /// The state message.
    pub message: Option<String>,
}

/// A Kubernetes event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventDiagnostic {
    /// The namespace.
    pub namespace: Option<String>,
    /// The event name.
    pub name: String,
    /// The reason.
    pub reason: Option<String>,
    /// The message, redacted.
    pub message: Option<String>,
    /// The involved object kind.
    pub kind: Option<String>,
    /// The involved object name.
    pub object: Option<String>,
    /// The event count.
    pub count: Option<i32>,
    /// The last observation time.
    pub last_timestamp: Option<Timestamp>,
}

/// An error writing diagnostics.
#[derive(Debug, Error)]
pub enum DiagnosticsError {
    /// The output directory could not be created.
    #[error("failed to create diagnostics directory {path}: {source}")]
    Directory {
        /// The directory path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A diagnostics file could not be written.
    #[error("failed to write {path}: {source}")]
    Write {
        /// The file path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The report could not be serialized.
    #[error("failed to serialize diagnostics: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl DiagnosticsReport {
    /// Writes `diagnostics.json` and `summary.md` into `directory`.
    ///
    /// # Errors
    ///
    /// Returns [`DiagnosticsError`] when the directory or a file cannot be
    /// written.
    pub fn write(&self, directory: &Path) -> Result<PathBuf, DiagnosticsError> {
        fs::create_dir_all(directory).map_err(|source| DiagnosticsError::Directory {
            path: directory.to_path_buf(),
            source,
        })?;

        let json_path = directory.join("diagnostics.json");
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&json_path, json).map_err(|source| DiagnosticsError::Write {
            path: json_path.clone(),
            source,
        })?;

        let summary_path = directory.join("summary.md");
        fs::write(&summary_path, self.summary_markdown()).map_err(|source| {
            DiagnosticsError::Write {
                path: summary_path,
                source,
            }
        })?;

        Ok(json_path)
    }

    /// Renders a Markdown summary suitable for `$GITHUB_STEP_SUMMARY`.
    #[must_use]
    pub fn summary_markdown(&self) -> String {
        let mut output = String::new();
        output.push_str("# Astrarium diagnostics\n\n");
        output.push_str(&format!("- Run: `{}`\n", self.run_id));
        output.push_str(&format!("- Generated: {}\n", self.generated_at));
        output.push_str(&format!("- Result: {}\n\n", redact(&self.summary)));

        for app in &self.apps {
            output.push_str(&format!(
                "## {} — {}\n\n",
                app.app.as_ref().map_or("<unknown>", AppName::as_str),
                redact(&app.result)
            ));
            if let Some(message) = &app.message {
                output.push_str(&format!("{}\n\n", redact(message)));
            }
            output.push_str("| field | value |\n|---|---|\n");
            output.push_str(&format!("| phase | {} |\n", redact(&app.phase)));
            output.push_str(&format!(
                "| health | {} |\n",
                redact(app.health.as_deref().unwrap_or("-"))
            ));
            output.push_str(&format!(
                "| sync | {} |\n",
                redact(app.sync.as_deref().unwrap_or("-"))
            ));
            output.push_str(&format!(
                "| operation | {} |\n",
                redact(app.operation.as_deref().unwrap_or("-"))
            ));
            output.push_str(&format!(
                "| revision | {} |\n\n",
                redact(app.revision.as_deref().unwrap_or("-"))
            ));

            if !app.resources.is_empty() {
                output.push_str("### Resources\n\n");
                for resource in &app.resources {
                    output.push_str(&format!(
                        "- `{} {} {}/{}`: {}\n",
                        resource.api_version,
                        resource.kind,
                        resource.namespace.as_deref().unwrap_or("-"),
                        resource.name,
                        redact(&resource.message)
                    ));
                }
                output.push('\n');
            }

            if !app.pods.is_empty() {
                output.push_str("### Pods\n\n");
                for pod in &app.pods {
                    output.push_str(&format!(
                        "- `{}/{}` phase={} node={} restarts={}\n",
                        pod.namespace,
                        pod.name,
                        pod.phase.as_deref().unwrap_or("-"),
                        pod.node.as_deref().unwrap_or("-"),
                        pod.restarts
                    ));
                    for container in &pod.containers {
                        output.push_str(&format!(
                            "  - `{}` ready={} restarts={} state={} reason={}\n",
                            container.name,
                            container.ready,
                            container.restart_count,
                            container.state.as_deref().unwrap_or("-"),
                            container.reason.as_deref().unwrap_or("-")
                        ));
                    }
                }
                output.push('\n');
            }

            if !app.events.is_empty() {
                output.push_str("### Events\n\n");
                for event in &app.events {
                    output.push_str(&format!(
                        "- `{}` {} {}: {}\n",
                        event.name,
                        event.kind.as_deref().unwrap_or("-"),
                        event.object.as_deref().unwrap_or("-"),
                        redact(event.message.as_deref().unwrap_or("-"))
                    ));
                }
                output.push('\n');
            }

            if !app.logs.is_empty() {
                output.push_str("### Logs\n\n");
                for (key, log) in &app.logs {
                    output.push_str(&format!("<details><summary>{key}</summary>\n\n```\n"));
                    output.push_str(&redact(log));
                    output.push_str("\n```\n\n</details>\n\n");
                }
            }
        }
        output
    }
}

/// Redacts credential-shaped key/value pairs and bearer tokens.
#[must_use]
pub fn redact(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for (index, line) in value.lines().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&redact_line(line));
    }
    if value.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn redact_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let sensitive = [
        "password",
        "passwd",
        "token",
        "secret",
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "private_key",
        "private-key",
    ];
    let is_sensitive = sensitive.iter().any(|needle| lower.contains(needle));
    if !is_sensitive {
        return line.to_owned();
    }

    if let Some(index) = line.find("Bearer ").or_else(|| line.find("bearer ")) {
        return format!("{}<redacted>", &line[..index + 7]);
    }

    if let Some(index) = line.find('=').or_else(|| line.find(':')) {
        let separator = line.as_bytes()[index] as char;
        return format!("{}{separator}<redacted>", &line[..index]);
    }

    "<redacted>".to_owned()
}

/// Recursively redacts sensitive keys in a JSON value.
pub fn redact_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let lower = key.to_ascii_lowercase();
                if lower.contains("password")
                    || lower.contains("token")
                    || lower.contains("secret")
                    || lower.contains("private_key")
                    || lower == "data"
                    || lower == "stringdata"
                {
                    *value = serde_json::Value::String("<redacted>".to_owned());
                } else {
                    redact_json(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_json(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_common_credentials() {
        let input = "PASSWORD=hunter2\napiToken=abc\nauthorization: Bearer abcdef\nplain=value";
        let output = redact(input);
        assert!(!output.contains("hunter2"));
        assert!(!output.contains("abc"));
        assert!(!output.contains("abcdef"));
        assert!(output.contains("plain=value"));
    }

    #[test]
    fn redacts_secret_json_data() {
        let mut value = serde_json::json!({
            "kind": "Secret",
            "data": {"password": "aHVudGVyMg=="},
            "metadata": {"name": "credentials"}
        });
        redact_json(&mut value);
        assert_eq!(value["data"], "<redacted>");
        assert_eq!(value["metadata"]["name"], "credentials");
    }

    #[test]
    fn markdown_summary_redacts_messages() {
        let report = DiagnosticsReport {
            generated_at: Timestamp::now(),
            run_id: "run".to_owned(),
            summary: "token=abc".to_owned(),
            apps: vec![AppDiagnostics {
                app: Some(AppName::new("alpha").unwrap()),
                result: "failure".to_owned(),
                message: Some("password=hunter2".to_owned()),
                ..AppDiagnostics::default()
            }],
        };
        let markdown = report.summary_markdown();
        assert!(!markdown.contains("abc"));
        assert!(!markdown.contains("hunter2"));
        assert!(markdown.contains("alpha"));
    }
}
