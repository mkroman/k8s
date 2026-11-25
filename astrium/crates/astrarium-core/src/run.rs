//! End-to-end suite orchestration.
//!
//! A run creates a kind cluster, installs Argo CD, renders every selected app
//! into a temporary Helm chart repository, creates the namespaces, projects and
//! Applications directly, waits for each Application in dependency order, runs
//! the generic post-sync checks, and writes diagnostics when something fails.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{StreamExt as _, TryStreamExt as _};
use thiserror::Error;
use tracing::{Instrument as _, info, warn};

use crate::BoxFuture;
use crate::argocd::{
    ApplicationStatus, ArgoError, InstallError, PublishedChart, app_project,
    application_for_manifest, apply_app_project, apply_application, apply_namespace,
    install_argo_cd,
};
use crate::clock::Clock;
use crate::cluster::{ClusterError, ClusterOptions, ClusterProvider, KindCluster};
use crate::command::CommandRunner;
use crate::diagnostics::{AppRequest, DiagnosticsCollector, DiagnosticsError, DiagnosticsRequest};
use crate::kube::{
    APPLICATION as APPLICATION_RESOURCE, ApplyOptions, DAEMON_SET, DEPLOYMENT, DeleteOptions, JOB,
    KubeApi, KubeClient, KubeError, NAMESPACE, POD, ResourceKey, ResourceType, STATEFUL_SET,
};
use crate::manifest::{AppSpec, Manifest};
use crate::plan::Plan;
use crate::probe::{ProbeError, ProbeOutcome, check_app};
use crate::render::{
    ChartRenderer, ChartRepositoryServer, HelmHome, HttpChartRepository, RenderError,
    RenderRequest, RenderedApp, Renderers,
};
use crate::telemetry::{ErrorType, RunResult, record_task_error, record_task_result, task_span};
use crate::types::{AppName, ProjectName, ValueError};

/// Options for one suite run.
#[derive(Clone, Debug)]
pub struct RunOptions {
    /// Keep the cluster after a successful run.
    pub keep_cluster: bool,
    /// Reuse an existing cluster instead of creating one.
    pub reuse_cluster: bool,
    /// Fail as soon as a pod is in a fatal crash loop.
    pub fast_fail: bool,
    /// The unique run ID, shared with telemetry and diagnostics.
    pub run_id: String,
}

/// The result of one app.
#[derive(Clone, Debug)]
pub struct AppRunResult {
    /// The app name.
    pub app: AppName,
    /// The app result.
    pub result: RunResult,
    /// How long the app took to become healthy.
    pub duration: Duration,
    /// The generic checks that passed.
    pub probe: Option<ProbeOutcome>,
}

/// A completed suite run.
#[derive(Clone, Debug)]
pub struct RunReport {
    /// The cluster name.
    pub cluster: String,
    /// Per-app results in dependency order.
    pub apps: Vec<AppRunResult>,
    /// Whether the cluster was left running.
    pub kept_cluster: bool,
    /// Diagnostics written for a failure.
    pub diagnostics: Option<PathBuf>,
}

impl RunReport {
    /// Renders a concise human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut output = format!("Astrarium run: {}\n", self.cluster);
        for app in &self.apps {
            output.push_str(&format!(
                "  {:<28} {:<10} {:>6.1}s\n",
                app.app.as_str(),
                app.result.as_str(),
                app.duration.as_secs_f64()
            ));
        }
        if self.kept_cluster {
            output.push_str("  cluster kept for inspection\n");
        }
        if let Some(diagnostics) = &self.diagnostics {
            output.push_str(&format!("  diagnostics: {}\n", diagnostics.display()));
        }
        output
    }
}

/// An error from the run orchestrator.
#[derive(Debug, Error)]
pub enum RunError {
    /// The cluster could not be created or deleted.
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    /// A manifest path or reference is invalid.
    #[error(transparent)]
    Manifest(#[from] crate::manifest::ManifestError),
    /// A chart could not be rendered.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// Argo CD could not be installed.
    #[error(transparent)]
    Install(#[from] InstallError),
    /// An Argo CD resource could not be built or applied.
    #[error(transparent)]
    Argo(#[from] ArgoError),
    /// A Kubernetes API request failed.
    #[error(transparent)]
    Kube(#[from] KubeError),
    /// A generic probe failed.
    #[error(transparent)]
    Probe(#[from] ProbeError),
    /// Diagnostics could not be collected.
    #[error(transparent)]
    Diagnostics(#[from] DiagnosticsError),
    /// An app did not become healthy.
    #[error("app {app} failed: {message}")]
    AppFailed {
        /// The app name.
        app: AppName,
        /// A human-readable failure message.
        message: String,
    },
    /// A manifest value is invalid.
    #[error(transparent)]
    ValueError(#[from] ValueError),
    /// The suite configuration cannot satisfy a run precondition.
    #[error("{0}")]
    Config(String),
    /// A filesystem operation failed.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// An app has no rendered chart.
    #[error("app {0} has no rendered chart")]
    MissingChart(AppName),
    /// An app is missing from the manifest.
    #[error("app {0} is not declared in the manifest")]
    MissingApp(String),
    /// A namespace is missing from the manifest.
    #[error("namespace {0} is not declared in the manifest")]
    MissingNamespace(String),
    /// A project is missing from the manifest.
    #[error("project {0} is not declared in the manifest")]
    MissingProject(ProjectName),
}

impl RunError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Runs the selected suite.
///
/// # Errors
///
/// Returns [`RunError`] when the cluster, Argo CD installation, rendering,
/// Argo CD resources, an app wait, or a probe fails. The cluster is kept on
/// failure so diagnostics can be inspected.
pub async fn run_suite(
    manifest: &Manifest,
    plan: &Plan,
    options: RunOptions,
    runner: Arc<dyn CommandRunner>,
    clock: Arc<dyn Clock>,
) -> Result<RunReport, RunError> {
    let root = manifest.project.root.clone();
    let cluster_name = manifest.cluster.name.clone();
    let work_dir = root.join(".astrium/clusters").join(&cluster_name);
    fs::create_dir_all(&work_dir).map_err(|source| RunError::io(&work_dir, source))?;
    let kind_config = manifest
        .cluster
        .kind_config
        .as_ref()
        .map(|path| manifest.resolve_path(path))
        .transpose()?;
    let kubeconfig = manifest.resolve_output_path(&manifest.cluster.kubeconfig)?;

    let cluster = KindCluster::new(runner.clone());
    let handle = cluster
        .create(ClusterOptions {
            name: cluster_name.clone(),
            kind_config,
            kubeconfig: kubeconfig.clone(),
            node_image: manifest.cluster.node_image.clone(),
            reuse: options.reuse_cluster || manifest.cluster.reuse,
            keep: options.keep_cluster || manifest.cluster.keep,
            registry_enabled: manifest.registry.enabled,
            registry_host: Some(manifest.registry.host.clone()),
            mirrors: manifest.registry.mirrors.clone(),
            work_dir: work_dir.clone(),
        })
        .await?;

    info!(
        cluster = %handle.name,
        kubeconfig = %handle.kubeconfig.display(),
        "cluster is ready"
    );

    let kube = KubeClient::from_kubeconfig(&handle.kubeconfig).await?;

    if let Err(error) = install_argo_cd(runner.as_ref(), &kube, manifest).await {
        warn!(%error, "Argo CD installation failed; keeping the cluster");
        return Err(error.into());
    }

    if let Err(error) = prepare_cold_start_apps(manifest, plan, &cluster, &handle, &kube).await {
        warn!(%error, "cold-start preparation failed; keeping the cluster");
        return Err(error);
    }

    let render_result =
        render_apps(manifest, plan, &work_dir, runner.clone(), clock.as_ref()).await;
    let rendered = match render_result {
        Ok(rendered) => rendered,
        Err(error) => {
            warn!(%error, "rendering failed; keeping the cluster");
            return Err(error);
        }
    };

    let charts_dir = work_dir.join("charts");
    let repository = HttpChartRepository::for_manifest(runner.clone(), manifest);
    let served = match repository.serve(charts_dir).await {
        Ok(served) => served,
        Err(error) => {
            warn!(%error, "chart repository failed; keeping the cluster");
            return Err(error.into());
        }
    };
    info!(url = %served.url, "serving the local chart repository");

    let apply_result = apply_suite(manifest, plan, &kube, &served.url, &rendered).await;
    if let Err(error) = apply_result {
        served.shutdown();
        warn!(%error, "applying Argo CD resources failed; keeping the cluster");
        return Err(error);
    }

    let mut results = Vec::with_capacity(plan.apps.len());
    for planned in &plan.apps {
        let app = manifest
            .app(planned.name.as_str())
            .ok_or_else(|| RunError::MissingApp(planned.name.to_string()))?;
        let rendered = rendered
            .iter()
            .find(|rendered| rendered.app == planned.name)
            .ok_or_else(|| RunError::MissingChart(planned.name.clone()))?;
        let timeout = app
            .timeout_seconds
            .map_or_else(|| manifest.render.timeout.get(), Duration::from_secs);
        let span = task_span(
            "wait application",
            &format!("{}/{}", options.run_id, app.name),
            Some(app.name.as_str()),
            "app.wait",
        );
        let started = Instant::now();
        let wait = wait_for_application(
            &kube,
            manifest.argocd.namespace.as_str(),
            app,
            &rendered.chart_version.to_string(),
            timeout,
            options.fast_fail,
            clock.as_ref(),
        )
        .instrument(span.clone())
        .await;

        if let Err(message) = wait {
            record_task_error(&span, ErrorType::Argocd);
            let diagnostics = collect_diagnostics(
                &kube,
                manifest,
                plan,
                &options.run_id,
                &results,
                app,
                &message,
            )
            .await;
            served.shutdown();
            warn!(
                cluster = %handle.name,
                kubeconfig = %handle.kubeconfig.display(),
                diagnostics = ?diagnostics,
                "app failed; keeping the cluster for inspection"
            );
            return Err(RunError::AppFailed {
                app: app.name.clone(),
                message,
            });
        }
        record_task_result(&span, RunResult::Success);

        let probe_span = task_span(
            "run probe",
            &format!("{}/{}", options.run_id, app.name),
            Some(app.name.as_str()),
            "app.probe",
        );
        let probe = wait_for_probe(&kube, app, clock.as_ref())
            .instrument(probe_span.clone())
            .await;
        let probe = match probe {
            Ok(probe) => probe,
            Err(message) => {
                record_task_error(&probe_span, ErrorType::Probe);
                let diagnostics = collect_diagnostics(
                    &kube,
                    manifest,
                    plan,
                    &options.run_id,
                    &results,
                    app,
                    &message,
                )
                .await;
                served.shutdown();
                warn!(
                    cluster = %handle.name,
                    kubeconfig = %handle.kubeconfig.display(),
                    diagnostics = ?diagnostics,
                    "probe failed; keeping the cluster for inspection"
                );
                return Err(RunError::AppFailed {
                    app: app.name.clone(),
                    message,
                });
            }
        };
        record_task_result(&probe_span, RunResult::Success);

        results.push(AppRunResult {
            app: app.name.clone(),
            result: RunResult::Success,
            duration: started.elapsed(),
            probe: Some(probe),
        });
    }

    served.shutdown();
    cluster.cleanup(&handle, options.keep_cluster).await?;

    Ok(RunReport {
        cluster: handle.name,
        apps: results,
        kept_cluster: options.keep_cluster || !handle.created,
        diagnostics: None,
    })
}

async fn prepare_cold_start_apps(
    manifest: &Manifest,
    plan: &Plan,
    cluster: &KindCluster<Arc<dyn CommandRunner>>,
    handle: &crate::cluster::ClusterHandle,
    kube: &dyn KubeApi,
) -> Result<(), RunError> {
    let mut verified_containerd = false;
    for planned in &plan.apps {
        let app = manifest
            .app(planned.name.as_str())
            .ok_or_else(|| RunError::MissingApp(planned.name.to_string()))?;
        if app.requires_containerd_config && !verified_containerd {
            cluster.verify_containerd_config(&handle.name).await?;
            verified_containerd = true;
        }
        if app.stale_probe_resources.is_some() {
            clear_stale_probe_resources(kube, app).await?;
        }
        if app.requires_fresh_image {
            let image = fresh_image(manifest, app)?;
            cluster
                .remove_image_from_nodes(&handle.name, &image)
                .await?;
        }
    }
    Ok(())
}

fn fresh_image(manifest: &Manifest, app: &AppSpec) -> Result<String, RunError> {
    let path = manifest.resolve_path(&app.source.join("values-kind.yaml"))?;
    let source = std::fs::read_to_string(&path).map_err(|source| RunError::io(&path, source))?;
    let value: serde_json::Value = yaml_serde::from_str(&source)
        .map_err(|error| RunError::Config(format!("invalid {}: {error}", path.display())))?;
    value
        .pointer("/e2e/pullImage")
        .and_then(serde_json::Value::as_str)
        .filter(|image| !image.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            RunError::Config(format!(
                "app {} requires a fresh image but {} has no e2e.pullImage",
                app.name,
                path.display()
            ))
        })
}

async fn clear_stale_probe_resources(kube: &dyn KubeApi, app: &AppSpec) -> Result<(), RunError> {
    let Some(stale) = &app.stale_probe_resources else {
        return Ok(());
    };
    let namespace = stale.namespace.as_str();
    if kube
        .get(ResourceKey::cluster(NAMESPACE, namespace))
        .await?
        .is_none()
    {
        return Ok(());
    }

    for resource in &stale.resource_names {
        let (kind, name) = resource.split_once('/').ok_or_else(|| {
            RunError::Config(format!(
                "stale resource {resource:?} for {} must be kind/name",
                app.name
            ))
        })?;
        let resource_type = stale_resource_type(kind).ok_or_else(|| {
            RunError::Config(format!(
                "stale resource kind {kind:?} for {} is not supported",
                app.name
            ))
        })?;
        kube.delete(
            ResourceKey::namespaced(resource_type, namespace, name),
            DeleteOptions::default(),
        )
        .await?;
    }

    for selector in &stale.pod_label_selectors {
        let pods = kube
            .list(POD, Some(namespace.to_owned()), Some(selector.clone()))
            .await?;
        for pod in pods {
            let Some(name) = pod.metadata.name.clone() else {
                continue;
            };
            kube.delete(
                ResourceKey::namespaced(POD, namespace, name),
                DeleteOptions {
                    grace_period: Some(0),
                    ..DeleteOptions::default()
                },
            )
            .await?;
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let remaining = kube
                .list(POD, Some(namespace.to_owned()), Some(selector.clone()))
                .await?;
            if remaining.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(RunError::Config(format!(
                    "stale probe pods for {} did not terminate: {}",
                    app.name,
                    remaining
                        .iter()
                        .filter_map(|pod| pod.metadata.name.as_deref())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    Ok(())
}

fn stale_resource_type(kind: &str) -> Option<ResourceType> {
    match kind.to_ascii_lowercase().as_str() {
        "daemonset" => Some(DAEMON_SET),
        "deployment" => Some(DEPLOYMENT),
        "job" => Some(JOB),
        "pod" => Some(POD),
        "statefulset" => Some(STATEFUL_SET),
        _ => None,
    }
}

async fn render_apps(
    manifest: &Manifest,
    plan: &Plan,
    work_dir: &std::path::Path,
    runner: Arc<dyn CommandRunner>,
    _clock: &dyn Clock,
) -> Result<Vec<RenderedApp>, RunError> {
    let charts = work_dir.join("charts");
    if charts.exists() {
        fs::remove_dir_all(&charts).map_err(|source| RunError::io(&charts, source))?;
    }
    fs::create_dir_all(&charts).map_err(|source| RunError::io(&charts, source))?;

    let renderers = Renderers::new(runner).with_helm_home(HelmHome::temporary()?);
    let requests = plan
        .apps
        .iter()
        .map(|planned| {
            let app = manifest
                .app(planned.name.as_str())
                .ok_or_else(|| RunError::MissingApp(planned.name.to_string()))?;
            RenderRequest::from_manifest(manifest, app, charts.clone()).map_err(RunError::from)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let rendered = futures::stream::iter(requests)
        .map(|request| renderers.render(request))
        .buffer_unordered(4)
        .try_collect::<Vec<_>>()
        .await?;
    Ok(rendered)
}

async fn apply_suite(
    manifest: &Manifest,
    plan: &Plan,
    kube: &dyn KubeApi,
    repository_url: &str,
    rendered: &[RenderedApp],
) -> Result<(), RunError> {
    let argocd_namespace = manifest.argocd.namespace.as_str();
    let mut namespaces = BTreeSet::new();
    let mut projects = BTreeSet::new();
    for planned in &plan.apps {
        let app = manifest
            .app(planned.name.as_str())
            .ok_or_else(|| RunError::MissingApp(planned.name.to_string()))?;
        namespaces.insert(app.namespace.clone());
        let project_name = app.project_name()?;
        projects.insert(project_name.clone());
        if let Some(project) = manifest.project(project_name.as_str()) {
            for destination in &project.destinations {
                if destination.namespace != "*" {
                    namespaces.insert(crate::NamespaceName::new(destination.namespace.clone())?);
                }
            }
        }
    }

    for namespace in namespaces {
        let spec = manifest
            .namespace(namespace.as_str())
            .ok_or_else(|| RunError::MissingNamespace(namespace.to_string()))?;
        apply_namespace(kube, spec, ApplyOptions::default()).await?;
    }

    for project_name in projects {
        let project = manifest
            .project(project_name.as_str())
            .ok_or_else(|| RunError::MissingProject(project_name.clone()))?;
        let resource = app_project(
            project,
            argocd_namespace,
            repository_url,
            &manifest.argocd.server,
        );
        apply_app_project(kube, &resource, ApplyOptions::default()).await?;
    }

    let rendered_by_name: BTreeMap<&str, &RenderedApp> = rendered
        .iter()
        .map(|rendered| (rendered.app.as_str(), rendered))
        .collect();
    for planned in &plan.apps {
        let app = manifest
            .app(planned.name.as_str())
            .ok_or_else(|| RunError::MissingApp(planned.name.to_string()))?;
        let rendered = rendered_by_name
            .get(app.name.as_str())
            .ok_or_else(|| RunError::MissingChart(app.name.clone()))?;
        let chart = PublishedChart::from_rendered(rendered, repository_url);
        let resource = application_for_manifest(
            manifest,
            app,
            &chart,
            argocd_namespace,
            &manifest.argocd.server,
        )?;
        apply_application(kube, &resource, ApplyOptions::default()).await?;
    }
    Ok(())
}

async fn wait_for_application(
    kube: &dyn KubeApi,
    argocd_namespace: &str,
    app: &AppSpec,
    expected_revision: &str,
    timeout: Duration,
    fast_fail: bool,
    clock: &dyn Clock,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut revision_deadline: Option<Instant> = None;

    loop {
        if Instant::now() >= deadline {
            return Err(format!(
                "Application {} did not become Synced and Healthy within {timeout:?}",
                app.name
            ));
        }

        if fast_fail
            && let Some(message) = fatal_pod(kube, app)
                .await
                .map_err(|error| error.to_string())?
        {
            return Err(message);
        }

        let key =
            ResourceKey::namespaced(APPLICATION_RESOURCE, argocd_namespace, app.name.as_str());
        let object = kube.get(key).await.map_err(|error| error.to_string())?;
        if let Some(object) = object {
            let status: ApplicationStatus = object
                .data
                .get("status")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| format!("invalid Application status: {error}"))?
                .unwrap_or_default();
            let health = status
                .health
                .as_ref()
                .and_then(|health| health.status.as_deref())
                .unwrap_or("Unknown");
            let sync = status
                .sync
                .as_ref()
                .and_then(|sync| sync.status.as_deref())
                .unwrap_or("Unknown");
            let operation = status.operation_state.as_ref();
            let phase = operation.map_or("", |operation| operation.phase.as_str());
            let message = operation
                .and_then(|operation| operation.message.as_deref())
                .unwrap_or("no message");

            match phase {
                "Failed" | "Error" => {
                    let retry_count = operation.map_or(0, |operation| operation.retry_count);
                    let retry_limit = i64::from(app.sync_policy.retry_limit);
                    if retry_limit < 0 || retry_count < retry_limit {
                        warn!(
                            app = %app.name,
                            retry_count,
                            retry_limit,
                            "sync attempt failed; waiting for the retry budget"
                        );
                    } else {
                        return Err(format!(
                            "Application {} sync operation {phase}: {message}",
                            app.name
                        ));
                    }
                }
                "Running" | "Terminating" => {}
                _ => {
                    if health == "Healthy" && sync == "Synced" {
                        let revision = status
                            .sync
                            .as_ref()
                            .and_then(|sync| sync.revision.as_deref())
                            .unwrap_or("");
                        if revision == expected_revision {
                            return Ok(());
                        }
                        let revision_deadline = *revision_deadline
                            .get_or_insert_with(|| Instant::now() + Duration::from_secs(30));
                        if Instant::now() >= revision_deadline {
                            return Err(format!(
                                "Application {} synced revision {revision:?}, expected {expected_revision:?}",
                                app.name
                            ));
                        }
                    }
                }
            }
        }

        clock.sleep(Duration::from_secs(1)).await;
    }
}

async fn fatal_pod(kube: &dyn KubeApi, app: &AppSpec) -> Result<Option<String>, KubeError> {
    let pods = kube
        .list(POD, Some(app.namespace.to_string()), None)
        .await?;
    for pod in pods {
        let name = pod.metadata.name.as_deref().unwrap_or("<unknown>");
        let Some(containers) = pod
            .data
            .pointer("/status/containerStatuses")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for container in containers {
            let restarts = container
                .get("restartCount")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let reason = container
                .pointer("/state/waiting/reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if restarts >= 3 && reason == "CrashLoopBackOff" {
                let container_name = container
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("<unknown>");
                return Ok(Some(format!(
                    "pod {}/{} container {container_name} is in CrashLoopBackOff after {restarts} restarts",
                    app.namespace, name
                )));
            }
        }
    }
    Ok(None)
}

async fn wait_for_probe(
    kube: &dyn KubeApi,
    app: &AppSpec,
    clock: &dyn Clock,
) -> Result<ProbeOutcome, String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match check_app(kube, app).await {
            Ok(outcome) => return Ok(outcome),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "post-sync checks for {} did not pass: {error}",
                        app.name
                    ));
                }
            }
        }
        clock.sleep(Duration::from_secs(2)).await;
    }
}

async fn collect_diagnostics(
    kube: &dyn KubeApi,
    manifest: &Manifest,
    plan: &Plan,
    run_id: &str,
    results: &[AppRunResult],
    failed: &AppSpec,
    message: &str,
) -> Option<PathBuf> {
    let mut request = DiagnosticsRequest::new(run_id, format!("app {} failed", failed.name));
    let completed: HashSet<&str> = results.iter().map(|result| result.app.as_str()).collect();
    for planned in &plan.apps {
        let app = manifest.app(planned.name.as_str())?;
        let result = if app.name == failed.name {
            RunResult::Failure
        } else if completed.contains(app.name.as_str()) {
            RunResult::Success
        } else {
            RunResult::Skip
        };
        let mut app_request = AppRequest::new(
            app.name.clone(),
            app.namespace.clone(),
            format!("app.{}", result.as_str()),
            result,
        );
        if app.name == failed.name {
            app_request = app_request.with_message(message.to_owned());
        }
        request = request.with_app(app_request);
    }

    let directory = manifest
        .project
        .root
        .join(".astrium/diagnostics")
        .join(run_id);
    let collector = DiagnosticsCollector::new(kube, manifest.argocd.namespace.as_str());
    let report = collector.collect(request).await;
    match report.write(&directory) {
        Ok(path) => {
            let _ = report.append_github_step_summary();
            for annotation in report.github_annotations() {
                eprintln!("{annotation}");
            }
            Some(path)
        }
        Err(error) => {
            warn!(%error, "failed to write diagnostics");
            None
        }
    }
}

/// A boxed future helper for tests that want to drive a fake clock.
pub type ClockFuture<'a, T> = BoxFuture<'a, T>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_error_keeps_app_name() {
        let error = RunError::AppFailed {
            app: AppName::new("demo").unwrap(),
            message: "boom".to_owned(),
        };
        assert!(error.to_string().contains("demo"));
    }
}
