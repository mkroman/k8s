//! The native `astrarium.toml` manifest.
//!
//! The manifest is the single source of truth for an Astrarium suite. It
//! describes the namespaces and Argo CD projects Astrarium creates, the
//! applications it renders and deploys, and the cluster and telemetry settings
//! used for a run. It is deliberately independent of the production
//! app-of-apps catalogue: there is no runtime adapter that reads
//! `apps/values.yaml` or `hack/e2e/apps.yaml`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::{AppName, HumanDuration, NamespaceName, ProjectName, Revision, ValueError};

/// An error while reading, parsing or validating an Astrarium manifest.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// The manifest could not be read.
    #[error("failed to read manifest {path}: {source}")]
    Read {
        /// The manifest path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The manifest is not valid TOML or does not match the schema.
    #[error("failed to parse manifest {path}: {source}")]
    Parse {
        /// The manifest path.
        path: PathBuf,
        /// The TOML parser error.
        #[source]
        source: toml::de::Error,
    },
    /// The manifest parsed but is semantically invalid.
    #[error("invalid manifest: {message}")]
    Invalid {
        /// A human-readable explanation.
        message: String,
    },
    /// A name is not a valid Kubernetes name.
    #[error(transparent)]
    Name(#[from] ValueError),
    /// A name is declared more than once.
    #[error("duplicate {kind} {name:?}")]
    Duplicate {
        /// The kind of declaration, for example `app`.
        kind: &'static str,
        /// The duplicated name.
        name: String,
    },
    /// A reference points at a declaration that does not exist.
    #[error("unknown {kind} {name:?} referenced by {context}")]
    Unknown {
        /// The kind of declaration, for example `project`.
        kind: &'static str,
        /// The missing name.
        name: String,
        /// The declaration that contains the reference.
        context: String,
    },
    /// The app dependency graph contains a cycle.
    #[error("dependency cycle: {path}")]
    Cycle {
        /// The cycle, rendered as `a -> b -> a`.
        path: String,
    },
    /// A path escapes the project root.
    #[error("path {path:?} escapes the project root")]
    PathEscape {
        /// The offending path.
        path: PathBuf,
    },
    /// A file or directory required by the manifest does not exist.
    #[error("required path {path:?} does not exist ({context})")]
    MissingPath {
        /// The missing path.
        path: PathBuf,
        /// The declaration that requires it.
        context: String,
    },
}

impl ManifestError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

/// The top-level `astrarium.toml` document.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Suite-wide settings.
    pub project: ProjectConfig,
    /// The kind cluster used for this run.
    #[serde(default)]
    pub cluster: ClusterConfig,
    /// Argo CD installation and project settings.
    #[serde(default)]
    pub argocd: ArgoCdConfig,
    /// Renderer and external-tool settings.
    #[serde(default)]
    pub render: RenderConfig,
    /// OpenTelemetry export settings.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// Pull-through registry mirrors for kind nodes.
    #[serde(default)]
    pub registry: RegistryConfig,
    /// Helm repositories that must be registered before dependency builds.
    #[serde(default)]
    pub helm_repositories: Vec<HelmRepository>,
    /// Argo CD projects created before applications.
    #[serde(default)]
    pub projects: Vec<ProjectSpec>,
    /// Kubernetes namespaces created before applications.
    #[serde(default)]
    pub namespaces: Vec<NamespaceSpec>,
    /// Applications in the suite.
    #[serde(default)]
    pub apps: Vec<AppSpec>,
    /// The path the manifest was loaded from, if any.
    #[serde(skip)]
    #[schemars(skip)]
    pub path: Option<PathBuf>,
}

/// Suite-wide configuration.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    /// A human-readable suite name.
    pub name: String,
    /// The repository root, relative to the manifest file.
    #[serde(default = "default_root")]
    pub root: PathBuf,
    /// The branch that change selection compares against.
    #[serde(default = "default_main")]
    pub base_branch: String,
    /// Repository-relative paths that select every app when they change.
    ///
    /// The manifest itself, the kind configuration and the Argo CD install
    /// directory are always treated as shared. Use this list for anything else,
    /// such as a shared values file or the orchestrator's own source tree.
    #[serde(default)]
    pub shared_paths: Vec<PathBuf>,
}

/// The cluster provider.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClusterProvider {
    /// Create a disposable kind cluster.
    #[default]
    Kind,
    /// Use an existing cluster from the current kubeconfig.
    Existing,
}

/// kind cluster configuration.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClusterConfig {
    /// The cluster provider.
    #[serde(default)]
    pub provider: ClusterProvider,
    /// The cluster name. Astrarium only deletes clusters with this name or the
    /// `astrarium-` prefix.
    #[serde(default = "default_cluster_name")]
    pub name: String,
    /// The kind configuration file, relative to the repository root.
    pub kind_config: Option<PathBuf>,
    /// Where to write the cluster kubeconfig. Relative paths are resolved
    /// against the repository root.
    #[serde(default = "default_kubeconfig")]
    pub kubeconfig: PathBuf,
    /// Reuse an existing cluster with the configured name.
    #[serde(default)]
    pub reuse: bool,
    /// Keep the cluster after a successful run.
    #[serde(default)]
    pub keep: bool,
    /// Override the kind node image.
    pub node_image: Option<String>,
}

/// Argo CD installation and project settings.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArgoCdConfig {
    /// The Kustomize directory that installs the Argo CD controller and repo
    /// server.
    #[serde(default = "default_argocd_install")]
    pub install: PathBuf,
    /// The namespace Argo CD runs in.
    #[serde(default = "default_argocd_namespace")]
    pub namespace: NamespaceName,
    /// The AppProject that owns the child Applications.
    #[serde(default = "default_argocd_project")]
    pub project: String,
    /// A stable name used for the local chart repository and Argo CD labels.
    #[serde(default = "default_repo_name")]
    pub repo_name: String,
    /// The in-cluster Kubernetes API server.
    #[serde(default = "default_argocd_server")]
    pub server: String,
    /// How long to wait for the Argo CD control plane.
    #[serde(default = "default_argocd_wait")]
    pub wait_timeout: HumanDuration,
}

/// Renderer and external-tool settings.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderConfig {
    /// The `helm` executable.
    #[serde(default = "default_helm")]
    pub helm: String,
    /// The `kustomize` executable.
    #[serde(default = "default_kustomize")]
    pub kustomize: String,
    /// The `kind` executable.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// The namespace used for `helm template` rendering.
    #[serde(default = "default_render_namespace")]
    pub namespace: NamespaceName,
    /// The timeout for a single render or package operation.
    #[serde(default = "default_render_timeout")]
    pub timeout: HumanDuration,
    /// Run `helm template` or `kustomize build` and validate the output with a
    /// server-side dry run before Argo CD syncs.
    #[serde(default = "default_true")]
    pub dry_run: bool,
}

/// OpenTelemetry export settings.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    /// Export traces to OTLP.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The OTLP/gRPC endpoint.
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,
    /// The `service.name` resource attribute.
    #[serde(default = "default_service_name")]
    pub service_name: String,
    /// The `deployment.environment.name` resource attribute.
    pub environment: Option<String>,
}

/// Registry mirror configuration.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryConfig {
    /// Mount containerd mirror configuration into kind nodes.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The host address reachable from kind nodes, or `auto`.
    #[serde(default = "default_registry_host")]
    pub host: String,
    /// One pull-through cache per upstream registry.
    #[serde(default = "default_registry_mirrors")]
    pub mirrors: Vec<RegistryMirror>,
}

/// A pull-through cache for one upstream registry.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryMirror {
    /// The upstream registry host, for example `docker.io`.
    pub upstream: String,
    /// The mirror endpoint. `{host}` is replaced with the resolved host address.
    pub endpoint: String,
}

/// A Helm chart repository that must be registered for dependency builds.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HelmRepository {
    /// The local Helm repository name.
    pub name: String,
    /// The repository URL.
    pub url: String,
}

/// An Argo CD AppProject.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectSpec {
    /// The project name.
    pub name: ProjectName,
    /// A human-readable description.
    pub description: Option<String>,
    /// Allowed source repositories. Defaults to every repository. Astrarium
    /// appends its local chart repository unless the list contains `"*"`.
    #[serde(default = "default_source_repositories")]
    pub source_repositories: Vec<String>,
    /// Allowed destinations.
    #[serde(default)]
    pub destinations: Vec<ProjectDestination>,
    /// Cluster-scoped resources the project may manage. Empty denies all.
    #[serde(default)]
    pub cluster_resource_whitelist: Vec<ResourceKind>,
    /// Cluster-scoped resources the project may not manage.
    #[serde(default)]
    pub cluster_resource_blacklist: Vec<ResourceKind>,
    /// Namespaced resources the project may manage. When omitted, Argo CD's
    /// default (allow all namespaced resources) applies.
    pub namespace_resource_whitelist: Option<Vec<ResourceKind>>,
    /// Namespaced resources the project may not manage.
    pub namespace_resource_blacklist: Option<Vec<ResourceKind>>,
}

/// An Argo CD project destination.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectDestination {
    /// The API server URL. Defaults to the in-cluster server.
    pub server: Option<String>,
    /// The cluster name. Mutually exclusive with `server`.
    pub name: Option<String>,
    /// The destination namespace, or `*`.
    pub namespace: String,
}

/// A Kubernetes group/kind pair.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceKind {
    /// The API group. The core group is the empty string.
    #[serde(default)]
    pub group: String,
    /// The kind.
    pub kind: String,
}

/// A Kubernetes namespace created before applications.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NamespaceSpec {
    /// The namespace name.
    pub name: NamespaceName,
    /// Labels applied with server-side apply.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Annotations applied with server-side apply.
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
}

/// An application in the suite.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    /// The application name.
    pub name: AppName,
    /// The destination namespace.
    pub namespace: NamespaceName,
    /// The owning AppProject. Defaults to the app name.
    pub project: Option<ProjectName>,
    /// The chart or Kustomize directory, relative to the repository root.
    pub source: PathBuf,
    /// Extra repository-relative paths that select this app when they change.
    ///
    /// Use this when `source` is a narrower directory than the files it
    /// consumes, for example a Kustomize overlay below a larger base tree.
    #[serde(default)]
    pub watch: Vec<PathBuf>,
    /// How the source is rendered.
    #[serde(default)]
    pub renderer: RendererKind,
    /// Helm values files, relative to `source` and merged in listed order
    /// after `common_values`.
    #[serde(default)]
    pub values: Vec<PathBuf>,
    /// A values file shared by several apps, merged before `values`.
    pub common_values: Option<PathBuf>,
    /// Inline Helm values, merged after the values files.
    #[serde(default)]
    pub inline_values: BTreeMap<String, serde_json::Value>,
    /// Apps that must be healthy first.
    #[serde(default)]
    pub requires: Vec<AppName>,
    /// The app carries a PostSync probe in its chart.
    #[serde(default)]
    pub probe: bool,
    /// Optional upgrade test definition.
    pub upgrade: Option<UpgradeSpec>,
    /// The app's probe needs an image that is absent from every node.
    #[serde(default)]
    pub requires_fresh_image: bool,
    /// The app's probe requires the kind containerd configuration, including
    /// the mirror `config_path` and isolated content sharing.
    #[serde(default)]
    pub requires_containerd_config: bool,
    /// Resources that must be removed before a cold-image redeploy.
    pub stale_probe_resources: Option<StaleProbeResources>,
    /// Objects that must exist after the app is healthy.
    #[serde(default)]
    pub resources: Vec<ResourceRef>,
    /// Prometheus targets that must all be up after the app is healthy.
    pub prometheus_targets: Option<PrometheusTargets>,
    /// Override the per-app wait timeout in seconds.
    pub timeout_seconds: Option<u64>,
    /// Argo CD sync policy.
    #[serde(default)]
    pub sync_policy: SyncPolicySpec,
    /// Extra Application annotations, for example a custom sync wave.
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    /// Helm-specific options.
    #[serde(default)]
    pub helm: HelmOptions,
    /// Kustomize-specific options.
    #[serde(default)]
    pub kustomize: KustomizeOptions,
}

/// How an application source is rendered.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RendererKind {
    /// Render a Helm chart.
    #[default]
    Helm,
    /// Render a Kustomize directory and wrap it in a chart.
    Kustomize,
}

/// An optional upgrade test.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpgradeSpec {
    /// Values used for the initial deployment, relative to the app's `source`
    /// and applied before the current values.
    #[serde(default)]
    pub from_values: Vec<PathBuf>,
    /// A Git revision to deploy before the current working tree.
    pub from_revision: Option<Revision>,
}

/// Resources that must be removed before a cold-image probe can run.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StaleProbeResources {
    /// The namespace that contains the resources.
    pub namespace: NamespaceName,
    /// Resources such as `daemonset/spegel-cleanup`.
    #[serde(default)]
    pub resource_names: Vec<String>,
    /// Pod label selectors to delete.
    #[serde(default)]
    pub pod_label_selectors: Vec<String>,
}

/// A concrete Kubernetes object that must exist after an app is healthy.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceRef {
    /// The API version, for example `apps/v1` or `v1`.
    pub api_version: String,
    /// The kind.
    pub kind: String,
    /// The namespace, or `None` for cluster-scoped objects.
    pub namespace: Option<NamespaceName>,
    /// The object name.
    pub name: String,
}

/// Prometheus targets that must be up after an app is healthy.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrometheusTargets {
    /// The namespace of the Prometheus service.
    pub namespace: NamespaceName,
    /// The Prometheus service name.
    pub service: String,
    /// The Prometheus HTTP port.
    pub port: u16,
    /// The `job` label values that must all have an `up` target.
    pub jobs: Vec<String>,
}

/// Argo CD sync policy settings.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncPolicySpec {
    /// Enable automated sync.
    #[serde(default = "default_true")]
    pub automated: bool,
    /// Prune resources removed from the source.
    #[serde(default = "default_true")]
    pub prune: bool,
    /// Self-heal drift.
    #[serde(default = "default_true")]
    pub self_heal: bool,
    /// Argo CD sync options such as `ServerSideApply=true`.
    #[serde(default)]
    pub sync_options: Vec<String>,
    /// The number of sync retries.
    #[serde(default = "default_retry_limit")]
    pub retry_limit: u32,
    /// The initial retry backoff.
    #[serde(default = "default_retry_backoff")]
    pub retry_backoff: HumanDuration,
    /// The maximum retry backoff.
    #[serde(default = "default_retry_max_duration")]
    pub retry_max_duration: HumanDuration,
}

impl Default for SyncPolicySpec {
    fn default() -> Self {
        Self {
            automated: true,
            prune: true,
            self_heal: true,
            sync_options: Vec::new(),
            retry_limit: default_retry_limit(),
            retry_backoff: default_retry_backoff(),
            retry_max_duration: default_retry_max_duration(),
        }
    }
}

/// Helm-specific application options.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HelmOptions {
    /// The Helm release name. Defaults to the app name.
    pub release_name: Option<String>,
}

/// Kustomize-specific application options.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KustomizeOptions {
    /// Extra arguments passed to `kustomize build`.
    #[serde(default)]
    pub build_args: Vec<String>,
}

impl Manifest {
    /// Loads and structurally validates a manifest from disk.
    ///
    /// Relative paths are resolved against the manifest's directory.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when the file cannot be read, parsed or
    /// validated.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut manifest: Self =
            toml::from_str(&source).map_err(|source| ManifestError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        manifest.path = Some(path.to_path_buf());
        manifest.absolute_root(base)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Parses and validates a manifest from a string.
    ///
    /// `base_dir` is used as the directory containing the manifest.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when the TOML cannot be parsed or the manifest
    /// is invalid.
    pub fn from_toml_str(source: &str, base_dir: &Path) -> Result<Self, ManifestError> {
        let mut manifest: Self = toml::from_str(source).map_err(|source| ManifestError::Parse {
            path: PathBuf::from("<memory>"),
            source,
        })?;
        manifest.absolute_root(base_dir)?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn absolute_root(&mut self, base_dir: &Path) -> Result<(), ManifestError> {
        let base = if base_dir.is_absolute() {
            base_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|source| ManifestError::Read {
                    path: base_dir.to_path_buf(),
                    source,
                })?
                .join(base_dir)
        };
        self.project.root = normalize_lexically(&base.join(&self.project.root));
        Ok(())
    }

    /// Returns the app with the given name.
    #[must_use]
    pub fn app(&self, name: &str) -> Option<&AppSpec> {
        self.apps.iter().find(|app| app.name.as_str() == name)
    }

    /// Returns the project with the given name.
    #[must_use]
    pub fn project(&self, name: &str) -> Option<&ProjectSpec> {
        self.projects
            .iter()
            .find(|project| project.name.as_str() == name)
    }

    /// Returns the namespace with the given name.
    #[must_use]
    pub fn namespace(&self, name: &str) -> Option<&NamespaceSpec> {
        self.namespaces
            .iter()
            .find(|namespace| namespace.name.as_str() == name)
    }

    /// Resolves a repository-relative path and rejects escapes.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::PathEscape`] when the path is absolute or
    /// contains too many parent components.
    pub fn resolve_path(&self, path: &Path) -> Result<PathBuf, ManifestError> {
        if path.is_absolute() {
            return Err(ManifestError::PathEscape {
                path: path.to_path_buf(),
            });
        }
        let joined = normalize_lexically(&self.project.root.join(path));
        if !joined.starts_with(&self.project.root) {
            return Err(ManifestError::PathEscape {
                path: path.to_path_buf(),
            });
        }
        Ok(joined)
    }

    /// Resolves a path that may be absolute, such as a generated kubeconfig.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::PathEscape`] when a relative path escapes the
    /// project root.
    pub fn resolve_output_path(&self, path: &Path) -> Result<PathBuf, ManifestError> {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            self.resolve_path(path)
        }
    }

    /// Validates names, references, dependency cycles and registry settings.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] for the first validation failure.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.project.name.trim().is_empty() {
            return Err(ManifestError::invalid("project.name must not be empty"));
        }
        if self.project.base_branch.trim().is_empty() {
            return Err(ManifestError::invalid(
                "project.base_branch must not be empty",
            ));
        }
        for shared in &self.project.shared_paths {
            if shared.as_os_str().is_empty() {
                return Err(ManifestError::invalid(
                    "project.shared_paths entries must not be empty",
                ));
            }
            self.resolve_path(shared)?;
        }

        let mut project_names = HashSet::new();
        for project in &self.projects {
            if !project_names.insert(project.name.as_str()) {
                return Err(ManifestError::Duplicate {
                    kind: "project",
                    name: project.name.to_string(),
                });
            }
            for destination in &project.destinations {
                if destination.server.is_some() && destination.name.is_some() {
                    return Err(ManifestError::invalid(format!(
                        "project {} destination sets both server and name",
                        project.name
                    )));
                }
            }
        }

        let mut namespace_names = HashSet::new();
        for namespace in &self.namespaces {
            if !namespace_names.insert(namespace.name.as_str()) {
                return Err(ManifestError::Duplicate {
                    kind: "namespace",
                    name: namespace.name.to_string(),
                });
            }
        }

        let mut app_names = HashSet::new();
        for app in &self.apps {
            if !app_names.insert(app.name.as_str()) {
                return Err(ManifestError::Duplicate {
                    kind: "app",
                    name: app.name.to_string(),
                });
            }

            self.resolve_path(&app.source)?;
            for watch in &app.watch {
                if watch.as_os_str().is_empty() {
                    return Err(ManifestError::invalid(format!(
                        "app {} watch entries must not be empty",
                        app.name
                    )));
                }
                self.resolve_path(watch)?;
            }
            for values in &app.values {
                self.resolve_path(&app.source.join(values))?;
            }
            if let Some(common) = &app.common_values {
                self.resolve_path(common)?;
            }
            if let Some(upgrade) = &app.upgrade {
                for values in &upgrade.from_values {
                    self.resolve_path(&app.source.join(values))?;
                }
            }
            for requirement in &app.requires {
                if !app_names.contains(requirement.as_str())
                    && !self
                        .apps
                        .iter()
                        .any(|candidate| candidate.name == *requirement)
                {
                    return Err(ManifestError::Unknown {
                        kind: "app",
                        name: requirement.to_string(),
                        context: format!("app {}", app.name),
                    });
                }
            }

            let project = app.project_name()?;
            if !project_names.contains(project.as_str()) {
                return Err(ManifestError::Unknown {
                    kind: "project",
                    name: project.to_string(),
                    context: format!("app {}", app.name),
                });
            }
            if !namespace_names.contains(app.namespace.as_str()) {
                return Err(ManifestError::Unknown {
                    kind: "namespace",
                    name: app.namespace.to_string(),
                    context: format!("app {}", app.name),
                });
            }

            if app.requires_fresh_image && app.stale_probe_resources.is_none() {
                // This is allowed: a cold-image app may have no stale resources.
            }
            if app.stale_probe_resources.as_ref().is_some_and(|stale| {
                stale.resource_names.is_empty() && stale.pod_label_selectors.is_empty()
            }) {
                return Err(ManifestError::invalid(format!(
                    "app {} stale_probe_resources must list resource_names or pod_label_selectors",
                    app.name
                )));
            }
        }

        // Every requires edge must point at an app declared before validation
        // completes; the duplicate check above also catches duplicate targets.
        let all: Vec<AppName> = self.apps.iter().map(|app| app.name.clone()).collect();
        self.dependency_order(&all)?;

        if self.registry.enabled {
            for mirror in &self.registry.mirrors {
                if mirror.upstream.trim().is_empty() {
                    return Err(ManifestError::invalid(
                        "registry mirror upstream must not be empty",
                    ));
                }
                if !mirror.endpoint.contains("{host}") {
                    return Err(ManifestError::invalid(format!(
                        "registry mirror for {} must contain {{host}} in its endpoint",
                        mirror.upstream
                    )));
                }
            }
        }

        if self.telemetry.enabled {
            reqwest::Url::parse(&self.telemetry.otlp_endpoint).map_err(|error| {
                ManifestError::invalid(format!(
                    "telemetry.otlp_endpoint {:?} is not a URL: {error}",
                    self.telemetry.otlp_endpoint
                ))
            })?;
        }

        for repository in &self.helm_repositories {
            if repository.name.trim().is_empty() || repository.url.trim().is_empty() {
                return Err(ManifestError::invalid(
                    "helm_repositories entries need a name and url",
                ));
            }
        }

        Ok(())
    }

    /// Checks that paths referenced by the manifest exist on disk.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::MissingPath`] for the first missing path.
    pub fn validate_files(&self) -> Result<(), ManifestError> {
        if let Some(kind_config) = &self.cluster.kind_config {
            self.require_path(kind_config, "cluster.kind_config")?;
        }
        self.require_path(&self.argocd.install, "argocd.install")?;

        for app in &self.apps {
            self.require_path(&app.source, &format!("app {} source", app.name))?;
            if app.renderer == RendererKind::Helm {
                let chart = self.resolve_path(&app.source.join("Chart.yaml"))?;
                if !chart.is_file() {
                    return Err(ManifestError::MissingPath {
                        path: chart,
                        context: format!("app {} Helm chart", app.name),
                    });
                }
            }
            for values in &app.values {
                self.require_path(
                    &app.source.join(values),
                    &format!("app {} values", app.name),
                )?;
            }
            if let Some(common) = &app.common_values {
                self.require_path(common, &format!("app {} common_values", app.name))?;
            }
            if let Some(upgrade) = &app.upgrade {
                for values in &upgrade.from_values {
                    self.require_path(
                        &app.source.join(values),
                        &format!("app {} upgrade values", app.name),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn require_path(&self, path: &Path, context: &str) -> Result<(), ManifestError> {
        let resolved = self.resolve_path(path)?;
        if resolved.exists() {
            Ok(())
        } else {
            Err(ManifestError::MissingPath {
                path: resolved,
                context: context.to_owned(),
            })
        }
    }

    /// Returns the selected apps and their transitive prerequisites in
    /// dependency order (prerequisites first).
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Unknown`] for an undeclared app and
    /// [`ManifestError::Cycle`] for a cycle.
    pub fn dependency_order(&self, selected: &[AppName]) -> Result<Vec<AppName>, ManifestError> {
        let mut order = Vec::new();
        let mut state = HashMap::<String, VisitState>::new();
        let mut stack = Vec::new();

        for name in selected {
            self.visit(name, &mut state, &mut stack, &mut order)?;
        }
        Ok(order)
    }

    fn visit(
        &self,
        name: &AppName,
        state: &mut HashMap<String, VisitState>,
        stack: &mut Vec<String>,
        order: &mut Vec<AppName>,
    ) -> Result<(), ManifestError> {
        match state.get(name.as_str()) {
            Some(VisitState::Done) => return Ok(()),
            Some(VisitState::Visiting) => {
                let cycle_start = stack
                    .iter()
                    .position(|entry| entry == name.as_str())
                    .unwrap_or(0);
                let mut cycle = stack[cycle_start..].to_vec();
                cycle.push(name.to_string());
                return Err(ManifestError::Cycle {
                    path: cycle.join(" -> "),
                });
            }
            None => {}
        }

        let app = self
            .app(name.as_str())
            .ok_or_else(|| ManifestError::Unknown {
                kind: "app",
                name: name.to_string(),
                context: "dependency graph".to_owned(),
            })?;

        state.insert(name.to_string(), VisitState::Visiting);
        stack.push(name.to_string());
        for requirement in &app.requires {
            self.visit(requirement, state, stack, order)?;
        }
        stack.pop();
        state.insert(name.to_string(), VisitState::Done);
        order.push(name.clone());
        Ok(())
    }

    /// Returns the dependency depth of an app, used to derive sync waves.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when the app or one of its dependencies is
    /// unknown or part of a cycle.
    pub fn depth(&self, name: &AppName) -> Result<u32, ManifestError> {
        let mut memo = HashMap::new();
        self.depth_inner(name, &mut memo, &mut Vec::new())
    }

    fn depth_inner(
        &self,
        name: &AppName,
        memo: &mut HashMap<String, u32>,
        stack: &mut Vec<String>,
    ) -> Result<u32, ManifestError> {
        if let Some(depth) = memo.get(name.as_str()) {
            return Ok(*depth);
        }
        if stack.iter().any(|entry| entry == name.as_str()) {
            let mut cycle = stack.clone();
            cycle.push(name.to_string());
            return Err(ManifestError::Cycle {
                path: cycle.join(" -> "),
            });
        }
        let app = self
            .app(name.as_str())
            .ok_or_else(|| ManifestError::Unknown {
                kind: "app",
                name: name.to_string(),
                context: "dependency depth".to_owned(),
            })?;
        stack.push(name.to_string());
        let mut depth = 0;
        for requirement in &app.requires {
            depth = depth.max(self.depth_inner(requirement, memo, stack)? + 1);
        }
        stack.pop();
        memo.insert(name.to_string(), depth);
        Ok(depth)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisitState {
    Visiting,
    Done,
}

impl AppSpec {
    /// Returns the project name, defaulting to the app name.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] if the app name is not a valid project name. App
    /// names and project names share the same DNS-label rules, so this only
    /// fails for manifests that bypassed name validation.
    pub fn project_name(&self) -> Result<ProjectName, ValueError> {
        match &self.project {
            Some(project) => Ok(project.clone()),
            None => ProjectName::new(self.name.as_str()),
        }
    }

    /// Returns the Helm release name, defaulting to the app name.
    #[must_use]
    pub fn release_name(&self) -> &str {
        self.helm
            .release_name
            .as_deref()
            .unwrap_or_else(|| self.name.as_str())
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn default_root() -> PathBuf {
    PathBuf::from(".")
}

fn default_main() -> String {
    "main".to_owned()
}

fn default_source_repositories() -> Vec<String> {
    vec!["*".to_owned()]
}

fn default_cluster_name() -> String {
    "astrarium".to_owned()
}

fn default_kubeconfig() -> PathBuf {
    PathBuf::from(".astrium/kubeconfig")
}

fn default_argocd_install() -> PathBuf {
    PathBuf::from("hack/e2e/argocd")
}

fn default_argocd_namespace() -> NamespaceName {
    NamespaceName::new("argocd").unwrap_or_else(|_| unreachable!("argocd is a static namespace"))
}

fn default_argocd_project() -> String {
    "astrarium".to_owned()
}

fn default_repo_name() -> String {
    "astrarium".to_owned()
}

fn default_argocd_server() -> String {
    "https://kubernetes.default.svc".to_owned()
}

fn default_argocd_wait() -> HumanDuration {
    HumanDuration::new(std::time::Duration::from_secs(600))
}

fn default_helm() -> String {
    "helm".to_owned()
}

fn default_kustomize() -> String {
    "kustomize".to_owned()
}

fn default_kind() -> String {
    "kind".to_owned()
}

fn default_render_namespace() -> NamespaceName {
    NamespaceName::new("default").unwrap_or_else(|_| unreachable!("default is a static namespace"))
}

fn default_render_timeout() -> HumanDuration {
    HumanDuration::new(std::time::Duration::from_secs(600))
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4317".to_owned()
}

fn default_service_name() -> String {
    "astrarium".to_owned()
}

fn default_registry_host() -> String {
    "auto".to_owned()
}

fn default_registry_mirrors() -> Vec<RegistryMirror> {
    vec![
        RegistryMirror {
            upstream: "docker.io".to_owned(),
            endpoint: "http://{host}:5000".to_owned(),
        },
        RegistryMirror {
            upstream: "quay.io".to_owned(),
            endpoint: "http://{host}:5001".to_owned(),
        },
        RegistryMirror {
            upstream: "ghcr.io".to_owned(),
            endpoint: "http://{host}:5002".to_owned(),
        },
        RegistryMirror {
            upstream: "registry.k8s.io".to_owned(),
            endpoint: "http://{host}:5003".to_owned(),
        },
    ]
}

fn default_true() -> bool {
    true
}

fn default_retry_limit() -> u32 {
    20
}

fn default_retry_backoff() -> HumanDuration {
    HumanDuration::new(std::time::Duration::from_secs(5))
}

fn default_retry_max_duration() -> HumanDuration {
    HumanDuration::new(std::time::Duration::from_secs(300))
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            provider: ClusterProvider::Kind,
            name: default_cluster_name(),
            kind_config: None,
            kubeconfig: default_kubeconfig(),
            reuse: false,
            keep: false,
            node_image: None,
        }
    }
}

impl Default for ArgoCdConfig {
    fn default() -> Self {
        Self {
            install: default_argocd_install(),
            namespace: default_argocd_namespace(),
            project: default_argocd_project(),
            repo_name: default_repo_name(),
            server: default_argocd_server(),
            wait_timeout: default_argocd_wait(),
        }
    }
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            helm: default_helm(),
            kustomize: default_kustomize(),
            kind: default_kind(),
            namespace: default_render_namespace(),
            timeout: default_render_timeout(),
            dry_run: true,
        }
    }
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            otlp_endpoint: default_otlp_endpoint(),
            service_name: default_service_name(),
            environment: None,
        }
    }
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: default_registry_host(),
            mirrors: default_registry_mirrors(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_manifest(extra_apps: &str) -> String {
        format!(
            r#"
[project]
name = "test"
root = "."

[[projects]]
name = "alpha"
source_repositories = ["*"]
destinations = [{{ namespace = "alpha" }}]

[[projects]]
name = "beta"
source_repositories = ["*"]
destinations = [{{ namespace = "alpha" }}]

[[projects]]
name = "gamma"
source_repositories = ["*"]
destinations = [{{ namespace = "alpha" }}]

[[namespaces]]
name = "alpha"

[[apps]]
name = "alpha"
namespace = "alpha"
source = "alpha"
{extra_apps}
"#
        )
    }

    #[test]
    fn parses_minimal_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest::from_toml_str(&base_manifest(""), dir.path()).unwrap();
        assert_eq!(manifest.project.name, "test");
        assert_eq!(manifest.apps.len(), 1);
        assert_eq!(manifest.apps[0].renderer, RendererKind::Helm);
        assert_eq!(manifest.apps[0].sync_policy.retry_limit, 20);
    }

    #[test]
    fn rejects_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let source = base_manifest("").replace("[project]", "[project]\nunknown = true");
        let error = Manifest::from_toml_str(&source, dir.path()).unwrap_err();
        assert!(matches!(error, ManifestError::Parse { .. }));
    }

    #[test]
    fn rejects_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let source = base_manifest(
            r#"
[[apps]]
name = "beta"
namespace = "alpha"
source = "beta"
requires = ["alpha"]

[[apps]]
name = "gamma"
namespace = "alpha"
source = "gamma"
requires = ["beta"]

[[apps]]
name = "alpha"
namespace = "alpha"
source = "alpha"
requires = ["gamma"]
"#,
        );
        // Replace the original alpha with the cyclic definition.
        let source = source.replacen(
            "[[apps]]\nname = \"alpha\"\nnamespace = \"alpha\"\nsource = \"alpha\"\n",
            "",
            1,
        );
        let error = Manifest::from_toml_str(&source, dir.path()).unwrap_err();
        assert!(matches!(error, ManifestError::Cycle { .. }), "{error:?}");
    }

    #[test]
    fn detects_dependency_order_and_depth() {
        let dir = tempfile::tempdir().unwrap();
        let source = base_manifest(
            r#"
[[apps]]
name = "beta"
namespace = "alpha"
source = "beta"
requires = ["alpha"]

[[apps]]
name = "gamma"
namespace = "alpha"
source = "gamma"
requires = ["beta"]
"#,
        );
        let manifest = Manifest::from_toml_str(&source, dir.path()).unwrap();
        let order = manifest
            .dependency_order(&[AppName::new("gamma").unwrap()])
            .unwrap();
        assert_eq!(
            order.iter().map(AppName::as_str).collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"]
        );
        assert_eq!(manifest.depth(&AppName::new("gamma").unwrap()).unwrap(), 2);
    }

    #[test]
    fn rejects_path_escape() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest::from_toml_str(&base_manifest(""), dir.path()).unwrap();
        let error = manifest.resolve_path(Path::new("../outside")).unwrap_err();
        assert!(matches!(error, ManifestError::PathEscape { .. }));
    }

    #[test]
    fn rejects_unknown_project_and_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let source = base_manifest("").replace(
            "namespace = \"alpha\"\nsource",
            "namespace = \"missing\"\nsource",
        );
        let error = Manifest::from_toml_str(&source, dir.path()).unwrap_err();
        assert!(matches!(error, ManifestError::Unknown { .. }), "{error:?}");
    }

    #[test]
    fn registry_defaults_have_mirrors() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest::from_toml_str(&base_manifest(""), dir.path()).unwrap();
        assert_eq!(manifest.registry.mirrors.len(), 4);
        assert_eq!(manifest.registry.mirrors[0].endpoint, "http://{host}:5000");
    }

    #[test]
    fn projects_default_to_every_source_and_no_cluster_blacklist() {
        let dir = tempfile::tempdir().unwrap();
        let source = base_manifest("").replace("source_repositories = [\"*\"]\n", "");
        let manifest = Manifest::from_toml_str(&source, dir.path()).unwrap();
        assert_eq!(manifest.projects[0].source_repositories, vec!["*"]);
        assert!(manifest.projects[0].cluster_resource_blacklist.is_empty());
    }

    #[test]
    fn project_cluster_blacklist_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let source = base_manifest("").replace(
            "source_repositories = [\"*\"]\n",
            "cluster_resource_blacklist = [{ group = \"\", kind = \"Namespace\" }]\n",
        );
        let manifest = Manifest::from_toml_str(&source, dir.path()).unwrap();
        assert_eq!(manifest.projects[0].cluster_resource_blacklist.len(), 1);
        let serialized = toml::to_string(&manifest.projects[0]).unwrap();
        assert!(serialized.contains("cluster_resource_blacklist"));
        assert!(serialized.contains("Namespace"));
    }

    #[test]
    fn parses_containerd_requirement_and_watch_paths() {
        let dir = tempfile::tempdir().unwrap();
        let source = base_manifest("").replace(
            "source = \"alpha\"",
            "source = \"alpha\"\nwatch = [\"shared\"]\nrequires_containerd_config = true",
        );
        let manifest = Manifest::from_toml_str(&source, dir.path()).unwrap();
        assert_eq!(manifest.apps[0].watch, vec![PathBuf::from("shared")]);
        assert!(manifest.apps[0].requires_containerd_config);
    }
}
