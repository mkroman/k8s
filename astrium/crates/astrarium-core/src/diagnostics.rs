//! Failure diagnostics and redaction.
//!
//! Diagnostics are written as `diagnostics.json` and `summary.md`. Secret
//! values and credential-shaped environment variables are removed before
//! anything is written or appended to a CI step summary.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use k8s_openapi::api::core::v1::{Event, Pod};
use kube::core::DynamicObject;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::argocd::ApplicationStatus;
use crate::kube::{APPLICATION, EVENT, KubeApi, LogRequest, POD, ResourceKey};
use crate::telemetry::RunResult;
use crate::types::{AppName, NamespaceName};

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
    /// Argo CD controller and repo-server error lines.
    #[serde(default)]
    pub argocd: ArgoCdDiagnostics,
}

/// Argo CD control-plane error lines.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArgoCdDiagnostics {
    /// Error-shaped lines from the application controller.
    pub application_controller: Vec<String>,
    /// Error-shaped lines from the repo server.
    pub repo_server: Vec<String>,
}

/// Diagnostics for one app.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppDiagnostics {
    /// The app name.
    pub app: Option<AppName>,
    /// The app namespace.
    pub namespace: Option<String>,
    /// The phase that failed or completed.
    pub phase: String,
    /// `success`, `failure`, `timeout`, `cancellation`, `error` or `skip`.
    pub result: String,
    /// A human-readable message.
    pub message: Option<String>,
    /// Argo CD health status.
    pub health: Option<String>,
    /// A health message from Argo CD.
    pub health_message: Option<String>,
    /// Argo CD sync status.
    pub sync: Option<String>,
    /// The revision Argo CD last synced.
    pub revision: Option<String>,
    /// The Argo CD operation phase.
    pub operation: Option<String>,
    /// A message from the last Argo CD operation.
    pub operation_message: Option<String>,
    /// Rendered resource identities that failed to apply.
    #[serde(default)]
    pub resources: Vec<ResourceDiagnostic>,
    /// Pods in the app namespace.
    #[serde(default)]
    pub pods: Vec<PodDiagnostic>,
    /// Kubernetes events, already redacted and sorted.
    #[serde(default)]
    pub events: Vec<EventDiagnostic>,
    /// Bounded current container logs keyed by `namespace/pod/container`.
    #[serde(default)]
    pub logs: BTreeMap<String, String>,
    /// Bounded logs from the previous container instance.
    #[serde(default)]
    pub previous_logs: BTreeMap<String, String>,
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
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
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
    /// The current terminated state's exit code.
    pub exit_code: Option<i32>,
    /// The previous terminated state's exit code.
    pub last_exit_code: Option<i32>,
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

/// What the collector should gather for one app.
#[derive(Clone, Debug)]
pub struct AppRequest {
    /// The app name.
    pub app: AppName,
    /// The app namespace.
    pub namespace: NamespaceName,
    /// The phase that failed or completed.
    pub phase: String,
    /// The task result.
    pub result: RunResult,
    /// A human-readable message from the failure.
    pub message: Option<String>,
    /// Resources that failed to apply.
    pub resources: Vec<ResourceDiagnostic>,
}

impl AppRequest {
    /// Creates a request for an app.
    #[must_use]
    pub fn new(
        app: AppName,
        namespace: NamespaceName,
        phase: impl Into<String>,
        result: RunResult,
    ) -> Self {
        Self {
            app,
            namespace,
            phase: phase.into(),
            result,
            message: None,
            resources: Vec::new(),
        }
    }

    /// Adds a human-readable failure message.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Adds resources that failed to apply.
    #[must_use]
    pub fn with_resources(mut self, resources: Vec<ResourceDiagnostic>) -> Self {
        self.resources = resources;
        self
    }
}

/// A diagnostics collection request.
#[derive(Clone, Debug)]
pub struct DiagnosticsRequest {
    /// The run ID.
    pub run_id: String,
    /// A one-line result summary.
    pub summary: String,
    /// The apps to collect.
    pub apps: Vec<AppRequest>,
}

impl DiagnosticsRequest {
    /// Creates a request for a run.
    #[must_use]
    pub fn new(run_id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            summary: summary.into(),
            apps: Vec::new(),
        }
    }

    /// Adds an app to collect.
    #[must_use]
    pub fn with_app(mut self, app: AppRequest) -> Self {
        self.apps.push(app);
        self
    }
}

/// Collects best-effort failure diagnostics through [`KubeApi`].
///
/// A Kubernetes API failure never fails collection: diagnostics exist to
/// explain failures, so a missing object or an unreachable log endpoint is
/// logged and skipped.
pub struct DiagnosticsCollector<'a> {
    kube: &'a dyn KubeApi,
    argocd_namespace: String,
    log_tail_lines: u32,
    argo_log_tail_lines: u32,
    argo_error_limit: usize,
}

impl<'a> DiagnosticsCollector<'a> {
    /// Creates a collector for an Argo CD namespace.
    #[must_use]
    pub fn new(kube: &'a dyn KubeApi, argocd_namespace: impl Into<String>) -> Self {
        Self {
            kube,
            argocd_namespace: argocd_namespace.into(),
            log_tail_lines: 100,
            argo_log_tail_lines: 500,
            argo_error_limit: 30,
        }
    }

    /// Sets how many lines of each pod log are kept.
    #[must_use]
    pub const fn with_log_tail_lines(mut self, lines: u32) -> Self {
        self.log_tail_lines = lines;
        self
    }

    /// Collects a report.
    ///
    /// Apps are processed concurrently and sorted by name. Pods, containers
    /// and events are sorted so repeated runs produce stable reports.
    pub async fn collect(&self, request: DiagnosticsRequest) -> DiagnosticsReport {
        let mut requests = request.apps;
        requests.sort_by(|left, right| left.app.as_str().cmp(right.app.as_str()));

        let mut collected = Vec::with_capacity(requests.len());
        let futures = requests.iter().map(|app| self.collect_app(app));
        for app in futures::future::join_all(futures).await {
            collected.push(app);
        }
        let argocd = self.collect_argocd_errors().await;

        DiagnosticsReport {
            generated_at: Timestamp::now(),
            run_id: request.run_id,
            summary: redact(&request.summary),
            apps: collected,
            argocd,
        }
    }

    async fn collect_app(&self, request: &AppRequest) -> AppDiagnostics {
        let status = self.application_status(&request.app).await;

        let pods = match self
            .kube
            .list(POD, Some(request.namespace.to_string()), None)
            .await
        {
            Ok(objects) => {
                let mut pods: Vec<PodDiagnostic> = objects.iter().filter_map(parse_pod).collect();
                pods.sort_by(|left, right| left.name.cmp(&right.name));
                for pod in &mut pods {
                    pod.containers
                        .sort_by(|left, right| left.name.cmp(&right.name));
                }
                pods
            }
            Err(error) => {
                warn!(app = %request.app, %error, "failed to list pods for diagnostics");
                Vec::new()
            }
        };

        let (logs, previous_logs) = self.collect_pod_logs(&request.namespace, &pods).await;

        let events = match self
            .kube
            .list(EVENT, Some(request.namespace.to_string()), None)
            .await
        {
            Ok(objects) => {
                let mut events: Vec<EventDiagnostic> =
                    objects.iter().filter_map(parse_event).collect();
                events.sort_by(compare_events);
                events
            }
            Err(error) => {
                warn!(app = %request.app, %error, "failed to list events for diagnostics");
                Vec::new()
            }
        };

        let resources = request
            .resources
            .iter()
            .map(|resource| ResourceDiagnostic {
                message: redact(&resource.message),
                ..resource.clone()
            })
            .collect();

        AppDiagnostics {
            app: Some(request.app.clone()),
            namespace: Some(request.namespace.to_string()),
            phase: request.phase.clone(),
            result: request.result.as_str().to_owned(),
            message: request.message.as_deref().map(redact),
            health: status
                .as_ref()
                .and_then(|status| status.health.as_ref())
                .and_then(|health| health.status.clone()),
            health_message: status
                .as_ref()
                .and_then(|status| status.health.as_ref())
                .and_then(|health| health.message.as_deref())
                .map(redact),
            sync: status
                .as_ref()
                .and_then(|status| status.sync.as_ref())
                .and_then(|sync| sync.status.clone()),
            revision: status
                .as_ref()
                .and_then(|status| status.sync.as_ref())
                .and_then(|sync| sync.revision.clone()),
            operation: status
                .as_ref()
                .and_then(|status| status.operation_state.as_ref())
                .map(|operation| operation.phase.clone()),
            operation_message: status
                .as_ref()
                .and_then(|status| status.operation_state.as_ref())
                .and_then(|operation| operation.message.as_deref())
                .map(redact),
            resources,
            pods,
            events,
            logs,
            previous_logs,
        }
    }

    async fn application_status(&self, app: &AppName) -> Option<ApplicationStatus> {
        let key = ResourceKey::namespaced(APPLICATION, self.argocd_namespace.clone(), app.as_str());
        match self.kube.get(key).await {
            Ok(Some(object)) => {
                serde_json::from_value::<ApplicationStatus>(object.data.get("status")?.clone()).ok()
            }
            Ok(None) => None,
            Err(error) => {
                warn!(app = %app, %error, "failed to read the Application for diagnostics");
                None
            }
        }
    }

    async fn collect_pod_logs(
        &self,
        namespace: &NamespaceName,
        pods: &[PodDiagnostic],
    ) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
        let mut logs = BTreeMap::new();
        let mut previous_logs = BTreeMap::new();

        for pod in pods {
            for container in &pod.containers {
                let key = format!("{namespace}/{}/{}", pod.name, container.name);
                if let Some(log) = self
                    .pod_logs(namespace, &pod.name, &container.name, false)
                    .await
                {
                    logs.insert(key.clone(), log);
                }
                if container.restart_count > 0
                    && let Some(log) = self
                        .pod_logs(namespace, &pod.name, &container.name, true)
                        .await
                {
                    previous_logs.insert(key, log);
                }
            }
        }

        (logs, previous_logs)
    }

    async fn pod_logs(
        &self,
        namespace: &NamespaceName,
        pod: &str,
        container: &str,
        previous: bool,
    ) -> Option<String> {
        let request = LogRequest {
            namespace: namespace.to_string(),
            pod: pod.to_owned(),
            container: Some(container.to_owned()),
            previous,
            tail_lines: self.log_tail_lines,
        };
        match self.kube.logs(request).await {
            Ok(log) => Some(redact(&log)),
            Err(error) => {
                warn!(
                    %namespace,
                    pod,
                    container,
                    previous,
                    %error,
                    "failed to read container logs for diagnostics"
                );
                None
            }
        }
    }

    async fn collect_argocd_errors(&self) -> ArgoCdDiagnostics {
        let mut diagnostics = ArgoCdDiagnostics::default();
        let objects = match self
            .kube
            .list(POD, Some(self.argocd_namespace.clone()), None)
            .await
        {
            Ok(objects) => objects,
            Err(error) => {
                warn!(%error, "failed to list Argo CD pods for diagnostics");
                return diagnostics;
            }
        };

        let mut pods: Vec<Pod> = objects
            .iter()
            .filter_map(|object| {
                serde_json::from_value::<Pod>(serde_json::to_value(object).ok()?).ok()
            })
            .collect();
        pods.sort_by(|left, right| left.metadata.name.cmp(&right.metadata.name));

        for pod in pods {
            let Some(name) = pod.metadata.name.clone() else {
                continue;
            };
            let target = if name.starts_with("argocd-application-controller") {
                &mut diagnostics.application_controller
            } else if name.starts_with("argocd-repo-server") {
                &mut diagnostics.repo_server
            } else {
                continue;
            };

            let containers: Vec<String> = pod.spec.as_ref().map_or_else(Vec::new, |spec| {
                spec.containers
                    .iter()
                    .map(|container| container.name.clone())
                    .collect()
            });
            for container in containers {
                let request = LogRequest {
                    namespace: self.argocd_namespace.clone(),
                    pod: name.clone(),
                    container: Some(container.clone()),
                    previous: false,
                    tail_lines: self.argo_log_tail_lines,
                };
                match self.kube.logs(request).await {
                    Ok(log) => {
                        for line in log.lines().filter(|line| is_error_line(line)) {
                            target.push(redact(&format!("{name}/{container}: {line}")));
                        }
                    }
                    Err(error) => {
                        warn!(
                            pod = %name,
                            %container,
                            %error,
                            "failed to read Argo CD logs for diagnostics"
                        );
                    }
                }
            }
            let excess = target.len().saturating_sub(self.argo_error_limit);
            if excess > 0 {
                target.drain(0..excess);
            }
        }

        diagnostics
    }
}

fn parse_pod(object: &DynamicObject) -> Option<PodDiagnostic> {
    let pod: Pod = serde_json::from_value(serde_json::to_value(object).ok()?).ok()?;
    let name = pod.metadata.name.clone()?;
    let namespace = pod.metadata.namespace.clone().unwrap_or_default();
    let phase = pod.status.as_ref().and_then(|status| status.phase.clone());
    let node = pod.spec.as_ref().and_then(|spec| spec.node_name.clone());

    let mut restarts = 0_i32;
    let mut containers = Vec::new();
    if let Some(statuses) = pod
        .status
        .as_ref()
        .and_then(|status| status.container_statuses.as_ref())
    {
        for status in statuses {
            let (state, reason, message, exit_code) = describe_state(status.state.as_ref());
            let (_, _, _, last_exit_code) = describe_state(status.last_state.as_ref());
            restarts = restarts.saturating_add(status.restart_count);
            containers.push(ContainerDiagnostic {
                name: status.name.clone(),
                ready: status.ready,
                restart_count: status.restart_count,
                state,
                reason,
                message: message.as_deref().map(redact),
                exit_code,
                last_exit_code,
            });
        }
    }

    Some(PodDiagnostic {
        namespace,
        name,
        phase,
        node,
        restarts,
        containers,
    })
}

fn describe_state(
    state: Option<&k8s_openapi::api::core::v1::ContainerState>,
) -> (Option<String>, Option<String>, Option<String>, Option<i32>) {
    match state {
        Some(state) if state.running.is_some() => (Some("running".to_owned()), None, None, None),
        Some(state) if state.waiting.is_some() => {
            let waiting = state.waiting.as_ref();
            (
                Some("waiting".to_owned()),
                waiting.and_then(|waiting| waiting.reason.clone()),
                waiting.and_then(|waiting| waiting.message.clone()),
                None,
            )
        }
        Some(state) if state.terminated.is_some() => {
            let terminated = state.terminated.as_ref();
            (
                Some("terminated".to_owned()),
                terminated.and_then(|terminated| terminated.reason.clone()),
                terminated.and_then(|terminated| terminated.message.clone()),
                terminated.map(|terminated| terminated.exit_code),
            )
        }
        _ => (None, None, None, None),
    }
}

fn parse_event(object: &DynamicObject) -> Option<EventDiagnostic> {
    let event: Event = serde_json::from_value(serde_json::to_value(object).ok()?).ok()?;
    let timestamp = event
        .last_timestamp
        .map(|time| time.0)
        .or_else(|| event.event_time.map(|time| time.0));
    Some(EventDiagnostic {
        namespace: event.metadata.namespace.clone(),
        name: event.metadata.name.clone()?,
        reason: event.reason.clone(),
        message: event.message.as_deref().map(redact),
        kind: event.involved_object.kind.clone(),
        object: event.involved_object.name.clone(),
        count: event.count,
        last_timestamp: timestamp.and_then(|time| time.to_string().parse::<Timestamp>().ok()),
    })
}

fn compare_events(left: &EventDiagnostic, right: &EventDiagnostic) -> std::cmp::Ordering {
    left.last_timestamp
        .cmp(&right.last_timestamp)
        .then_with(|| left.namespace.cmp(&right.namespace))
        .then_with(|| left.name.cmp(&right.name))
}

fn is_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "\"level\":\"error\"",
        "\"level\":\"warn\"",
        "error",
        "invalid",
        "failed",
        "forbidden",
        "unable",
        "timeout",
        "timed out",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

impl DiagnosticsReport {
    /// Writes `diagnostics.json` and `summary.md` into `directory`.
    ///
    /// The JSON report is redacted as well as the Markdown summary, so an
    /// artifact upload cannot leak Secrets. The summary is also appended to
    /// `$GITHUB_STEP_SUMMARY` and failed apps are annotated when running in
    /// GitHub Actions; those CI side effects are best effort.
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
        let json = serde_json::to_string_pretty(&self.redacted())?;
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

        if let Err(error) = self.emit_ci() {
            warn!(%error, "failed to publish diagnostics to the CI summary");
        }

        Ok(json_path)
    }

    /// Appends the Markdown summary to `$GITHUB_STEP_SUMMARY` when set.
    ///
    /// Returns `Ok(false)` outside GitHub Actions. A write failure is returned
    /// so the caller can warn without changing a run's result.
    ///
    /// # Errors
    ///
    /// Returns [`DiagnosticsError::Write`] when the summary file cannot be
    /// appended to.
    pub fn append_github_step_summary(&self) -> Result<bool, DiagnosticsError> {
        let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") else {
            return Ok(false);
        };
        if path.is_empty() {
            return Ok(false);
        }
        append_to(Path::new(&path), &self.summary_markdown())?;
        Ok(true)
    }

    /// Returns `::error` annotations for failed apps.
    ///
    /// The strings are escaped for GitHub Actions commands and redacted.
    #[must_use]
    pub fn github_annotations(&self) -> Vec<String> {
        self.apps
            .iter()
            .filter(|app| !matches!(app.result.as_str(), "success" | "skip"))
            .map(|app| {
                let name = app.app.as_ref().map_or("<unknown>", AppName::as_str);
                let message = app
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("{} failed in phase {}", name, app.phase));
                format!(
                    "::error title={}::{}",
                    escape_github_command(&format!("Astrarium app {name}")),
                    escape_github_command(&redact(&message))
                )
            })
            .collect()
    }

    /// Appends the CI summary and emits annotations inside GitHub Actions.
    ///
    /// # Errors
    ///
    /// Returns [`DiagnosticsError::Write`] when the step summary cannot be
    /// appended to.
    pub fn emit_ci(&self) -> Result<(), DiagnosticsError> {
        self.append_github_step_summary()?;
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            for annotation in self.github_annotations() {
                eprintln!("{annotation}");
            }
        }
        Ok(())
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
            output.push_str(&format!(
                "| namespace | {} |\n",
                redact(app.namespace.as_deref().unwrap_or("-"))
            ));
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

            if let Some(message) = &app.health_message {
                output.push_str(&format!("Health message: {}\n\n", redact(message)));
            }
            if let Some(message) = &app.operation_message {
                output.push_str(&format!("Operation message: {}\n\n", redact(message)));
            }

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
                            "  - `{}` ready={} restarts={} state={} reason={} exit={} previous_exit={}\n",
                            container.name,
                            container.ready,
                            container.restart_count,
                            container.state.as_deref().unwrap_or("-"),
                            container.reason.as_deref().unwrap_or("-"),
                            container.exit_code.map_or_else(|| "-".to_owned(), |code| code.to_string()),
                            container.last_exit_code.map_or_else(|| "-".to_owned(), |code| code.to_string()),
                        ));
                    }
                }
                output.push('\n');
            }

            if !app.events.is_empty() {
                output.push_str("### Events\n\n");
                for event in &app.events {
                    output.push_str(&format!(
                        "- `{}` {} {} count={}: {}\n",
                        event.name,
                        event.kind.as_deref().unwrap_or("-"),
                        event.object.as_deref().unwrap_or("-"),
                        event
                            .count
                            .map_or_else(|| "-".to_owned(), |count| count.to_string()),
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

            if !app.previous_logs.is_empty() {
                output.push_str("### Previous logs\n\n");
                for (key, log) in &app.previous_logs {
                    output.push_str(&format!(
                        "<details><summary>{key} (previous)</summary>\n\n```\n"
                    ));
                    output.push_str(&redact(log));
                    output.push_str("\n```\n\n</details>\n\n");
                }
            }
        }

        if !self.argocd.application_controller.is_empty() {
            output.push_str("### Argo CD application controller\n\n```\n");
            output.push_str(&redact(&self.argocd.application_controller.join("\n")));
            output.push_str("\n```\n\n");
        }
        if !self.argocd.repo_server.is_empty() {
            output.push_str("### Argo CD repo server\n\n```\n");
            output.push_str(&redact(&self.argocd.repo_server.join("\n")));
            output.push_str("\n```\n\n");
        }

        output
    }

    fn redacted(&self) -> Self {
        let mut report = self.clone();
        report.summary = redact(&report.summary);
        for app in &mut report.apps {
            app.message = app.message.as_deref().map(redact);
            app.health_message = app.health_message.as_deref().map(redact);
            app.operation_message = app.operation_message.as_deref().map(redact);
            for resource in &mut app.resources {
                resource.message = redact(&resource.message);
            }
            for container in app
                .pods
                .iter_mut()
                .flat_map(|pod| pod.containers.iter_mut())
            {
                container.message = container.message.as_deref().map(redact);
            }
            for event in &mut app.events {
                event.message = event.message.as_deref().map(redact);
            }
            for log in app.logs.values_mut() {
                *log = redact(log);
            }
            for log in app.previous_logs.values_mut() {
                *log = redact(log);
            }
        }
        for line in report
            .argocd
            .application_controller
            .iter_mut()
            .chain(report.argocd.repo_server.iter_mut())
        {
            *line = redact(line);
        }
        report
    }
}

fn append_to(path: &Path, contents: &str) -> Result<(), DiagnosticsError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| DiagnosticsError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(contents.as_bytes())
        .map_err(|source| DiagnosticsError::Write {
            path: path.to_path_buf(),
            source,
        })
}

fn escape_github_command(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
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
    if line.trim().is_empty() {
        return line.to_owned();
    }

    if let Some(index) = find_ignore_case(line, "bearer ") {
        return format!("{}<redacted>", &line[..index + 7]);
    }

    for (index, character) in line.char_indices() {
        if character != '=' && character != ':' {
            continue;
        }
        let key = last_key_token(&line[..index]);
        if is_sensitive_key(&key) {
            return format!("{}<redacted>", &line[..=index]);
        }
    }

    if is_sensitive_key(line.trim()) {
        return "<redacted>".to_owned();
    }

    line.to_owned()
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack.to_ascii_lowercase().find(needle)
}

fn last_key_token(prefix: &str) -> String {
    prefix
        .split_whitespace()
        .next_back()
        .unwrap_or_default()
        .trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && character != '_' && character != '-'
        })
        .to_owned()
}

fn is_sensitive_key(key: &str) -> bool {
    const WORDS: &[&str] = &[
        "PASSWORD",
        "PASSWD",
        "TOKEN",
        "SECRET",
        "AUTHORIZATION",
        "PRIVATE_KEY",
        "PRIVATE-KEY",
        "API_KEY",
        "APIKEY",
        "API-KEY",
        "ACCESS_KEY",
        "CERTIFICATE-AUTHORITY-DATA",
        "CLIENT-CERTIFICATE-DATA",
        "CLIENT-KEY-DATA",
        "KUBECONFIG",
    ];

    let upper = key.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return false;
    }
    if WORDS.iter().any(|word| upper.contains(word)) {
        return true;
    }
    // Match KEY only as a whole token so MONKEY and KEYBOARD stay readable.
    upper
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| token == "KEY")
}

/// Recursively redacts sensitive keys in a JSON value.
///
/// Secret `data`/`stringData`, kubeconfig fields and environment entries whose
/// `name` matches PASSWORD/TOKEN/KEY/SECRET are replaced.
pub fn redact_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let sensitive_name = map
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(is_sensitive_key);
            if sensitive_name && let Some(value) = map.get_mut("value") {
                *value = serde_json::Value::String("<redacted>".to_owned());
            }

            for (key, value) in map {
                let lower = key.to_ascii_lowercase();
                if is_sensitive_key(key)
                    || lower == "data"
                    || lower == "stringdata"
                    || lower == "kubeconfig"
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
    use std::sync::Mutex;

    use serde_json::json;

    use crate::kube::{ApplyOptions, DeleteOptions, KubeError, ResourceType};

    use super::*;

    #[derive(Default)]
    struct FakeKube {
        applications: Vec<DynamicObject>,
        pods: BTreeMap<String, Vec<DynamicObject>>,
        events: BTreeMap<String, Vec<DynamicObject>>,
        logs: Mutex<BTreeMap<String, String>>,
    }

    impl FakeKube {
        fn with_logs(self, entries: &[(&str, &str)]) -> Self {
            let mut logs = self.logs.lock().expect("logs lock");
            for (key, value) in entries {
                logs.insert((*key).to_owned(), (*value).to_owned());
            }
            drop(logs);
            self
        }
    }

    fn log_key(namespace: &str, pod: &str, container: &str, previous: bool) -> String {
        format!(
            "{namespace}/{pod}/{container}{}",
            if previous { "/previous" } else { "" }
        )
    }

    impl KubeApi for FakeKube {
        fn apply(
            &self,
            _key: ResourceKey,
            _object: DynamicObject,
            _options: ApplyOptions,
        ) -> crate::BoxFuture<'_, Result<DynamicObject, KubeError>> {
            Box::pin(async { panic!("apply is not used by diagnostics tests") })
        }

        fn get(
            &self,
            key: ResourceKey,
        ) -> crate::BoxFuture<'_, Result<Option<DynamicObject>, KubeError>> {
            Box::pin(async move {
                Ok(self
                    .applications
                    .iter()
                    .find(|object| {
                        object.metadata.name.as_deref() == Some(key.name.as_str())
                            && object.metadata.namespace.as_deref() == key.namespace.as_deref()
                    })
                    .cloned())
            })
        }

        fn list(
            &self,
            resource: ResourceType,
            namespace: Option<String>,
            _label_selector: Option<String>,
        ) -> crate::BoxFuture<'_, Result<Vec<DynamicObject>, KubeError>> {
            Box::pin(async move {
                let namespace = namespace.unwrap_or_default();
                let objects = match resource.kind {
                    "Pod" => self.pods.get(&namespace),
                    "Event" => self.events.get(&namespace),
                    _ => None,
                };
                Ok(objects.cloned().unwrap_or_default())
            })
        }

        fn delete(
            &self,
            _key: ResourceKey,
            _options: DeleteOptions,
        ) -> crate::BoxFuture<'_, Result<(), KubeError>> {
            Box::pin(async { Ok(()) })
        }

        fn logs(&self, request: LogRequest) -> crate::BoxFuture<'_, Result<String, KubeError>> {
            Box::pin(async move {
                let key = log_key(
                    &request.namespace,
                    &request.pod,
                    request.container.as_deref().unwrap_or("-"),
                    request.previous,
                );
                match self.logs.lock().expect("logs lock").get(&key) {
                    Some(log) => Ok(log.clone()),
                    None => Err(KubeError::MissingNamespace { kind: "Pod" }),
                }
            })
        }
    }

    fn app_object() -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "Application",
            "metadata": {"name": "alpha", "namespace": "argocd"},
            "status": {
                "health": {"status": "Degraded", "message": "token=abc"},
                "sync": {"status": "OutOfSync", "revision": "rev-1"},
                "operationState": {
                    "phase": "Failed",
                    "message": "password=hunter2"
                }
            }
        }))
        .expect("application object")
    }

    fn pod_object(namespace: &str, name: &str) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": name, "namespace": namespace},
            "spec": {
                "nodeName": "worker",
                "containers": [{"name": "app"}, {"name": "sidecar"}]
            },
            "status": {
                "phase": "Running",
                "containerStatuses": [
                    {
                        "name": "sidecar",
                        "ready": true,
                        "restartCount": 0,
                        "state": {"running": {}}
                    },
                    {
                        "name": "app",
                        "ready": false,
                        "restartCount": 3,
                        "state": {
                            "waiting": {
                                "reason": "CrashLoopBackOff",
                                "message": "PASSWORD=hunter2"
                            }
                        },
                        "lastState": {
                            "terminated": {"exitCode": 137, "reason": "Error"}
                        }
                    }
                ]
            }
        }))
        .expect("pod object")
    }

    fn event_object(name: &str, timestamp: &str, message: &str) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Event",
            "metadata": {"name": name, "namespace": "alpha"},
            "involvedObject": {"kind": "Pod", "name": "alpha-pod"},
            "reason": "BackOff",
            "message": message,
            "count": 4,
            "lastTimestamp": timestamp
        }))
        .expect("event object")
    }

    fn argo_pod(name: &str, container: &str) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": name, "namespace": "argocd"},
            "spec": {"containers": [{"name": container}]},
            "status": {"phase": "Running"}
        }))
        .expect("argo pod object")
    }

    fn fake_kube() -> FakeKube {
        let mut pods = BTreeMap::new();
        pods.insert("alpha".to_owned(), vec![pod_object("alpha", "alpha-pod")]);
        pods.insert(
            "argocd".to_owned(),
            vec![
                argo_pod("argocd-repo-server-abc", "argocd-repo-server"),
                argo_pod(
                    "argocd-application-controller-0",
                    "argocd-application-controller",
                ),
            ],
        );

        let mut events = BTreeMap::new();
        events.insert(
            "alpha".to_owned(),
            vec![
                event_object("event-b", "2026-01-01T00:00:02Z", "token=abc happened"),
                event_object("event-a", "2026-01-01T00:00:01Z", "ordinary event"),
            ],
        );

        FakeKube {
            applications: vec![app_object()],
            pods,
            events,
            logs: Mutex::new(BTreeMap::new()),
        }
        .with_logs(&[
            (
                "alpha/alpha-pod/app",
                "started\nPASSWORD=hunter2\nstill running\n",
            ),
            (
                "alpha/alpha-pod/app/previous",
                "previous token=abcdef\n",
            ),
            ("alpha/alpha-pod/sidecar", "sidecar is healthy\n"),
            (
                "argocd/argocd-application-controller-0/argocd-application-controller",
                "{\"level\":\"info\",\"msg\":\"ordinary line\"}\n{\"level\":\"error\",\"msg\":\"failed to sync\"}\n",
            ),
            (
                "argocd/argocd-repo-server-abc/argocd-repo-server",
                "generated manifest\ninvalid chart: timeout\n",
            ),
        ])
    }

    fn request() -> DiagnosticsRequest {
        DiagnosticsRequest::new("run-1", "alpha failed: token=abc").with_app(
            AppRequest::new(
                AppName::new("alpha").expect("app"),
                NamespaceName::new("alpha").expect("namespace"),
                "wait",
                RunResult::Failure,
            )
            .with_message("password=hunter2")
            .with_resources(vec![ResourceDiagnostic {
                api_version: "v1".to_owned(),
                kind: "Secret".to_owned(),
                namespace: Some("alpha".to_owned()),
                name: "credentials".to_owned(),
                message: "token=shhh".to_owned(),
            }]),
        )
    }

    #[tokio::test]
    async fn collects_sorted_redacted_diagnostics() {
        let kube = fake_kube();
        let report = DiagnosticsCollector::new(&kube, "argocd")
            .collect(request())
            .await;

        assert_eq!(report.apps.len(), 1);
        let app = &report.apps[0];
        assert_eq!(app.namespace.as_deref(), Some("alpha"));
        assert_eq!(app.health.as_deref(), Some("Degraded"));
        assert_eq!(app.sync.as_deref(), Some("OutOfSync"));
        assert_eq!(app.operation.as_deref(), Some("Failed"));
        assert_eq!(app.revision.as_deref(), Some("rev-1"));
        assert!(!app.message.as_deref().unwrap_or("").contains("hunter2"));
        assert!(!app.health_message.as_deref().unwrap_or("").contains("abc"));
        assert!(
            !app.operation_message
                .as_deref()
                .unwrap_or("")
                .contains("hunter2")
        );
        assert!(!app.resources[0].message.contains("shhh"));

        assert_eq!(app.pods.len(), 1);
        let pod = &app.pods[0];
        assert_eq!(pod.name, "alpha-pod");
        assert_eq!(pod.node.as_deref(), Some("worker"));
        assert_eq!(pod.restarts, 3);
        assert_eq!(pod.containers[0].name, "app");
        assert_eq!(pod.containers[1].name, "sidecar");
        assert_eq!(pod.containers[0].state.as_deref(), Some("waiting"));
        assert_eq!(
            pod.containers[0].reason.as_deref(),
            Some("CrashLoopBackOff")
        );
        assert_eq!(pod.containers[0].exit_code, None);
        assert_eq!(pod.containers[0].last_exit_code, Some(137));
        assert!(
            !pod.containers[0]
                .message
                .as_deref()
                .unwrap_or("")
                .contains("hunter2")
        );

        assert_eq!(app.events.len(), 2);
        assert_eq!(app.events[0].name, "event-a");
        assert_eq!(app.events[1].name, "event-b");
        assert!(
            !app.events[1]
                .message
                .as_deref()
                .unwrap_or("")
                .contains("abc")
        );

        let current = app
            .logs
            .get("alpha/alpha-pod/app")
            .expect("current app log");
        assert!(!current.contains("hunter2"));
        let previous = app
            .previous_logs
            .get("alpha/alpha-pod/app")
            .expect("previous app log");
        assert!(!previous.contains("abcdef"));
        assert!(
            app.logs
                .get("alpha/alpha-pod/sidecar")
                .is_some_and(|log| log.contains("healthy"))
        );

        assert_eq!(report.argocd.application_controller.len(), 1);
        assert!(report.argocd.application_controller[0].contains("failed to sync"));
        assert_eq!(report.argocd.repo_server.len(), 1);
        assert!(report.argocd.repo_server[0].contains("invalid chart"));
    }

    #[tokio::test]
    async fn collection_is_deterministic_apart_from_the_timestamp() {
        let kube = fake_kube();
        let collector = DiagnosticsCollector::new(&kube, "argocd");
        let first = collector.collect(request()).await;
        let second = collector.collect(request()).await;

        let mut first = serde_json::to_value(first).expect("first JSON");
        let mut second = serde_json::to_value(second).expect("second JSON");
        first
            .as_object_mut()
            .expect("report object")
            .remove("generated_at");
        second
            .as_object_mut()
            .expect("report object")
            .remove("generated_at");
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn writes_redacted_json_and_summary() {
        let kube = fake_kube();
        let report = DiagnosticsCollector::new(&kube, "argocd")
            .collect(request())
            .await;
        let directory = tempfile::tempdir().expect("tempdir");
        let json_path = report.write(directory.path()).expect("diagnostics written");
        let json = fs::read_to_string(&json_path).expect("json");
        let summary = fs::read_to_string(directory.path().join("summary.md")).expect("summary");

        for secret in ["hunter2", "abcdef", "shhh", "token=abc"] {
            assert!(!json.contains(secret), "JSON leaked {secret}");
            assert!(!summary.contains(secret), "summary leaked {secret}");
        }
        assert!(json.contains("\"kind\": \"Secret\""));
        assert!(summary.contains("alpha"));
        assert!(summary.contains("Argo CD application controller"));
    }

    #[tokio::test]
    async fn github_annotations_are_escaped_and_redacted() {
        let kube = fake_kube();
        let report = DiagnosticsCollector::new(&kube, "argocd")
            .collect(request())
            .await;
        let annotations = report.github_annotations();
        assert_eq!(annotations.len(), 1);
        assert!(annotations[0].starts_with("::error title=Astrarium app alpha::"));
        assert!(!annotations[0].contains("hunter2"));
        assert!(!annotations[0].contains('\n'));

        assert_eq!(escape_github_command("a%b\r\nc"), "a%25b%0D%0Ac");
    }

    #[test]
    fn step_summary_appends_to_the_configured_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("summary.md");
        append_to(&path, "first\n").expect("appended");
        append_to(&path, "second\n").expect("appended");
        assert_eq!(
            fs::read_to_string(path).expect("summary"),
            "first\nsecond\n"
        );
    }

    #[test]
    fn redacts_common_credentials() {
        let input = "PASSWORD=hunter2\napiToken=abc\nauthorization: Bearer abcdef\nplain=value\nSSH_KEY=xyz\nmonkey=banana";
        let output = redact(input);
        assert!(!output.contains("hunter2"));
        assert!(!output.contains("abcdef"));
        assert!(!output.contains("xyz"));
        assert!(!output.contains("abc\n"));
        assert!(output.contains("plain=value"));
        assert!(output.contains("monkey=banana"));
    }

    #[test]
    fn redacts_kubeconfig_fields() {
        let input = "\
certificate-authority-data: Y2VydA==
client-key-data: c2VjcmV0
server: https://127.0.0.1:6443
";
        let output = redact(input);
        assert!(!output.contains("Y2VydA=="));
        assert!(!output.contains("c2VjcmV0"));
        assert!(output.contains("server: https://127.0.0.1:6443"));
    }

    #[test]
    fn redacts_secret_json_data() {
        let mut value = json!({
            "kind": "Secret",
            "data": {"password": "aHVudGVyMg=="},
            "metadata": {"name": "credentials"}
        });
        redact_json(&mut value);
        assert_eq!(value["data"], "<redacted>");
        assert_eq!(value["metadata"]["name"], "credentials");
    }

    #[test]
    fn redacts_sensitive_environment_json_entries() {
        let mut value = json!({
            "env": [
                {"name": "DB_PASSWORD", "value": "hunter2"},
                {"name": "LOG_LEVEL", "value": "debug"},
                {"name": "SSH_KEY", "value": "private"}
            ]
        });
        redact_json(&mut value);
        assert_eq!(value["env"][0]["value"], "<redacted>");
        assert_eq!(value["env"][1]["value"], "debug");
        assert_eq!(value["env"][2]["value"], "<redacted>");
    }

    #[test]
    fn markdown_summary_redacts_messages() {
        let report = DiagnosticsReport {
            generated_at: Timestamp::now(),
            run_id: "run".to_owned(),
            summary: "token=abc".to_owned(),
            apps: vec![AppDiagnostics {
                app: Some(AppName::new("alpha").expect("app")),
                result: "failure".to_owned(),
                message: Some("password=hunter2".to_owned()),
                ..AppDiagnostics::default()
            }],
            argocd: ArgoCdDiagnostics::default(),
        };
        let markdown = report.summary_markdown();
        assert!(!markdown.contains("abc"));
        assert!(!markdown.contains("hunter2"));
        assert!(markdown.contains("alpha"));
    }
}
