//! `astrarium upgrade` and the generic in-cluster upgrade test.
//!
//! The command first deploys the app and its prerequisites through the normal
//! Argo CD run path. It then installs the same Helm chart into a disposable
//! `<namespace>-rollout` namespace, upgrades it in place, restarts its pods and
//! verifies that the pod identity changed, a marker written below
//! `upgrade.data_path` survived, and its PersistentVolumeClaims kept their
//! identity and bind state. Finally it uninstalls the release and, unless
//! configured otherwise, checks that the PVCs survived the uninstall.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tracing::{info, warn};

use crate::clock::Clock;
use crate::cluster::{ClusterError, KindCluster};
use crate::command::{CommandError, CommandRunner, CommandSpec, Tool};
use crate::kube::{
    DEFAULT_EXEC_TIMEOUT, DeleteOptions, ExecRequest, KubeApi, KubeClient, KubeError, NAMESPACE,
    PERSISTENT_VOLUME_CLAIM, POD, ResourceKey,
};
use crate::manifest::{
    AppSpec, Manifest, ManifestError, RendererKind, UpgradeSecurityProfile, UpgradeSpec,
};
use crate::plan::Plan;
use crate::render::{ChartRenderer, HelmHome, HelmRenderer, RenderError, RenderRequest};
use crate::run::{RunError, RunOptions, RunReport, run_suite};
use crate::types::{AppName, NamespaceName, ValueError};

/// Options for one upgrade run.
#[derive(Clone, Debug)]
pub struct UpgradeOptions {
    /// Keep the cluster after a successful run.
    pub keep_cluster: bool,
    /// Reuse an existing cluster instead of creating one.
    pub reuse_cluster: bool,
    /// Skip the normal suite run and assume the app is already deployed.
    pub skip_run: bool,
    /// The unique run ID, shared with telemetry and diagnostics.
    pub run_id: String,
}

/// The result of an upgrade run.
#[derive(Clone, Debug)]
pub struct UpgradeReport {
    /// The app that was upgraded.
    pub app: AppName,
    /// The normal Argo CD run that established the app and its prerequisites.
    pub run: RunReport,
    /// The disposable namespace used by the upgrade test.
    pub namespace: String,
    /// The replacement pod that became ready after the restart.
    pub pod: String,
    /// How long the in-cluster upgrade test took.
    pub duration: Duration,
}

impl UpgradeReport {
    /// Renders a concise human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut output = self.run.summary();
        output.push_str(&format!(
            "  upgrade {:<20} passed in {:.1}s (namespace {}, pod {})\n",
            self.app.as_str(),
            self.duration.as_secs_f64(),
            self.namespace,
            self.pod
        ));
        output
    }
}

/// An error from the upgrade command.
#[derive(Debug, Error)]
pub enum UpgradeError {
    /// The prerequisite suite run failed.
    #[error(transparent)]
    Run(#[from] RunError),
    /// A Kubernetes API request failed.
    #[error(transparent)]
    Kube(#[from] KubeError),
    /// The chart could not be rendered.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// Helm failed.
    #[error(transparent)]
    Command(#[from] CommandError),
    /// The cluster could not be deleted.
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    /// The manifest could not be resolved.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A manifest value is invalid.
    #[error(transparent)]
    Value(#[from] ValueError),
    /// An app is missing from the manifest.
    #[error("app {0} is not declared in the manifest")]
    MissingApp(String),
    /// The app does not opt into the upgrade test.
    #[error("app {app} has no upgrade test in the manifest")]
    NotConfigured {
        /// The app name.
        app: AppName,
    },
    /// The app's upgrade test is disabled.
    #[error("app {app} upgrade test is disabled")]
    Disabled {
        /// The app name.
        app: AppName,
    },
    /// The generic test only supports Helm charts.
    #[error("app {app} upgrade test requires a Helm chart, not {renderer:?}")]
    NotHelm {
        /// The app name.
        app: AppName,
        /// The configured renderer.
        renderer: RendererKind,
    },
    /// A check failed.
    #[error("{0}")]
    Check(String),
    /// A filesystem operation failed.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl UpgradeError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Deploys the planned apps, then runs the upgrade test for `target`.
///
/// The cluster is kept on failure so the rollout namespace and its pods remain
/// available for diagnostics. On success it is deleted unless
/// `options.keep_cluster` is set, the cluster was reused, or the manifest keeps
/// it.
///
/// # Errors
///
/// Returns [`UpgradeError`] when the prerequisite run, the chart render, a
/// Helm command, the Kubernetes checks, or the cluster cleanup fails.
pub async fn upgrade_suite(
    manifest: &Manifest,
    plan: &Plan,
    target: &AppName,
    options: UpgradeOptions,
    runner: Arc<dyn CommandRunner>,
    clock: Arc<dyn Clock>,
) -> Result<UpgradeReport, UpgradeError> {
    let app = manifest
        .app(target.as_str())
        .ok_or_else(|| UpgradeError::MissingApp(target.to_string()))?;
    let upgrade = configured_upgrade(app)?;

    let reused = options.reuse_cluster || manifest.cluster.reuse;
    let run_id = options.run_id.clone();
    let run = if options.skip_run {
        RunReport {
            cluster: manifest.cluster.name.clone(),
            apps: Vec::new(),
            kept_cluster: true,
            diagnostics: None,
        }
    } else {
        run_suite(
            manifest,
            plan,
            RunOptions {
                keep_cluster: true,
                reuse_cluster: reused,
                fast_fail: false,
                run_id: options.run_id,
            },
            Arc::clone(&runner),
            Arc::clone(&clock),
        )
        .await?
    };
    if options.skip_run {
        let clusters = KindCluster::new(Arc::clone(&runner))
            .list_clusters()
            .await?;
        if !clusters.iter().any(|name| name == &manifest.cluster.name) {
            return Err(UpgradeError::Check(format!(
                "refusing to run --no-deploy against a kubeconfig that is not the kind cluster {:?}",
                manifest.cluster.name
            )));
        }
    }

    let (rollout_namespace, _) = rollout_names(app, &run_id)?;
    info!(
        cluster = %run.cluster,
        app = %app.name,
        namespace = %rollout_namespace.as_str(),
        "starting the in-cluster upgrade test"
    );

    let kubeconfig = manifest.resolve_output_path(&manifest.cluster.kubeconfig)?;
    let kube = KubeClient::from_kubeconfig(&kubeconfig).await?;
    let started = std::time::Instant::now();
    let outcome = perform_upgrade(
        manifest,
        app,
        upgrade,
        &kube,
        &kubeconfig,
        Arc::clone(&runner),
        clock.as_ref(),
        &run_id,
    )
    .await?;
    let duration = started.elapsed();

    let keep_cluster = options.keep_cluster || manifest.cluster.keep || reused || options.skip_run;
    if !keep_cluster {
        KindCluster::new(Arc::clone(&runner))
            .delete_cluster(&manifest.cluster.name)
            .await?;
    }

    Ok(UpgradeReport {
        app: app.name.clone(),
        run,
        namespace: outcome.namespace,
        pod: outcome.pod,
        duration,
    })
}

fn configured_upgrade(app: &AppSpec) -> Result<&UpgradeSpec, UpgradeError> {
    let upgrade = app
        .upgrade
        .as_ref()
        .ok_or_else(|| UpgradeError::NotConfigured {
            app: app.name.clone(),
        })?;
    if !upgrade.enabled {
        return Err(UpgradeError::Disabled {
            app: app.name.clone(),
        });
    }
    if app.renderer != RendererKind::Helm {
        return Err(UpgradeError::NotHelm {
            app: app.name.clone(),
            renderer: app.renderer,
        });
    }
    if upgrade.from_revision.is_some() {
        return Err(UpgradeError::Check(
            "`upgrade.from_revision` is not implemented; use `upgrade.from_values` with the previous values".to_owned(),
        ));
    }
    if upgrade.from_values.is_empty() {
        return Err(UpgradeError::Check(
            "the upgrade test requires at least one `upgrade.from_values` file so the initial deployment differs from the current values".to_owned(),
        ));
    }
    Ok(upgrade)
}

#[derive(Debug)]
struct UpgradeOutcome {
    namespace: String,
    pod: String,
}

#[allow(clippy::too_many_arguments)]
async fn perform_upgrade(
    manifest: &Manifest,
    app: &AppSpec,
    upgrade: &UpgradeSpec,
    kube: &dyn KubeApi,
    kubeconfig: &Path,
    runner: Arc<dyn CommandRunner>,
    clock: &dyn Clock,
    run_id: &str,
) -> Result<UpgradeOutcome, UpgradeError> {
    let (namespace, release) = rollout_names(app, run_id)?;
    let namespace = namespace.as_str().to_owned();
    let timeout = Duration::from_secs(upgrade.timeout_seconds.max(1));
    let selector = format!("app.kubernetes.io/instance={release}");

    // Start from a clean rollout namespace. A previous failed attempt may
    // still own the Helm release name, and leftover PVCs would invalidate the
    // retention check.
    if helm(
        runner.as_ref(),
        manifest,
        kubeconfig,
        timeout,
        [
            "uninstall",
            &release,
            "--namespace",
            &namespace,
            "--wait",
            "--timeout",
            &format!("{}s", timeout.as_secs()),
        ]
        .map(str::to_owned),
    )
    .await
    .is_err()
    {
        info!(release = %release, namespace = %namespace, "no previous rollout release to remove");
    }
    delete_namespace(kube, &namespace, timeout, clock).await?;

    let work = tempfile::Builder::new()
        .prefix("astrarium-upgrade-")
        .tempdir()
        .map_err(|source| UpgradeError::io(std::env::temp_dir(), source))?;
    let output_dir = work.path().join("charts");
    let base_request = RenderRequest::from_manifest(manifest, app, output_dir)?;
    let mut baseline_request = base_request.clone();
    baseline_request.namespace = NamespaceName::new(namespace.clone())?;
    baseline_request.release_name = release.clone();
    baseline_request.inline_values = disable_e2e(baseline_request.inline_values.clone());
    baseline_request.values_files.extend(
        upgrade
            .from_values
            .iter()
            .map(|path| manifest.resolve_path(&app.source.join(path)))
            .collect::<Result<Vec<_>, _>>()?,
    );

    let mut current_request = base_request;
    current_request.namespace = NamespaceName::new(namespace.clone())?;
    current_request.release_name = release.clone();
    current_request.inline_values = disable_e2e(current_request.inline_values);

    let renderer = HelmRenderer::new(Arc::clone(&runner)).with_home(HelmHome::temporary()?);
    let baseline = renderer.render(baseline_request).await?;
    let current = renderer.render(current_request).await?;
    if baseline.content_hash == current.content_hash {
        return Err(UpgradeError::Check(
            "upgrade.from_values did not change the rendered chart or values; the test would not exercise an upgrade".to_owned(),
        ));
    }

    let baseline_values_path = work.path().join("values-baseline.json");
    let current_values_path = work.path().join("values-current.json");
    write_values(&baseline_values_path, &baseline.values)?;
    write_values(&current_values_path, &current.values)?;

    let chart = current.chart_path.display().to_string();
    helm(
        runner.as_ref(),
        manifest,
        kubeconfig,
        timeout,
        [
            "install",
            &release,
            &chart,
            "--namespace",
            &namespace,
            "--create-namespace",
            "--wait",
            "--timeout",
            &format!("{}s", timeout.as_secs()),
            "--values",
            &baseline_values_path.display().to_string(),
        ]
        .map(str::to_owned),
    )
    .await?;

    let pod = wait_for_ready_pod(kube, &namespace, &selector, timeout, clock).await?;
    let container = container_name(&pod, upgrade.container.as_deref())?;
    if upgrade.security_profile == UpgradeSecurityProfile::Restricted {
        check_restricted(&pod, &container)?;
    }
    let before_pvcs = pvc_snapshot(kube, &namespace).await?;
    let pod_uid = pod.metadata.uid.clone().unwrap_or_default();
    let pod_name = pod
        .metadata
        .name
        .clone()
        .ok_or_else(|| UpgradeError::Check("the ready pod has no name".to_owned()))?;

    if let Some(data_path) = &upgrade.data_path {
        let marker = marker_path(data_path);
        let output = kube
            .exec(ExecRequest {
                namespace: namespace.clone(),
                pod: pod_name.clone(),
                container: Some(container.clone()),
                command: vec![
                    "/bin/sh".to_owned(),
                    "-ec".to_owned(),
                    format!("printf '%s' e2e-rollout > '{marker}'"),
                ],
                timeout: DEFAULT_EXEC_TIMEOUT,
            })
            .await?;
        if !output.success {
            return Err(UpgradeError::Check(format!(
                "could not write the rollout marker below {data_path}: {}",
                output.stderr.trim()
            )));
        }
    }

    info!(app = %app.name, namespace = %namespace, %run_id, "upgrading the Helm release");
    helm(
        runner.as_ref(),
        manifest,
        kubeconfig,
        timeout,
        [
            "upgrade",
            &release,
            &chart,
            "--namespace",
            &namespace,
            "--wait",
            "--timeout",
            &format!("{}s", timeout.as_secs()),
            "--values",
            &current_values_path.display().to_string(),
        ]
        .map(str::to_owned),
    )
    .await?;

    let replacement =
        wait_for_replacement_pod(kube, &namespace, &selector, &pod_uid, timeout, clock).await?;
    let replacement_name = replacement
        .metadata
        .name
        .clone()
        .ok_or_else(|| UpgradeError::Check("the replacement pod has no name".to_owned()))?;

    if let Some(data_path) = &upgrade.data_path {
        let marker = marker_path(data_path);
        let output = kube
            .exec(ExecRequest {
                namespace: namespace.clone(),
                pod: replacement_name.clone(),
                container: Some(container.clone()),
                command: vec!["cat".to_owned(), marker],
                timeout: DEFAULT_EXEC_TIMEOUT,
            })
            .await?;
        if !output.success || output.stdout.trim() != "e2e-rollout" {
            return Err(UpgradeError::Check(format!(
                "the data marker did not survive the rollout (got {:?})",
                output.stdout.trim()
            )));
        }
    }

    let after_pvcs = pvc_snapshot(kube, &namespace).await?;
    if before_pvcs != after_pvcs {
        return Err(UpgradeError::Check(format!(
            "PersistentVolumeClaim identity or bind state changed during the rollout: {before_pvcs:?} -> {after_pvcs:?}"
        )));
    }

    helm(
        runner.as_ref(),
        manifest,
        kubeconfig,
        timeout,
        [
            "uninstall",
            &release,
            "--namespace",
            &namespace,
            "--wait",
            "--timeout",
            &format!("{}s", timeout.as_secs()),
        ]
        .map(str::to_owned),
    )
    .await?;

    if upgrade.verify_pvc_retention && !before_pvcs.is_empty() {
        let retained = pvc_snapshot(kube, &namespace).await?;
        let missing: Vec<&str> = before_pvcs
            .keys()
            .filter(|name| !retained.contains_key(name.as_str()))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(UpgradeError::Check(format!(
                "helm uninstall deleted PersistentVolumeClaims: {}",
                missing.join(", ")
            )));
        }
    }

    if let Err(error) = delete_namespace(kube, &namespace, timeout, clock).await {
        warn!(namespace = %namespace, %error, "could not delete the rollout namespace");
    }

    Ok(UpgradeOutcome {
        namespace,
        pod: replacement_name,
    })
}

fn rollout_names(app: &AppSpec, run_id: &str) -> Result<(NamespaceName, String), UpgradeError> {
    let digest = Sha256::digest(run_id.as_bytes());
    let suffix: String = format!("{digest:x}").chars().take(8).collect();
    let namespace = format!("{}-rollout-{}", app.namespace.as_str(), suffix);
    let namespace = NamespaceName::new(namespace.clone()).map_err(|error| {
        UpgradeError::Check(format!(
            "the upgrade test cannot derive a namespace from {namespace:?}: {error}"
        ))
    })?;
    Ok((namespace, format!("{}-rollout-{}", app.name, suffix)))
}

fn disable_e2e(mut values: serde_json::Value) -> serde_json::Value {
    let Some(object) = values.as_object_mut() else {
        return serde_json::json!({"e2e": {"enabled": false}});
    };
    match object.get_mut("e2e") {
        Some(serde_json::Value::Object(e2e)) => {
            e2e.insert("enabled".to_owned(), serde_json::Value::Bool(false));
        }
        _ => {
            object.insert("e2e".to_owned(), serde_json::json!({"enabled": false}));
        }
    }
    values
}

fn write_values(path: &Path, values: &serde_json::Value) -> Result<(), UpgradeError> {
    let encoded = serde_json::to_vec_pretty(values).map_err(|source| {
        UpgradeError::io(
            path,
            std::io::Error::new(std::io::ErrorKind::InvalidData, source),
        )
    })?;
    fs::write(path, encoded).map_err(|source| UpgradeError::io(path, source))
}

fn marker_path(data_path: &str) -> String {
    format!("{}/.e2e-rollout", data_path.trim_end_matches('/'))
}

#[allow(clippy::too_many_arguments)]
async fn helm(
    runner: &dyn CommandRunner,
    manifest: &Manifest,
    kubeconfig: &Path,
    timeout: Duration,
    args: impl IntoIterator<Item = String>,
) -> Result<(), UpgradeError> {
    let spec = CommandSpec::with_program(Tool::Helm, &manifest.render.helm)
        .args(args)
        .arg("--kubeconfig")
        .arg(kubeconfig.display().to_string())
        .timeout(timeout);
    runner.run(spec).await?;
    Ok(())
}

async fn wait_for_ready_pod(
    kube: &dyn KubeApi,
    namespace: &str,
    selector: &str,
    timeout: Duration,
    clock: &dyn Clock,
) -> Result<kube::core::DynamicObject, UpgradeError> {
    let deadline = deadline_after(clock, timeout);
    loop {
        let pods = kube
            .list(POD, Some(namespace.to_owned()), Some(selector.to_owned()))
            .await?;
        if let Some(pod) = pods.iter().find(|pod| pod_ready(pod)) {
            return Ok(pod.clone());
        }
        if clock.now() >= deadline {
            return Err(UpgradeError::Check(format!(
                "no Ready pod appeared for {selector} in {namespace} within {}s",
                timeout.as_secs()
            )));
        }
        clock.sleep(Duration::from_secs(2)).await;
    }
}

async fn wait_for_replacement_pod(
    kube: &dyn KubeApi,
    namespace: &str,
    selector: &str,
    old_uid: &str,
    timeout: Duration,
    clock: &dyn Clock,
) -> Result<kube::core::DynamicObject, UpgradeError> {
    let deadline = deadline_after(clock, timeout);
    loop {
        let pods = kube
            .list(POD, Some(namespace.to_owned()), Some(selector.to_owned()))
            .await?;
        if let Some(pod) = pods.iter().find(|pod| {
            pod.metadata.uid.as_deref() != Some(old_uid)
                && pod.metadata.uid.is_some()
                && pod_ready(pod)
        }) {
            return Ok(pod.clone());
        }
        if clock.now() >= deadline {
            return Err(UpgradeError::Check(format!(
                "no replacement pod became Ready for {selector} in {namespace} within {}s",
                timeout.as_secs()
            )));
        }
        clock.sleep(Duration::from_secs(2)).await;
    }
}

fn pod_ready(pod: &kube::core::DynamicObject) -> bool {
    let status = &pod.data["status"];
    status["phase"].as_str() == Some("Running")
        && status["conditions"].as_array().is_some_and(|conditions| {
            conditions.iter().any(|condition| {
                condition["type"].as_str() == Some("Ready")
                    && condition["status"].as_str() == Some("True")
            })
        })
}

fn container_name(
    pod: &kube::core::DynamicObject,
    configured: Option<&str>,
) -> Result<String, UpgradeError> {
    let containers = pod.data["spec"]["containers"]
        .as_array()
        .ok_or_else(|| UpgradeError::Check("the pod has no containers".to_owned()))?;
    if let Some(name) = configured {
        if containers
            .iter()
            .any(|container| container["name"].as_str() == Some(name))
        {
            return Ok(name.to_owned());
        }
        return Err(UpgradeError::Check(format!(
            "container {name:?} is not in the pod"
        )));
    }
    containers
        .first()
        .and_then(|container| container["name"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| UpgradeError::Check("the pod's first container has no name".to_owned()))
}

fn check_restricted(pod: &kube::core::DynamicObject, container: &str) -> Result<(), UpgradeError> {
    let pod_spec = &pod.data["spec"];
    let pod_security = &pod_spec["securityContext"];
    let container = pod_spec["containers"]
        .as_array()
        .and_then(|containers| {
            containers
                .iter()
                .find(|candidate| candidate["name"].as_str() == Some(container))
        })
        .ok_or_else(|| UpgradeError::Check(format!("container {container:?} not found")))?;
    let container_security = &container["securityContext"];

    let mut failures = Vec::new();
    if pod_spec["automountServiceAccountToken"].as_bool() == Some(true) {
        failures.push("automountServiceAccountToken must not be true");
    }
    let run_as_non_root = pod_security["runAsNonRoot"].as_bool() == Some(true)
        || container_security["runAsNonRoot"].as_bool() == Some(true);
    if !run_as_non_root {
        failures.push("runAsNonRoot must be true");
    }
    if container_security["readOnlyRootFilesystem"].as_bool() != Some(true) {
        failures.push("readOnlyRootFilesystem must be true");
    }
    if container_security["allowPrivilegeEscalation"].as_bool() != Some(false) {
        failures.push("allowPrivilegeEscalation must be false");
    }
    let seccomp = pod_security["seccompProfile"]["type"]
        .as_str()
        .or_else(|| container_security["seccompProfile"]["type"].as_str());
    if seccomp != Some("RuntimeDefault") {
        failures.push("seccompProfile.type must be RuntimeDefault");
    }
    let drop = container_security["capabilities"]["drop"].as_array();
    if !drop.is_some_and(|drop| {
        drop.iter()
            .any(|capability| capability.as_str() == Some("ALL"))
    }) {
        failures.push("capabilities.drop must contain ALL");
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(UpgradeError::Check(format!(
            "the pod does not satisfy the restricted security profile: {}",
            failures.join("; ")
        )))
    }
}

async fn pvc_snapshot(
    kube: &dyn KubeApi,
    namespace: &str,
) -> Result<BTreeMap<String, String>, UpgradeError> {
    let claims = kube
        .list(PERSISTENT_VOLUME_CLAIM, Some(namespace.to_owned()), None)
        .await?;
    let mut snapshot = BTreeMap::new();
    for claim in claims {
        let Some(name) = claim.metadata.name.clone() else {
            continue;
        };
        let uid = claim.metadata.uid.clone().unwrap_or_default();
        let phase = claim.data["status"]["phase"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        snapshot.insert(name, format!("{uid} {phase}"));
    }
    Ok(snapshot)
}

async fn delete_namespace(
    kube: &dyn KubeApi,
    namespace: &str,
    timeout: Duration,
    clock: &dyn Clock,
) -> Result<(), UpgradeError> {
    let key = ResourceKey::cluster(NAMESPACE, namespace.to_owned());
    kube.delete(key.clone(), DeleteOptions::default()).await?;
    let deadline = deadline_after(clock, timeout);
    loop {
        if kube.get(key.clone()).await?.is_none() {
            return Ok(());
        }
        if clock.now() >= deadline {
            return Err(UpgradeError::Check(format!(
                "namespace {namespace} did not disappear within {}s",
                timeout.as_secs()
            )));
        }
        clock.sleep(Duration::from_secs(2)).await;
    }
}

fn deadline_after(clock: &dyn Clock, timeout: Duration) -> Timestamp {
    clock
        .now()
        .saturating_add(timeout)
        .unwrap_or(Timestamp::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use super::*;
    use crate::BoxFuture;
    use crate::kube::{
        ApplyOptions, DeleteOptions, ExecOutput, KubeError, LogRequest, ObjectReference,
        ProxyRequest, ResourceType,
    };
    use crate::manifest::Manifest;
    use crate::testing::FakeClock;

    const MANIFEST: &str = r#"
[project]
name = "test"
root = "."

[telemetry]
enabled = false

[registry]
enabled = false
mirrors = []

[[projects]]
name = "demo"
destinations = [{ namespace = "demo" }]

[[namespaces]]
name = "demo"

[[apps]]
name = "demo"
namespace = "demo"
project = "demo"
source = "demo"
upgrade = { from_values = ["values-upgrade-from.yaml"], container = "app", data_path = "/data", timeout_seconds = 1 }
"#;

    fn manifest() -> (tempfile::TempDir, Manifest) {
        let directory = tempfile::tempdir().expect("temp dir");
        let chart = directory.path().join("demo");
        std::fs::create_dir_all(&chart).expect("chart dir");
        std::fs::write(
            chart.join("Chart.yaml"),
            "apiVersion: v2\nname: demo\nversion: 0.1.0\n",
        )
        .expect("chart metadata");
        std::fs::write(
            chart.join("values-upgrade-from.yaml"),
            "image:\n  tag: previous\n",
        )
        .expect("baseline values");
        let manifest = Manifest::from_toml_str(MANIFEST, directory.path()).expect("manifest");
        (directory, manifest)
    }

    fn pod(uid: &str, ready: bool) -> kube::core::DynamicObject {
        let mut pod = crate::kube::object(POD, Some("demo-rollout"), "demo-rollout-0");
        pod.metadata.uid = Some(uid.to_owned());
        pod.data = serde_json::json!({
            "spec": {
                "automountServiceAccountToken": false,
                "securityContext": {
                    "runAsNonRoot": true,
                    "seccompProfile": {"type": "RuntimeDefault"},
                },
                "containers": [{
                    "name": "app",
                    "securityContext": {
                        "readOnlyRootFilesystem": true,
                        "allowPrivilegeEscalation": false,
                        "capabilities": {"drop": ["ALL"]},
                    },
                }],
            },
            "status": {
                "phase": "Running",
                "conditions": [{"type": "Ready", "status": if ready { "True" } else { "False" }}],
            },
        });
        pod
    }

    fn pvc(name: &str, uid: &str, phase: &str) -> kube::core::DynamicObject {
        let mut pvc = crate::kube::object(PERSISTENT_VOLUME_CLAIM, Some("demo-rollout"), name);
        pvc.metadata.uid = Some(uid.to_owned());
        pvc.data = serde_json::json!({"status": {"phase": phase}});
        pvc
    }

    #[derive(Debug)]
    struct FakeKube {
        state: Mutex<FakeState>,
    }

    #[derive(Debug, Default)]
    struct FakeState {
        pod_batches: VecDeque<Vec<kube::core::DynamicObject>>,
        pvc_batches: VecDeque<Vec<kube::core::DynamicObject>>,
        execs: Vec<ExecRequest>,
        marker: Option<String>,
        deletes: Vec<ResourceKey>,
    }

    impl FakeKube {
        fn new(
            pods: Vec<Vec<kube::core::DynamicObject>>,
            pvcs: Vec<Vec<kube::core::DynamicObject>>,
        ) -> Self {
            Self {
                state: Mutex::new(FakeState {
                    pod_batches: pods.into(),
                    pvc_batches: pvcs.into(),
                    ..FakeState::default()
                }),
            }
        }

        fn with_marker(self, marker: &str) -> Self {
            self.lock().marker = Some(marker.to_owned());
            self
        }

        fn execs(&self) -> Vec<ExecRequest> {
            self.lock().execs.clone()
        }

        fn deletes(&self) -> Vec<ResourceKey> {
            self.lock().deletes.clone()
        }

        fn lock(&self) -> MutexGuard<'_, FakeState> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    impl KubeApi for FakeKube {
        fn apply(
            &self,
            _key: ResourceKey,
            object: kube::core::DynamicObject,
            _options: ApplyOptions,
        ) -> BoxFuture<'_, Result<kube::core::DynamicObject, KubeError>> {
            Box::pin(async move { Ok(object) })
        }

        fn get(
            &self,
            _key: ResourceKey,
        ) -> BoxFuture<'_, Result<Option<kube::core::DynamicObject>, KubeError>> {
            Box::pin(async { Ok(None) })
        }

        fn get_object(
            &self,
            _reference: ObjectReference,
        ) -> BoxFuture<'_, Result<Option<kube::core::DynamicObject>, KubeError>> {
            Box::pin(async { Ok(None) })
        }

        fn service_proxy(
            &self,
            _request: ProxyRequest,
        ) -> BoxFuture<'_, Result<String, KubeError>> {
            Box::pin(async { Ok(String::new()) })
        }

        fn list(
            &self,
            resource: ResourceType,
            _namespace: Option<String>,
            _label_selector: Option<String>,
        ) -> BoxFuture<'_, Result<Vec<kube::core::DynamicObject>, KubeError>> {
            Box::pin(async move {
                let mut state = self.lock();
                let batch = if resource == POD {
                    state.pod_batches.pop_front()
                } else if resource == PERSISTENT_VOLUME_CLAIM {
                    state.pvc_batches.pop_front()
                } else {
                    None
                };
                Ok(batch.unwrap_or_default())
            })
        }

        fn delete(
            &self,
            key: ResourceKey,
            _options: DeleteOptions,
        ) -> BoxFuture<'_, Result<(), KubeError>> {
            Box::pin(async move {
                self.lock().deletes.push(key);
                Ok(())
            })
        }

        fn logs(&self, _request: LogRequest) -> BoxFuture<'_, Result<String, KubeError>> {
            Box::pin(async { Ok(String::new()) })
        }

        fn exec(&self, request: ExecRequest) -> BoxFuture<'_, Result<ExecOutput, KubeError>> {
            Box::pin(async move {
                let marker = self.lock().marker.clone().unwrap_or_default();
                self.lock().execs.push(request.clone());
                let stdout = match request.command.first().map(String::as_str) {
                    Some("cat") => marker,
                    _ => String::new(),
                };
                Ok(ExecOutput {
                    stdout,
                    stderr: String::new(),
                    success: true,
                })
            })
        }
    }

    fn run_upgrade(manifest: &Manifest, kube: &FakeKube) -> Result<UpgradeOutcome, UpgradeError> {
        let app = manifest.app("demo").expect("app");
        let upgrade = app.upgrade.as_ref().expect("upgrade");
        let runner: Arc<dyn CommandRunner> = Arc::new(crate::testing::FakeCommandRunner::new());
        let clock = FakeClock::default();
        futures::executor::block_on(perform_upgrade(
            manifest,
            app,
            upgrade,
            kube,
            Path::new("/tmp/kubeconfig"),
            runner,
            &clock,
            "run-1",
        ))
    }

    #[test]
    fn upgrade_checks_pod_marker_and_pvc_retention() {
        let (_directory, manifest) = manifest();
        let pvcs = vec![vec![pvc("data-demo-0", "pvc-1", "Bound")]; 3];
        let kube = FakeKube::new(
            vec![vec![pod("pod-1", true)], vec![pod("pod-2", true)]],
            pvcs,
        )
        .with_marker("e2e-rollout");

        let outcome = run_upgrade(&manifest, &kube).expect("upgrade");

        assert!(outcome.namespace.starts_with("demo-rollout-"));
        assert_eq!(outcome.pod, "demo-rollout-0");
        assert_eq!(kube.execs().len(), 2);
        assert!(
            !kube.deletes().iter().any(|key| key.resource == POD),
            "the normal upgrade must let the StatefulSet replace the pod"
        );
    }

    #[test]
    fn upgrade_fails_when_the_marker_is_lost() {
        let (_directory, manifest) = manifest();
        let pvcs = vec![vec![pvc("data-demo-0", "pvc-1", "Bound")]; 3];
        let kube = FakeKube::new(
            vec![vec![pod("pod-1", true)], vec![pod("pod-2", true)]],
            pvcs,
        )
        .with_marker("");

        let error = run_upgrade(&manifest, &kube).expect_err("marker must survive");
        assert!(error.to_string().contains("data marker"), "{error}");
    }

    #[test]
    fn upgrade_fails_when_a_pvc_changes() {
        let (_directory, manifest) = manifest();
        let pvcs = vec![
            vec![pvc("data-demo-0", "pvc-1", "Bound")],
            vec![pvc("data-demo-0", "pvc-2", "Bound")],
        ];
        let kube = FakeKube::new(
            vec![vec![pod("pod-1", true)], vec![pod("pod-2", true)]],
            pvcs,
        )
        .with_marker("e2e-rollout");

        let error = run_upgrade(&manifest, &kube).expect_err("PVC identity must not change");
        assert!(
            error.to_string().contains("PersistentVolumeClaim"),
            "{error}"
        );
    }

    #[test]
    fn upgrade_rejects_an_insecure_pod() {
        let (_directory, manifest) = manifest();
        let mut insecure = pod("pod-1", true);
        insecure.data["spec"]["containers"][0]["securityContext"]["readOnlyRootFilesystem"] =
            serde_json::Value::Bool(false);
        let pvcs = vec![vec![pvc("data-demo-0", "pvc-1", "Bound")]; 3];
        let kube = FakeKube::new(
            vec![
                vec![insecure],
                vec![pod("pod-1", true)],
                vec![pod("pod-2", true)],
            ],
            pvcs,
        )
        .with_marker("e2e-rollout");

        let error = run_upgrade(&manifest, &kube).expect_err("security profile");
        assert!(error.to_string().contains("restricted"), "{error}");
    }

    #[test]
    fn disabling_e2e_preserves_the_rest_of_the_values() {
        let values = disable_e2e(serde_json::json!({
            "e2e": {"enabled": true, "probeImage": "busybox"},
            "replicaCount": 1,
        }));
        assert_eq!(values["e2e"]["enabled"], serde_json::Value::Bool(false));
        assert_eq!(values["e2e"]["probeImage"], "busybox");
        assert_eq!(values["replicaCount"], 1);
    }
}
