//! Builders and server-side apply helpers for Argo CD resources.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::core::DynamicObject;
use serde::Serialize;

use crate::kube::{
    APP_PROJECT, APPLICATION, ApplyOptions, KubeApi, NAMESPACE, ResourceKey, ResourceType,
};
use crate::manifest::{AppSpec, Manifest, NamespaceSpec, ProjectSpec, SyncPolicySpec};
use crate::render::RenderedApp;
use crate::types::ChartVersion;

use super::{
    AppProject, AppProjectSpec, Application, ApplicationDestination, ApplicationSource,
    ApplicationSourceHelm, ApplicationSpec, ApplicationSyncPolicy, ArgoError, AutomatedSync,
    ResourceKind, SYNC_WAVE_ANNOTATION, SyncRetry, SyncRetryBackoff,
};

/// The in-cluster destination used when a manifest does not name one.
pub const DEFAULT_DESTINATION_SERVER: &str = "https://kubernetes.default.svc";

/// A chart that has been published to the local repository.
///
/// This is the join between the render milestone and Argo CD: the chart name,
/// exact chart version and merged `valuesObject` all come from one
/// [`RenderedApp`] so they cannot drift apart.
#[derive(Clone, Debug)]
pub struct PublishedChart {
    /// The repository base URL, without a trailing slash.
    pub repository_url: String,
    /// The chart name as it appears in `index.yaml`.
    pub chart_name: String,
    /// The exact chart version.
    pub version: ChartVersion,
    /// The values passed as `source.helm.valuesObject`.
    pub values: serde_json::Value,
}

impl PublishedChart {
    /// Creates a published chart reference.
    #[must_use]
    pub fn new(
        repository_url: impl Into<String>,
        chart_name: impl Into<String>,
        version: ChartVersion,
        values: serde_json::Value,
    ) -> Self {
        Self {
            repository_url: repository_url.into(),
            chart_name: chart_name.into(),
            version,
            values,
        }
    }

    /// Builds a reference from a rendered app and the served repository URL.
    #[must_use]
    pub fn from_rendered(rendered: &RenderedApp, repository_url: impl Into<String>) -> Self {
        Self {
            repository_url: repository_url.into(),
            chart_name: rendered.chart_name.clone(),
            version: rendered.chart_version.clone(),
            values: rendered.values.clone(),
        }
    }
}

impl ApplicationSpec {
    /// Converts a native app definition into an Argo CD Application spec.
    ///
    /// The source is a Helm repository source with an exact
    /// `targetRevision`; the merged values are sent as `valuesObject` so JSON
    /// booleans and numbers are preserved.
    #[must_use]
    pub fn from_app(app: &AppSpec, chart: &PublishedChart, destination_server: &str) -> Self {
        let project = app
            .project
            .as_ref()
            .map_or_else(|| app.name.to_string(), ToString::to_string);
        Self {
            project,
            source: ApplicationSource {
                repo_url: chart.repository_url.clone(),
                chart: Some(chart.chart_name.clone()),
                target_revision: Some(chart.version.to_string()),
                path: None,
                helm: Some(ApplicationSourceHelm {
                    release_name: Some(app.release_name().to_owned()),
                    value_files: Vec::new(),
                    values_object: Some(chart.values.clone()),
                    parameters: Vec::new(),
                }),
            },
            destination: ApplicationDestination {
                server: Some(destination_server.to_owned()),
                name: None,
                namespace: app.namespace.to_string(),
            },
            sync_policy: Some(ApplicationSyncPolicy::from(&app.sync_policy)),
            revision_history_limit: None,
            ignore_differences: Vec::new(),
        }
    }
}

impl From<&SyncPolicySpec> for ApplicationSyncPolicy {
    fn from(policy: &SyncPolicySpec) -> Self {
        Self {
            automated: policy.automated.then_some(AutomatedSync {
                prune: policy.prune,
                self_heal: policy.self_heal,
                allow_empty: false,
            }),
            sync_options: policy.sync_options.clone(),
            retry: Some(SyncRetry {
                limit: policy.retry_limit,
                backoff: SyncRetryBackoff {
                    duration: policy.retry_backoff.to_string(),
                    factor: 2,
                    max_duration: policy.retry_max_duration.to_string(),
                },
            }),
        }
    }
}

impl AppProjectSpec {
    /// Converts a native project definition, defaulting destinations without a
    /// server or cluster name to the in-cluster API server.
    #[must_use]
    pub fn from_project(project: &ProjectSpec) -> Self {
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
            cluster_resource_whitelist: resource_kinds(&project.cluster_resource_whitelist),
            cluster_resource_blacklist: resource_kinds(&project.cluster_resource_blacklist),
            namespace_resource_whitelist: project
                .namespace_resource_whitelist
                .as_ref()
                .map(|kinds| resource_kinds(kinds)),
            namespace_resource_blacklist: project
                .namespace_resource_blacklist
                .as_ref()
                .map(|kinds| resource_kinds(kinds)),
        }
    }

    /// Adds the in-cluster server and cluster name to destinations that have
    /// neither.
    #[must_use]
    pub fn with_destination_server(mut self, server: &str) -> Self {
        for destination in &mut self.destinations {
            if destination.server.is_none() && destination.name.is_none() {
                destination.server = Some(server.to_owned());
                destination.name = Some("in-cluster".to_owned());
            }
        }
        self
    }

    /// Adds the local chart repository to `sourceRepos` if it is not already
    /// allowed.
    #[must_use]
    pub fn with_chart_repository(mut self, repository_url: &str) -> Self {
        if !self
            .source_repos
            .iter()
            .any(|source| source == repository_url)
        {
            self.source_repos.push(repository_url.to_owned());
        }
        self
    }
}

impl From<&ProjectSpec> for AppProjectSpec {
    fn from(project: &ProjectSpec) -> Self {
        Self::from_project(project)
    }
}

fn resource_kinds(kinds: &[crate::manifest::ResourceKind]) -> Vec<ResourceKind> {
    kinds
        .iter()
        .map(|resource| ResourceKind {
            group: resource.group.clone(),
            kind: resource.kind.clone(),
        })
        .collect()
}

/// Builds a Kubernetes Namespace from a manifest entry.
#[must_use]
pub fn namespace(spec: &NamespaceSpec) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(spec.name.to_string()),
            labels: (!spec.labels.is_empty()).then(|| spec.labels.clone()),
            annotations: (!spec.annotations.is_empty()).then(|| spec.annotations.clone()),
            ..ObjectMeta::default()
        },
        ..Namespace::default()
    }
}

/// Builds an AppProject in the Argo CD namespace.
///
/// The local chart repository is always added to `sourceRepos`; the manifest's
/// declarations are preserved in order.
#[must_use]
pub fn app_project(
    project: &ProjectSpec,
    argocd_namespace: &str,
    chart_repository_url: &str,
    destination_server: &str,
) -> AppProject {
    let spec = AppProjectSpec::from_project(project)
        .with_destination_server(destination_server)
        .with_chart_repository(chart_repository_url);
    let mut resource = AppProject::new(project.name.as_str(), spec);
    resource.metadata.namespace = Some(argocd_namespace.to_owned());
    resource
}

/// Builds the Application annotations from the manifest and the dependency
/// depth.
///
/// A manifest sync-wave annotation wins over the computed wave.
#[must_use]
pub fn application_annotations(app: &AppSpec, sync_wave: i32) -> BTreeMap<String, String> {
    let mut annotations = app.annotations.clone();
    annotations
        .entry(SYNC_WAVE_ANNOTATION.to_owned())
        .or_insert_with(|| sync_wave.to_string());
    annotations
}

/// Builds an Application in the Argo CD namespace.
#[must_use]
pub fn application(
    app: &AppSpec,
    chart: &PublishedChart,
    argocd_namespace: &str,
    destination_server: &str,
    sync_wave: i32,
) -> Application {
    let spec = ApplicationSpec::from_app(app, chart, destination_server);
    let mut resource = Application::new(app.name.as_str(), spec);
    resource.metadata.namespace = Some(argocd_namespace.to_owned());
    resource.metadata.annotations = Some(application_annotations(app, sync_wave));
    resource
}

/// Builds an Application and derives its sync wave from the manifest.
///
/// # Errors
///
/// Returns [`ArgoError::Manifest`] when the app's dependency depth cannot be
/// resolved.
pub fn application_for_manifest(
    manifest: &Manifest,
    app: &AppSpec,
    chart: &PublishedChart,
    argocd_namespace: &str,
    destination_server: &str,
) -> Result<Application, ArgoError> {
    let depth = manifest.depth(&app.name)?;
    Ok(application(
        app,
        chart,
        argocd_namespace,
        destination_server,
        sync_wave_for_depth(depth),
    ))
}

/// Converts a dependency depth into a sync wave.
#[must_use]
pub fn sync_wave_for_depth(depth: u32) -> i32 {
    i32::try_from(depth).unwrap_or(i32::MAX)
}

/// Returns the sync wave of an Application, defaulting to zero.
#[must_use]
pub fn sync_wave(application: &Application) -> i32 {
    application
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(SYNC_WAVE_ANNOTATION))
        .and_then(|wave| wave.parse().ok())
        .unwrap_or(0)
}

/// Applies a Namespace with server-side apply.
///
/// # Errors
///
/// Returns [`ArgoError`] when the object cannot be encoded or applied.
pub async fn apply_namespace(
    api: &dyn KubeApi,
    spec: &NamespaceSpec,
    options: ApplyOptions,
) -> Result<DynamicObject, ArgoError> {
    apply_object(api, &namespace(spec), NAMESPACE, "Namespace", options).await
}

/// Applies an AppProject with server-side apply.
///
/// # Errors
///
/// Returns [`ArgoError`] when the object cannot be encoded or applied.
pub async fn apply_app_project(
    api: &dyn KubeApi,
    project: &AppProject,
    options: ApplyOptions,
) -> Result<DynamicObject, ArgoError> {
    apply_object(api, project, APP_PROJECT, "AppProject", options).await
}

/// Applies an Application with server-side apply.
///
/// # Errors
///
/// Returns [`ArgoError`] when the object cannot be encoded or applied.
pub async fn apply_application(
    api: &dyn KubeApi,
    application: &Application,
    options: ApplyOptions,
) -> Result<DynamicObject, ArgoError> {
    apply_object(api, application, APPLICATION, "Application", options).await
}

async fn apply_object<T: Serialize>(
    api: &dyn KubeApi,
    resource: &T,
    resource_type: ResourceType,
    kind: &'static str,
    options: ApplyOptions,
) -> Result<DynamicObject, ArgoError> {
    let value =
        serde_json::to_value(resource).map_err(|source| ArgoError::Encode { kind, source })?;
    let name = value
        .pointer("/metadata/name")
        .and_then(serde_json::Value::as_str)
        .ok_or(ArgoError::MissingName { kind })?
        .to_owned();
    let namespace = value
        .pointer("/metadata/namespace")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    if resource_type.namespaced && namespace.is_none() {
        return Err(ArgoError::MissingNamespace { kind });
    }
    let object: DynamicObject =
        serde_json::from_value(value).map_err(|source| ArgoError::Encode { kind, source })?;
    let key = ResourceKey {
        resource: resource_type,
        namespace,
        name,
    };
    Ok(api.apply(key, object, options).await?)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::kube::testing::FakeKubeApi;
    use crate::kube::{APP_PROJECT, APPLICATION, ApplyOptions, NAMESPACE};
    use crate::manifest::Manifest;
    use crate::render::testing::version;

    const MANIFEST: &str = r#"
[project]
name = "suite"
root = "."

[telemetry]
enabled = false

[registry]
enabled = false
mirrors = []

[[projects]]
name = "demo"
description = "Demo project"
source_repositories = ["*"]
destinations = [{ namespace = "demo" }]
namespace_resource_whitelist = []

[[namespaces]]
name = "demo"
labels = { "pod-security.kubernetes.io/enforce" = "restricted" }
annotations = { "example.test/owner" = "suite" }

[[apps]]
name = "demo"
namespace = "demo"
source = "demo"
inline_values = { replicaCount = 1 }
annotations = { "example.test/owner" = "suite" }
"#;

    fn fixture() -> Manifest {
        Manifest::from_toml_str(MANIFEST, Path::new(".")).expect("manifest")
    }

    fn chart() -> PublishedChart {
        PublishedChart::new(
            "http://127.0.0.1:1234",
            "demo",
            version("0.1.0"),
            json!({
                "replicaCount": 1,
                "enabled": true,
                "image": {"tag": "v1.2.3"},
            }),
        )
    }

    #[test]
    fn application_serialization_snapshot() {
        let manifest = fixture();
        let app = manifest.app("demo").expect("app");
        let application = application_for_manifest(
            &manifest,
            app,
            &chart(),
            "argocd",
            DEFAULT_DESTINATION_SERVER,
        )
        .expect("application");
        let value = serde_json::to_value(&application).expect("JSON");
        insta::assert_json_snapshot!("application_serialization", value);
        assert_eq!(sync_wave(&application), 0);
    }

    #[test]
    fn project_serialization_snapshot() {
        let manifest = fixture();
        let project = manifest.project("demo").expect("project");
        let resource = app_project(
            project,
            "argocd",
            "http://127.0.0.1:1234",
            DEFAULT_DESTINATION_SERVER,
        );
        let value = serde_json::to_value(&resource).expect("JSON");
        insta::assert_json_snapshot!("project_serialization", value);
    }

    #[test]
    fn sync_wave_comes_from_the_dependency_depth() {
        let source = r#"
[project]
name = "suite"
root = "."

[telemetry]
enabled = false

[registry]
enabled = false
mirrors = []

[[projects]]
name = "demo"
source_repositories = ["*"]
destinations = [{ namespace = "demo" }]

[[namespaces]]
name = "demo"

[[apps]]
name = "demo"
namespace = "demo"
source = "demo"

[[apps]]
name = "beta"
namespace = "demo"
project = "demo"
source = "beta"
requires = ["demo"]
"#;
        let manifest = Manifest::from_toml_str(source, Path::new(".")).expect("manifest");
        let app = manifest.app("beta").expect("app");
        let resource = application_for_manifest(
            &manifest,
            app,
            &PublishedChart::new("http://127.0.0.1:1234", "beta", version("0.1.0"), json!({})),
            "argocd",
            DEFAULT_DESTINATION_SERVER,
        )
        .expect("application");
        assert_eq!(sync_wave(&resource), 1);

        // An explicit manifest annotation wins over the computed wave.
        let mut annotated = manifest.app("beta").expect("app").clone();
        annotated
            .annotations
            .insert(SYNC_WAVE_ANNOTATION.to_owned(), "-5".to_owned());
        let annotated_resource = application(
            &annotated,
            &PublishedChart::new("http://127.0.0.1:1234", "beta", version("0.1.0"), json!({})),
            "argocd",
            DEFAULT_DESTINATION_SERVER,
            1,
        );
        assert_eq!(sync_wave(&annotated_resource), -5);
    }

    #[test]
    fn explicit_empty_namespace_whitelist_is_preserved() {
        let manifest = fixture();
        let project = manifest.project("demo").expect("project");
        let converted = AppProjectSpec::from_project(project);
        assert_eq!(converted.namespace_resource_whitelist, Some(Vec::new()));
        assert!(converted.namespace_resource_blacklist.is_none());

        let value = serde_json::to_value(&converted).expect("JSON");
        assert_eq!(value["namespaceResourceWhitelist"], json!([]));
        assert!(value.get("namespaceResourceBlacklist").is_none());
        assert_eq!(value["clusterResourceWhitelist"], json!([]));
    }

    #[test]
    fn project_destinations_default_to_the_in_cluster_server() {
        let manifest = fixture();
        let project = manifest.project("demo").expect("project");
        let converted = AppProjectSpec::from_project(project)
            .with_destination_server(DEFAULT_DESTINATION_SERVER)
            .with_chart_repository("http://127.0.0.1:1234");
        assert_eq!(
            converted.destinations[0].server.as_deref(),
            Some(DEFAULT_DESTINATION_SERVER)
        );
        assert_eq!(
            converted.destinations[0].name.as_deref(),
            Some("in-cluster")
        );
        assert_eq!(
            converted.source_repos,
            vec!["*".to_owned(), "http://127.0.0.1:1234".to_owned()]
        );

        // The chart repository is not added twice.
        let again = converted.with_chart_repository("http://127.0.0.1:1234");
        assert_eq!(
            again
                .source_repos
                .iter()
                .filter(|source| source.as_str() == "http://127.0.0.1:1234")
                .count(),
            1
        );
    }

    #[test]
    fn namespace_carries_labels_and_annotations() {
        let manifest = fixture();
        let spec = manifest.namespace("demo").expect("namespace");
        let resource = namespace(spec);
        assert_eq!(resource.metadata.name.as_deref(), Some("demo"));
        assert_eq!(
            resource
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get("pod-security.kubernetes.io/enforce"))
                .map(String::as_str),
            Some("restricted")
        );
        assert_eq!(
            resource
                .metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get("example.test/owner"))
                .map(String::as_str),
            Some("suite")
        );
    }

    #[tokio::test]
    async fn applies_typed_resources_through_the_kube_api() {
        let manifest = fixture();
        let app = manifest.app("demo").expect("app");
        let resource = application_for_manifest(
            &manifest,
            app,
            &chart(),
            "argocd",
            DEFAULT_DESTINATION_SERVER,
        )
        .expect("application");
        let api = FakeKubeApi::new();
        let options = ApplyOptions::dry_run();
        apply_application(&api, &resource, options.clone())
            .await
            .expect("applied");

        let applied = api.applied();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].key.resource, APPLICATION);
        assert_eq!(applied[0].key.namespace.as_deref(), Some("argocd"));
        assert_eq!(applied[0].key.name, "demo");
        assert!(applied[0].options.dry_run);
        assert!(applied[0].options.strict);
        assert_eq!(
            applied[0]
                .object
                .types
                .as_ref()
                .map(|types| (types.api_version.as_str(), types.kind.as_str())),
            Some(("argoproj.io/v1alpha1", "Application"))
        );

        let namespace_spec = manifest.namespace("demo").expect("namespace");
        apply_namespace(&api, namespace_spec, options.clone())
            .await
            .expect("applied namespace");
        let project = manifest.project("demo").expect("project");
        let project_resource = app_project(
            project,
            "argocd",
            "http://127.0.0.1:1234",
            DEFAULT_DESTINATION_SERVER,
        );
        apply_app_project(&api, &project_resource, options)
            .await
            .expect("applied project");

        let applied = api.applied();
        assert_eq!(applied.len(), 3);
        assert_eq!(applied[1].key.resource, NAMESPACE);
        assert_eq!(applied[1].key.namespace, None);
        assert_eq!(applied[2].key.resource, APP_PROJECT);
        assert_eq!(applied[2].key.name, "demo");
    }
}
