//! Typed Argo CD resources.
//!
//! Astrarium creates `Namespace`, `AppProject` and `Application` objects
//! directly instead of bootstrapping the production `apps` chart. The
//! Application and AppProject types below model the Argo CD `v1alpha1` API
//! surface Astrarium uses; status fields are typed so tests and diagnostics do
//! not have to index into untyped JSON.

use kube::CustomResource;
use serde::{Deserialize, Serialize};

/// The Argo CD API group.
pub const ARGOCD_GROUP: &str = "argoproj.io";
/// The Argo CD API version.
pub const ARGOCD_VERSION: &str = "v1alpha1";

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
    /// The chart or Git source.
    pub source: ApplicationSource,
    /// The destination cluster and namespace.
    pub destination: ApplicationDestination,
    /// The sync policy, or `None` for manual sync.
    pub sync_policy: Option<ApplicationSyncPolicy>,
    /// The number of revisions to retain.
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
    pub chart: Option<String>,
    /// The chart version or Git revision.
    pub target_revision: Option<String>,
    /// The path within a Git repository.
    pub path: Option<String>,
    /// Helm-specific source settings.
    pub helm: Option<ApplicationSourceHelm>,
}

/// Helm-specific Application source settings.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSourceHelm {
    /// The release name.
    pub release_name: Option<String>,
    /// Values files, relative to the chart or repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_files: Vec<String>,
    /// Inline values, passed as `valuesObject` to preserve types.
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
    pub server: Option<String>,
    /// The cluster name.
    pub name: Option<String>,
    /// The destination namespace.
    pub namespace: String,
}

/// An Argo CD sync policy.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSyncPolicy {
    /// Automated sync settings.
    pub automated: Option<AutomatedSync>,
    /// Argo CD sync options.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync_options: Vec<String>,
    /// Retry settings.
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
    /// `Healthy`, `Progressing`, `Degraded`, `Suspended`, `Missing` or `Unknown`.
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
    pub description: Option<String>,
    /// Allowed source repositories.
    #[serde(default)]
    pub source_repos: Vec<String>,
    /// Allowed destinations.
    #[serde(default)]
    pub destinations: Vec<ApplicationDestination>,
    /// Allowed cluster-scoped resources.
    #[serde(default)]
    pub cluster_resource_whitelist: Vec<ResourceKind>,
    /// Allowed namespaced resources. Omitted means allow all.
    pub namespace_resource_whitelist: Option<Vec<ResourceKind>>,
    /// Denied namespaced resources.
    pub namespace_resource_blacklist: Option<Vec<ResourceKind>>,
}

/// A Kubernetes group/kind pair in an AppProject policy.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceKind {
    /// The API group, or the empty string for the core group.
    #[serde(default)]
    pub group: String,
    /// The kind.
    pub kind: String,
}

impl From<&crate::manifest::ProjectSpec> for AppProjectSpec {
    fn from(project: &crate::manifest::ProjectSpec) -> Self {
        Self {
            description: project.description.clone(),
            source_repos: project.source_repositories.clone(),
            destinations: project
                .destinations
                .iter()
                .map(|destination| ApplicationDestination {
                    server: destination.server.clone(),
                    name: destination.name.clone(),
                    namespace: destination.namespace.clone(),
                })
                .collect(),
            cluster_resource_whitelist: project
                .cluster_resource_whitelist
                .iter()
                .map(|resource| ResourceKind {
                    group: resource.group.clone(),
                    kind: resource.kind.clone(),
                })
                .collect(),
            namespace_resource_whitelist: project.namespace_resource_whitelist.as_ref().map(
                |resources| {
                    resources
                        .iter()
                        .map(|resource| ResourceKind {
                            group: resource.group.clone(),
                            kind: resource.kind.clone(),
                        })
                        .collect()
                },
            ),
            namespace_resource_blacklist: project.namespace_resource_blacklist.as_ref().map(
                |resources| {
                    resources
                        .iter()
                        .map(|resource| ResourceKind {
                            group: resource.group.clone(),
                            kind: resource.kind.clone(),
                        })
                        .collect()
                },
            ),
        }
    }
}

/// Builds an AppProject in the Argo CD namespace.
#[must_use]
pub fn app_project(project: &crate::manifest::ProjectSpec, namespace: &str) -> AppProject {
    let mut resource = AppProject::new(project.name.as_str(), AppProjectSpec::from(project));
    resource.metadata.namespace = Some(namespace.to_owned());
    resource
}

/// Builds an Application in the Argo CD namespace.
#[must_use]
pub fn application(
    name: &str,
    namespace: &str,
    spec: ApplicationSpec,
    sync_wave: i32,
) -> Application {
    let mut resource = Application::new(name, spec);
    resource.metadata.namespace = Some(namespace.to_owned());
    resource.metadata.annotations = Some(std::collections::BTreeMap::from([(
        "argocd.argoproj.io/sync-wave".to_owned(),
        sync_wave.to_string(),
    )]));
    resource
}

/// Returns the sync wave of an Application, defaulting to zero.
#[must_use]
pub fn sync_wave(application: &Application) -> i32 {
    application
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("argocd.argoproj.io/sync-wave"))
        .and_then(|wave| wave.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn application_serializes_camel_case_values_object() {
        let spec = ApplicationSpec {
            project: "e2e".to_owned(),
            source: ApplicationSource {
                repo_url: "http://host:1234".to_owned(),
                chart: Some("mosquitto".to_owned()),
                target_revision: Some("0.1.0".to_owned()),
                helm: Some(ApplicationSourceHelm {
                    values_object: Some(json!({"replicaCount": 1})),
                    ..ApplicationSourceHelm::default()
                }),
                ..ApplicationSource::default()
            },
            destination: ApplicationDestination {
                server: Some("https://kubernetes.default.svc".to_owned()),
                namespace: "mosquitto".to_owned(),
                ..ApplicationDestination::default()
            },
            sync_policy: Some(ApplicationSyncPolicy {
                automated: Some(AutomatedSync {
                    prune: true,
                    self_heal: true,
                    allow_empty: false,
                }),
                sync_options: vec!["ServerSideApply=true".to_owned()],
                retry: None,
            }),
            revision_history_limit: None,
            ignore_differences: Vec::new(),
        };

        let application = application("mosquitto", "argocd", spec, -10);
        let value = serde_json::to_value(&application).unwrap();
        assert_eq!(value["spec"]["source"]["repoURL"], "http://host:1234");
        assert_eq!(
            value["spec"]["source"]["helm"]["valuesObject"]["replicaCount"],
            1
        );
        assert_eq!(
            value["metadata"]["annotations"]["argocd.argoproj.io/sync-wave"],
            "-10"
        );
        assert_eq!(sync_wave(&application), -10);
    }

    #[test]
    fn project_conversion_preserves_explicit_whitelists() {
        let project = crate::manifest::ProjectSpec {
            name: crate::ProjectName::new("alpha").unwrap(),
            description: None,
            source_repositories: vec!["*".to_owned()],
            destinations: vec![crate::manifest::ProjectDestination {
                server: None,
                name: None,
                namespace: "alpha".to_owned(),
            }],
            cluster_resource_whitelist: Vec::new(),
            cluster_resource_blacklist: Vec::new(),
            namespace_resource_whitelist: Some(Vec::new()),
            namespace_resource_blacklist: None,
        };
        let converted = AppProjectSpec::from(&project);
        assert!(converted.namespace_resource_whitelist.is_some());
        assert!(converted.namespace_resource_blacklist.is_none());
    }
}
