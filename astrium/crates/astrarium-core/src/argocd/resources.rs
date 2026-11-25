//! Typed Argo CD `v1alpha1` resources.
//!
//! The types model the subset of the Argo CD API Astrarium creates and reads.
//! Unknown fields in status objects are ignored, while manifests are built
//! from the native `astrarium.toml` types instead of untyped JSON.

use kube::CustomResource;
use serde::{Deserialize, Serialize};

/// The Argo CD API group.
pub const ARGOCD_GROUP: &str = "argoproj.io";
/// The Argo CD API version.
pub const ARGOCD_VERSION: &str = "v1alpha1";
/// The annotation that controls the Application sync wave.
pub const SYNC_WAVE_ANNOTATION: &str = "argocd.argoproj.io/sync-wave";

/// An Argo CD Application.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize)]
#[kube(
    group = "argoproj.io",
    version = "v1alpha1",
    kind = "Application",
    plural = "applications",
    namespaced,
    status = "ApplicationStatus",
    schema = "disabled"
)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSpec {
    /// The AppProject that owns this application.
    pub project: String,
    /// The chart source.
    pub source: ApplicationSource,
    /// The destination cluster and namespace.
    pub destination: ApplicationDestination,
    /// The sync policy, or `None` for manual sync.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_policy: Option<ApplicationSyncPolicy>,
    /// The number of revisions to retain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_history_limit: Option<i64>,
    /// Ignored differences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_differences: Vec<serde_json::Value>,
}

/// An Argo CD Application source.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSource {
    /// The repository URL.
    #[serde(rename = "repoURL")]
    pub repo_url: String,
    /// The chart name for a Helm repository source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chart: Option<String>,
    /// The exact chart version or Git revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_revision: Option<String>,
    /// The path within a Git repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Helm-specific source settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub helm: Option<ApplicationSourceHelm>,
}

/// Helm-specific Application source settings.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSourceHelm {
    /// The release name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_name: Option<String>,
    /// Values files, relative to the chart or repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_files: Vec<String>,
    /// Inline values, passed as `valuesObject` to preserve JSON types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values_object: Option<serde_json::Value>,
    /// Helm parameters that override values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<HelmParameter>,
}

/// A Helm `--set`-style parameter.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelmParameter {
    /// The value path.
    pub name: String,
    /// The value.
    pub value: String,
    /// Force the value to be treated as a string.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force_string: bool,
}

/// An Argo CD Application destination.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationDestination {
    /// The API server URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// The cluster name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The destination namespace.
    pub namespace: String,
}

/// An Argo CD sync policy.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSyncPolicy {
    /// Automated sync settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automated: Option<AutomatedSync>,
    /// Argo CD sync options.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync_options: Vec<String>,
    /// Retry settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<SyncRetry>,
}

/// Automated sync settings.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomatedSync {
    /// Prune resources removed from the source.
    pub prune: bool,
    /// Self-heal drift.
    pub self_heal: bool,
    /// Allow an empty application to be synced.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_empty: bool,
}

/// Retry settings for a sync policy.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRetry {
    /// The number of retries.
    pub limit: u32,
    /// The backoff configuration.
    pub backoff: SyncRetryBackoff,
}

/// Exponential retry backoff.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRetryBackoff {
    /// The initial duration, for example `5s`.
    pub duration: String,
    /// The multiplier.
    pub factor: u32,
    /// The maximum duration, for example `5m`.
    pub max_duration: String,
}

/// The Argo CD Application status fields Astrarium reads.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApplicationStatus {
    /// Health status.
    pub health: Option<HealthStatus>,
    /// Sync status.
    pub sync: Option<SyncStatus>,
    /// The last operation.
    pub operation_state: Option<OperationState>,
    /// A compact summary.
    pub summary: Option<ApplicationSummary>,
}

/// Argo CD health status.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HealthStatus {
    /// `Healthy`, `Progressing`, `Degraded`, `Suspended`, `Missing` or
    /// `Unknown`.
    pub status: Option<String>,
    /// A human-readable message.
    pub message: Option<String>,
}

/// Argo CD sync status.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncStatus {
    /// `Synced`, `OutOfSync` or `Unknown`.
    pub status: Option<String>,
    /// The revision Argo CD last synced.
    pub revision: Option<String>,
}

/// Argo CD operation state.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OperationState {
    /// `Running`, `Succeeded`, `Failed` or `Error`.
    pub phase: String,
    /// A human-readable message.
    pub message: Option<String>,
    /// The start time as an RFC 3339 string.
    pub started_at: Option<String>,
    /// The finish time as an RFC 3339 string.
    pub finished_at: Option<String>,
    /// The number of retries already attempted.
    pub retry_count: i64,
}

/// Argo CD Application summary.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApplicationSummary {
    /// The number of external URLs.
    pub external_urls: Vec<String>,
    /// The number of images.
    pub images: Vec<String>,
}

/// An Argo CD AppProject.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize)]
#[kube(
    group = "argoproj.io",
    version = "v1alpha1",
    kind = "AppProject",
    plural = "appprojects",
    namespaced,
    schema = "disabled"
)]
#[serde(rename_all = "camelCase")]
pub struct AppProjectSpec {
    /// A human-readable description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Allowed source repositories.
    #[serde(default)]
    pub source_repos: Vec<String>,
    /// Allowed destinations.
    #[serde(default)]
    pub destinations: Vec<ApplicationDestination>,
    /// Allowed cluster-scoped resources. An empty list denies every
    /// cluster-scoped kind.
    #[serde(default)]
    pub cluster_resource_whitelist: Vec<ResourceKind>,
    /// Denied cluster-scoped resources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cluster_resource_blacklist: Vec<ResourceKind>,
    /// Allowed namespaced resources. `Some(vec![])` is an explicit empty
    /// allow-list and must serialize as `[]`; `None` leaves Argo CD's default
    /// in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace_resource_whitelist: Option<Vec<ResourceKind>>,
    /// Denied namespaced resources. `Some(vec![])` is explicit and serializes
    /// as `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace_resource_blacklist: Option<Vec<ResourceKind>>,
}

/// A Kubernetes group/kind pair in an AppProject policy.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceKind {
    /// The API group, or the empty string for the core group.
    #[serde(default)]
    pub group: String,
    /// The kind.
    pub kind: String,
}
