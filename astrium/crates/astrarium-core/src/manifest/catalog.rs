//! Read-only adapter for the repository's YAML app-of-apps catalogues.
//!
//! The native `astrarium.toml` mode remains the default. A suite can opt in to
//! this adapter with a `[catalog]` table; Astrarium then reads the production
//! `apps/values.yaml` and the e2e `hack/e2e/apps.yaml`, converts the test
//! subset into the native [`ProjectSpec`], [`NamespaceSpec`] and [`AppSpec`]
//! values, and runs the normal manifest validation afterwards.
//!
//! The adapter is read-only. It never deploys the production `apps` chart and
//! never writes the catalogues.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    AppSpec, HelmOptions, HelmRepository, KustomizeOptions, NamespaceSpec, ProjectDestination,
    ProjectSpec, PrometheusTargets, RendererKind, ResourceKind, ResourceRef, StaleProbeResources,
    SyncPolicySpec, UpgradeSecurityProfile, UpgradeSpec, default_upgrade_timeout,
    normalize_lexically,
};
use crate::types::{AppName, NamespaceName, ProjectName, ValueError};

/// The optional `[catalog]` table in `astrarium.toml`.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfig {
    /// Repository-relative path to the production app-of-apps values file.
    pub apps: PathBuf,
    /// Repository-relative path to the e2e application metadata file.
    pub e2e: PathBuf,
    /// A Helm values file shared by every imported app, merged before the
    /// app's own value files. The e2e suite uses it for `global.e2e` probe
    /// defaults.
    #[serde(default)]
    pub common_values: Option<PathBuf>,
}

/// An error while reading or converting a YAML catalogue.
#[derive(Debug, Error)]
pub enum CatalogError {
    /// A catalogue could not be read.
    #[error("failed to read catalog {path}: {source}")]
    Read {
        /// The catalogue path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A catalogue is not valid YAML or does not match the expected shape.
    #[error("failed to parse catalog {path}: {source}")]
    Parse {
        /// The catalogue path.
        path: PathBuf,
        /// The YAML parser error.
        #[source]
        source: yaml_serde::Error,
    },
    /// A catalogue path escapes the project root.
    #[error("catalog path {path:?} escapes the project root")]
    PathEscape {
        /// The offending path.
        path: PathBuf,
    },
    /// A catalogue declares the same name more than once.
    #[error("duplicate catalog {kind} {name:?}")]
    Duplicate {
        /// The kind of declaration, for example `application`.
        kind: &'static str,
        /// The duplicated name.
        name: String,
    },
    /// An e2e app has no application in the production catalogue.
    #[error("e2e app {app:?} has no application in {catalog}")]
    MissingApplication {
        /// The e2e app name.
        app: String,
        /// The production catalogue path.
        catalog: PathBuf,
    },
    /// A catalogue application references a project that is not declared.
    #[error("catalog application {app:?} references unknown project {project:?}")]
    MissingProject {
        /// The application name.
        app: String,
        /// The missing project.
        project: String,
    },
    /// An e2e app requires another app that the e2e catalogue does not declare.
    #[error("e2e app {app:?} requires {required:?}, which is not declared in the e2e catalog")]
    MissingE2eApp {
        /// The app that declares the requirement.
        app: String,
        /// The missing prerequisite.
        required: String,
    },
    /// An application has no destination namespace and its project cannot
    /// supply one.
    #[error(
        "application {app:?} has no destination namespace and project {project:?} has no destinations"
    )]
    MissingNamespace {
        /// The application name.
        app: String,
        /// The project name.
        project: String,
    },
    /// A destination namespace is the wildcard, which is not a real namespace.
    #[error(
        "application {app:?} has no concrete destination namespace; the application destination or project {project:?} uses \"*\""
    )]
    WildcardNamespace {
        /// The application name.
        app: String,
        /// The project name.
        project: String,
    },
    /// An application source directory does not exist.
    #[error("catalog application {app:?} source {path} does not exist")]
    MissingSource {
        /// The application name.
        app: String,
        /// The missing source path.
        path: PathBuf,
    },
    /// An application source has neither a Helm chart nor a Kustomization.
    #[error("catalog application {app:?} source {path} has neither Chart.yaml nor a kustomization")]
    UnknownRenderer {
        /// The application name.
        app: String,
        /// The source path.
        path: PathBuf,
    },
    /// An upgrade test uses an unknown security profile.
    #[error("e2e app {app:?} upgrade.security_profile must be restricted or none (got {value:?})")]
    InvalidUpgradeProfile {
        /// The e2e app name.
        app: String,
        /// The rejected profile.
        value: String,
    },
    /// A catalogue value is not a valid Kubernetes name or duration.
    #[error(transparent)]
    Value(#[from] ValueError),
}

/// The native manifest values produced from the catalogues.
pub(super) struct CatalogSuite {
    /// AppProjects referenced by the selected applications.
    pub projects: Vec<ProjectSpec>,
    /// Namespaces required by the selected applications and project
    /// destinations.
    pub namespaces: Vec<NamespaceSpec>,
    /// Applications listed in the e2e catalogue.
    pub apps: Vec<AppSpec>,
    /// Helm repositories from the e2e catalogue.
    pub helm_repositories: Vec<HelmRepository>,
}

/// Reads both catalogues and converts the test subset to native manifest
/// values.
pub(super) fn load(
    root: &Path,
    apps_path: &Path,
    e2e_path: &Path,
    common_values: Option<&Path>,
) -> Result<CatalogSuite, CatalogError> {
    let values: ValuesCatalog = read_yaml(apps_path)?;
    let e2e: E2eCatalog = read_yaml(e2e_path)?;

    let projects = index_projects(values.projects)?;
    let namespaces = index_namespaces(values.namespaces)?;
    let applications = index_applications(values.applications)?;

    let mut selected_projects = BTreeSet::new();
    let mut selected_namespaces = BTreeSet::new();
    for (name, app) in &e2e.apps {
        let catalog_app =
            applications
                .get(name)
                .ok_or_else(|| CatalogError::MissingApplication {
                    app: name.clone(),
                    catalog: apps_path.to_path_buf(),
                })?;
        let project =
            projects
                .get(&catalog_app.project)
                .ok_or_else(|| CatalogError::MissingProject {
                    app: name.clone(),
                    project: catalog_app.project.clone(),
                })?;
        for required in &app.requires {
            if !e2e.apps.contains_key(required) {
                return Err(CatalogError::MissingE2eApp {
                    app: name.clone(),
                    required: required.clone(),
                });
            }
        }
        selected_projects.insert(catalog_app.project.clone());
        selected_namespaces.insert(destination_namespace(catalog_app, project)?);
    }

    for project_name in &selected_projects {
        let Some(project) = projects.get(project_name) else {
            continue;
        };
        for destination in &project.destinations {
            if destination.namespace != "*" {
                selected_namespaces.insert(destination.namespace.clone());
            }
        }
    }

    let converted_projects = projects
        .order
        .iter()
        .filter(|name| selected_projects.contains(*name))
        .map(|name| project_spec(&projects.items[name]))
        .collect::<Result<Vec<_>, _>>()?;

    let converted_namespaces = selected_namespaces
        .iter()
        .map(|name| namespace_spec(name, namespaces.get(name)))
        .collect::<Result<Vec<_>, _>>()?;

    let converted_apps = e2e
        .apps
        .iter()
        .map(|(name, app)| {
            app_spec(
                root,
                apps_path,
                name,
                app,
                &e2e.defaults.upgrade,
                common_values,
                &applications,
                &projects,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CatalogSuite {
        projects: converted_projects,
        namespaces: converted_namespaces,
        apps: converted_apps,
        helm_repositories: e2e
            .helm_repositories
            .into_iter()
            .map(|repository| HelmRepository {
                name: repository.name,
                url: repository.url,
            })
            .collect(),
    })
}

fn read_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CatalogError> {
    let source = fs::read_to_string(path).map_err(|source| CatalogError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    yaml_serde::from_str(&source).map_err(|source| CatalogError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

struct Indexed<T> {
    order: Vec<String>,
    items: BTreeMap<String, T>,
}

fn index_projects(projects: Vec<CatalogProject>) -> Result<Indexed<CatalogProject>, CatalogError> {
    let mut indexed = Indexed {
        order: Vec::new(),
        items: BTreeMap::new(),
    };
    for project in projects {
        let name = project.name.clone();
        if indexed.items.insert(name.clone(), project).is_some() {
            return Err(CatalogError::Duplicate {
                kind: "project",
                name,
            });
        }
        indexed.order.push(name);
    }
    Ok(indexed)
}

fn index_namespaces(
    namespaces: Vec<CatalogNamespace>,
) -> Result<Indexed<CatalogNamespace>, CatalogError> {
    let mut indexed = Indexed {
        order: Vec::new(),
        items: BTreeMap::new(),
    };
    for namespace in namespaces {
        let name = namespace.name.clone();
        if indexed.items.insert(name.clone(), namespace).is_some() {
            return Err(CatalogError::Duplicate {
                kind: "namespace",
                name,
            });
        }
        indexed.order.push(name);
    }
    Ok(indexed)
}

fn index_applications(
    applications: Vec<CatalogApplication>,
) -> Result<Indexed<CatalogApplication>, CatalogError> {
    let mut indexed = Indexed {
        order: Vec::new(),
        items: BTreeMap::new(),
    };
    for application in applications {
        let name = application.name.clone();
        if indexed.items.insert(name.clone(), application).is_some() {
            return Err(CatalogError::Duplicate {
                kind: "application",
                name,
            });
        }
        indexed.order.push(name);
    }
    Ok(indexed)
}

impl<T> Indexed<T> {
    fn get(&self, name: &str) -> Option<&T> {
        self.items.get(name)
    }
}

fn destination_namespace(
    app: &CatalogApplication,
    project: &CatalogProject,
) -> Result<String, CatalogError> {
    if let Some(destination) = &app.destination {
        if destination.namespace.trim().is_empty() {
            return Err(CatalogError::MissingNamespace {
                app: app.name.clone(),
                project: project.name.clone(),
            });
        }
        if destination.namespace == "*" {
            return Err(CatalogError::WildcardNamespace {
                app: app.name.clone(),
                project: project.name.clone(),
            });
        }
        return Ok(destination.namespace.clone());
    }

    let Some(destination) = project.destinations.first() else {
        return Err(CatalogError::MissingNamespace {
            app: app.name.clone(),
            project: project.name.clone(),
        });
    };
    if destination.namespace == "*" {
        return Err(CatalogError::WildcardNamespace {
            app: app.name.clone(),
            project: project.name.clone(),
        });
    }
    if destination.namespace.trim().is_empty() {
        return Err(CatalogError::MissingNamespace {
            app: app.name.clone(),
            project: project.name.clone(),
        });
    }
    Ok(destination.namespace.clone())
}

fn project_spec(project: &CatalogProject) -> Result<ProjectSpec, CatalogError> {
    let cluster_resource_whitelist = convert_kinds(project.cluster_resource_whitelist.as_deref());
    let cluster_resource_blacklist = convert_kinds(project.cluster_resource_blacklist.as_deref());
    let (namespace_resource_whitelist, namespace_resource_blacklist) = namespace_lists(project);

    Ok(ProjectSpec {
        name: ProjectName::new(project.name.clone())?,
        description: project.description.clone(),
        source_repositories: project.source_repositories.clone(),
        destinations: project
            .destinations
            .iter()
            .map(|destination| ProjectDestination {
                server: destination.server.clone(),
                name: destination.name.clone(),
                namespace: destination.namespace.clone(),
            })
            .collect(),
        cluster_resource_whitelist,
        cluster_resource_blacklist,
        namespace_resource_whitelist,
        namespace_resource_blacklist,
    })
}

fn namespace_lists(
    project: &CatalogProject,
) -> (Option<Vec<ResourceKind>>, Option<Vec<ResourceKind>>) {
    match (
        &project.namespace_resource_whitelist,
        &project.namespace_resource_blacklist,
    ) {
        (Some(whitelist), Some(blacklist)) => (
            Some(convert_kinds(Some(whitelist))),
            Some(convert_kinds(Some(blacklist))),
        ),
        (Some(whitelist), None) => (Some(convert_kinds(Some(whitelist))), None),
        (None, Some(blacklist)) => (None, Some(convert_kinds(Some(blacklist)))),
        (None, None) => (None, Some(default_namespace_blacklist())),
    }
}

fn default_true() -> bool {
    true
}

fn default_namespace_blacklist() -> Vec<ResourceKind> {
    vec![
        ResourceKind {
            group: String::new(),
            kind: "ResourceQuota".to_owned(),
        },
        ResourceKind {
            group: String::new(),
            kind: "LimitRange".to_owned(),
        },
        ResourceKind {
            group: "networking.k8s.io".to_owned(),
            kind: "NetworkPolicy".to_owned(),
        },
    ]
}

fn convert_kinds(kinds: Option<&[CatalogResourceKind]>) -> Vec<ResourceKind> {
    kinds
        .unwrap_or_default()
        .iter()
        .map(|kind| ResourceKind {
            group: kind.group.clone(),
            kind: kind.kind.clone(),
        })
        .collect()
}

fn namespace_spec(
    name: &str,
    catalog: Option<&CatalogNamespace>,
) -> Result<NamespaceSpec, CatalogError> {
    let (labels, annotations) = catalog.map_or_else(
        || (BTreeMap::new(), BTreeMap::new()),
        |namespace| (namespace.labels.clone(), namespace.annotations.clone()),
    );
    Ok(NamespaceSpec {
        name: NamespaceName::new(name)?,
        labels,
        annotations,
    })
}

#[allow(clippy::too_many_arguments)]
fn app_spec(
    root: &Path,
    apps_path: &Path,
    name: &str,
    e2e: &E2eApp,
    upgrade_defaults: &CatalogUpgradeDefaults,
    common_values: Option<&Path>,
    applications: &Indexed<CatalogApplication>,
    projects: &Indexed<CatalogProject>,
) -> Result<AppSpec, CatalogError> {
    let application = applications
        .get(name)
        .ok_or_else(|| CatalogError::MissingApplication {
            app: name.to_owned(),
            catalog: apps_path.to_path_buf(),
        })?;
    let project =
        projects
            .get(&application.project)
            .ok_or_else(|| CatalogError::MissingProject {
                app: name.to_owned(),
                project: application.project.clone(),
            })?;
    let namespace = destination_namespace(application, project)?;

    let relative = normalize_lexically(Path::new(&application.source.path));
    let source = kind_source(root, &relative, name)?;
    let source_path = root.join(&source);
    let renderer = if source_path.join("Chart.yaml").is_file() {
        RendererKind::Helm
    } else if is_kustomization_dir(&source_path) {
        RendererKind::Kustomize
    } else {
        return Err(CatalogError::UnknownRenderer {
            app: name.to_owned(),
            path: source,
        });
    };

    let mut values = application
        .source
        .helm
        .as_ref()
        .map_or_else(Vec::new, |helm| {
            helm.value_files
                .iter()
                .map(|path| normalize_lexically(path))
                .collect()
        });
    if renderer == RendererKind::Helm {
        let kind_values = PathBuf::from("values-kind.yaml");
        if source_path.join(&kind_values).is_file() && !values.contains(&kind_values) {
            values.push(kind_values);
        }
    }

    let watch = if source == relative {
        Vec::new()
    } else {
        vec![relative]
    };

    Ok(AppSpec {
        name: AppName::new(name)?,
        namespace: NamespaceName::new(namespace)?,
        project: Some(ProjectName::new(application.project.clone())?),
        source,
        watch,
        renderer,
        values,
        common_values: common_values.map(Path::to_path_buf),
        inline_values: BTreeMap::new(),
        requires: e2e
            .requires
            .iter()
            .map(|required| AppName::new(required.clone()))
            .collect::<Result<_, _>>()?,
        probe: false,
        upgrade: e2e
            .upgrade
            .as_ref()
            .map(|upgrade| upgrade_spec(name, upgrade, upgrade_defaults))
            .transpose()?,
        requires_fresh_image: e2e.requires_fresh_image,
        requires_containerd_config: e2e.requires_containerd_config,
        stale_probe_resources: e2e
            .stale_probe_resources
            .as_ref()
            .map(stale_probe_resources)
            .transpose()?,
        resources: e2e
            .resources
            .iter()
            .map(resource_ref)
            .collect::<Result<_, _>>()?,
        prometheus_targets: e2e
            .prometheus_targets
            .as_ref()
            .map(prometheus_targets)
            .transpose()?,
        timeout_seconds: e2e.timeout_seconds,
        sync_policy: sync_policy(application, e2e.retry_limit),
        annotations: application.annotations.clone(),
        helm: HelmOptions::default(),
        kustomize: KustomizeOptions::default(),
    })
}

fn upgrade_spec(
    app: &str,
    upgrade: &CatalogUpgrade,
    defaults: &CatalogUpgradeDefaults,
) -> Result<UpgradeSpec, CatalogError> {
    let security_profile = upgrade
        .security_profile
        .as_deref()
        .or(defaults.security_profile.as_deref())
        .unwrap_or("restricted");
    let security_profile = match security_profile {
        "restricted" => UpgradeSecurityProfile::Restricted,
        "none" => UpgradeSecurityProfile::None,
        other => {
            return Err(CatalogError::InvalidUpgradeProfile {
                app: app.to_owned(),
                value: other.to_owned(),
            });
        }
    };
    Ok(UpgradeSpec {
        enabled: upgrade.enabled,
        from_values: upgrade.from_values.clone(),
        from_revision: None,
        container: upgrade.container.clone(),
        data_path: upgrade.data_path.clone(),
        timeout_seconds: upgrade
            .timeout_seconds
            .or(defaults.timeout_seconds)
            .unwrap_or_else(default_upgrade_timeout),
        security_profile,
        verify_pvc_retention: upgrade
            .verify_pvc_retention
            .or(defaults.verify_pvc_retention)
            .unwrap_or(true),
    })
}

fn kind_source(root: &Path, path: &Path, app: &str) -> Result<PathBuf, CatalogError> {
    if path.is_absolute() || path.as_os_str().is_empty() {
        return Err(CatalogError::PathEscape {
            path: path.to_path_buf(),
        });
    }
    let absolute = normalize_lexically(&root.join(path));
    if !absolute.starts_with(root) {
        return Err(CatalogError::PathEscape {
            path: path.to_path_buf(),
        });
    }
    if !absolute.is_dir() {
        return Err(CatalogError::MissingSource {
            app: app.to_owned(),
            path: path.to_path_buf(),
        });
    }

    let overlay = normalize_lexically(&path.join("overlays/kind"));
    let overlay_path = root.join(&overlay);
    if is_kustomization_dir(&overlay_path) {
        Ok(overlay)
    } else {
        Ok(path.to_path_buf())
    }
}

fn is_kustomization_dir(path: &Path) -> bool {
    ["kustomization.yaml", "kustomization.yml", "Kustomization"]
        .iter()
        .any(|name| path.join(name).is_file())
}

fn sync_policy(application: &CatalogApplication, retry_limit: Option<u32>) -> SyncPolicySpec {
    let mut policy = match &application.sync_policy {
        Some(policy) => SyncPolicySpec {
            automated: policy.automated.is_some(),
            prune: policy
                .automated
                .as_ref()
                .is_some_and(|automated| automated.prune),
            self_heal: policy
                .automated
                .as_ref()
                .is_some_and(|automated| automated.self_heal),
            sync_options: policy.sync_options.clone(),
            ..SyncPolicySpec::default()
        },
        None => SyncPolicySpec::default(),
    };
    if let Some(limit) = retry_limit {
        policy.retry_limit = limit;
    }
    policy
}

fn resource_ref(resource: &CatalogResourceRef) -> Result<ResourceRef, CatalogError> {
    Ok(ResourceRef {
        api_version: resource.api_version.clone(),
        kind: resource.kind.clone(),
        namespace: resource
            .namespace
            .as_deref()
            .map(NamespaceName::new)
            .transpose()?,
        name: resource.name.clone(),
    })
}

fn prometheus_targets(
    targets: &CatalogPrometheusTargets,
) -> Result<PrometheusTargets, CatalogError> {
    Ok(PrometheusTargets {
        namespace: NamespaceName::new(targets.namespace.clone())?,
        service: targets.service.clone(),
        port: targets.port,
        jobs: targets.jobs.clone(),
    })
}

fn stale_probe_resources(
    stale: &CatalogStaleProbeResources,
) -> Result<StaleProbeResources, CatalogError> {
    Ok(StaleProbeResources {
        namespace: NamespaceName::new(stale.namespace.clone())?,
        resource_names: stale.resource_names.clone(),
        pod_label_selectors: stale.pod_label_selectors.clone(),
    })
}

/// The production catalogue. Chart-wide keys such as `repository` and
/// `argocdNamespace` are intentionally ignored; only the project, namespace
/// and application lists are imported.
#[derive(Debug, Deserialize)]
struct ValuesCatalog {
    #[serde(default)]
    projects: Vec<CatalogProject>,
    #[serde(default)]
    namespaces: Vec<CatalogNamespace>,
    #[serde(default)]
    applications: Vec<CatalogApplication>,
}

#[derive(Clone, Debug, Deserialize)]
struct CatalogProject {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    destinations: Vec<CatalogDestination>,
    #[serde(default, rename = "sourceRepos")]
    source_repositories: Vec<String>,
    #[serde(default, rename = "clusterResourceWhitelist")]
    cluster_resource_whitelist: Option<Vec<CatalogResourceKind>>,
    #[serde(default, rename = "clusterResourceBlacklist")]
    cluster_resource_blacklist: Option<Vec<CatalogResourceKind>>,
    #[serde(default, rename = "namespaceResourceWhitelist")]
    namespace_resource_whitelist: Option<Vec<CatalogResourceKind>>,
    #[serde(default, rename = "namespaceResourceBlacklist")]
    namespace_resource_blacklist: Option<Vec<CatalogResourceKind>>,
}

#[derive(Clone, Debug, Deserialize)]
struct CatalogDestination {
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    name: Option<String>,
    namespace: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CatalogResourceKind {
    #[serde(default)]
    group: String,
    kind: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CatalogNamespace {
    name: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
struct CatalogApplication {
    name: String,
    project: String,
    #[serde(default)]
    destination: Option<CatalogDestination>,
    source: CatalogSource,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
    #[serde(default, rename = "syncPolicy")]
    sync_policy: Option<CatalogSyncPolicy>,
}

#[derive(Clone, Debug, Deserialize)]
struct CatalogSource {
    path: String,
    #[serde(default)]
    helm: Option<CatalogHelm>,
}

#[derive(Clone, Debug, Deserialize)]
struct CatalogHelm {
    #[serde(default, rename = "valueFiles")]
    value_files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
struct CatalogSyncPolicy {
    #[serde(default)]
    automated: Option<CatalogAutomated>,
    #[serde(default, rename = "syncOptions")]
    sync_options: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct CatalogAutomated {
    #[serde(default)]
    prune: bool,
    #[serde(default, rename = "selfHeal")]
    self_heal: bool,
}

/// The e2e catalogue from `hack/e2e/apps.yaml`.
#[derive(Debug, Deserialize)]
struct E2eCatalog {
    #[serde(default)]
    helm_repositories: Vec<CatalogHelmRepository>,
    #[serde(default)]
    defaults: E2eDefaults,
    apps: BTreeMap<String, E2eApp>,
}

#[derive(Debug, Default, Deserialize)]
struct E2eDefaults {
    #[serde(default)]
    upgrade: CatalogUpgradeDefaults,
}

#[derive(Debug, Default, Deserialize)]
struct CatalogUpgradeDefaults {
    #[serde(default, alias = "timeoutSeconds")]
    timeout_seconds: Option<u64>,
    #[serde(default, alias = "securityProfile")]
    security_profile: Option<String>,
    #[serde(default, alias = "verifyPvcRetention")]
    verify_pvc_retention: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CatalogHelmRepository {
    name: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct E2eApp {
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    resources: Vec<CatalogResourceRef>,
    #[serde(default)]
    prometheus_targets: Option<CatalogPrometheusTargets>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    requires_fresh_image: bool,
    #[serde(default)]
    requires_containerd_config: bool,
    #[serde(default)]
    stale_probe_resources: Option<CatalogStaleProbeResources>,
    #[serde(default)]
    retry_limit: Option<u32>,
    #[serde(default)]
    upgrade: Option<CatalogUpgrade>,
}

#[derive(Debug, Deserialize)]
struct CatalogUpgrade {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    from_values: Vec<PathBuf>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default, alias = "dataPath")]
    data_path: Option<String>,
    #[serde(default, alias = "timeoutSeconds")]
    timeout_seconds: Option<u64>,
    #[serde(default, alias = "securityProfile")]
    security_profile: Option<String>,
    #[serde(default, alias = "verifyPvcRetention")]
    verify_pvc_retention: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CatalogResourceRef {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    #[serde(default)]
    namespace: Option<String>,
    name: String,
}

#[derive(Debug, Deserialize)]
struct CatalogPrometheusTargets {
    namespace: String,
    service: String,
    port: u16,
    #[serde(default)]
    jobs: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogStaleProbeResources {
    namespace: String,
    #[serde(default)]
    resource_names: Vec<String>,
    #[serde(default)]
    pod_label_selectors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Manifest, ManifestError};
    use crate::types::AppName;

    const MANIFEST: &str = r#"
[project]
name = "test"
root = "."

[catalog]
apps = "apps.yaml"
e2e = "e2e.yaml"

[telemetry]
enabled = false

[registry]
enabled = false
mirrors = []
"#;

    fn write_catalogs(directory: &Path, apps: &str, e2e: &str) {
        fs::write(directory.join("apps.yaml"), apps).expect("apps catalog");
        fs::write(directory.join("e2e.yaml"), e2e).expect("e2e catalog");
    }

    fn repository_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    fn kinds(kinds: &[ResourceKind]) -> Vec<(&str, &str)> {
        kinds
            .iter()
            .map(|kind| (kind.group.as_str(), kind.kind.as_str()))
            .collect()
    }

    #[test]
    fn converts_the_repository_catalogues() {
        let root = repository_root();
        let manifest = Manifest::load(root.join("astrarium.toml")).expect("repository manifest");

        let e2e: E2eCatalog = read_yaml(&root.join("hack/e2e/apps.yaml")).expect("e2e catalog");
        for (name, expected) in &e2e.apps {
            let app = manifest.app(name).unwrap_or_else(|| {
                panic!("{name:?} is in the e2e catalog but not converted");
            });
            assert_eq!(
                app.requires.iter().map(AppName::as_str).collect::<Vec<_>>(),
                expected.requires
            );
            assert_eq!(app.requires_fresh_image, expected.requires_fresh_image);
            assert_eq!(
                app.requires_containerd_config,
                expected.requires_containerd_config
            );
            assert_eq!(app.timeout_seconds, expected.timeout_seconds);
            assert_eq!(app.resources.len(), expected.resources.len());
            for (actual, expected) in app.resources.iter().zip(&expected.resources) {
                assert_eq!(actual.api_version, expected.api_version);
                assert_eq!(actual.kind, expected.kind);
                assert_eq!(
                    actual.namespace.as_ref().map(NamespaceName::as_str),
                    expected.namespace.as_deref()
                );
                assert_eq!(actual.name, expected.name);
            }
            match (&app.prometheus_targets, &expected.prometheus_targets) {
                (Some(actual), Some(expected)) => {
                    assert_eq!(actual.namespace.as_str(), expected.namespace);
                    assert_eq!(actual.service, expected.service);
                    assert_eq!(actual.port, expected.port);
                    assert_eq!(actual.jobs, expected.jobs);
                }
                (None, None) => {}
                (actual, expected) => {
                    panic!("Prometheus targets differ for {name}: {actual:?} vs {expected:?}");
                }
            }
            match (&app.stale_probe_resources, &expected.stale_probe_resources) {
                (Some(actual), Some(expected)) => {
                    assert_eq!(actual.namespace.as_str(), expected.namespace);
                    assert_eq!(actual.resource_names, expected.resource_names);
                    assert_eq!(actual.pod_label_selectors, expected.pod_label_selectors);
                }
                (None, None) => {}
                (actual, expected) => {
                    panic!("stale probe resources differ for {name}: {actual:?} vs {expected:?}");
                }
            }
        }

        assert_eq!(
            manifest
                .app("spegel")
                .expect("spegel")
                .sync_policy
                .retry_limit,
            0
        );
        assert_eq!(
            manifest
                .app("mosquitto")
                .expect("mosquitto")
                .sync_policy
                .retry_limit,
            20
        );
        assert!(!manifest.app("grafana").expect("grafana").probe);

        assert_eq!(
            manifest.app("stalwart").expect("stalwart").common_values,
            Some(PathBuf::from("hack/e2e/values-common.yaml"))
        );
        let upgrade = manifest
            .app("stalwart")
            .expect("stalwart")
            .upgrade
            .as_ref()
            .expect("stalwart upgrade test");
        assert!(upgrade.enabled);
        assert_eq!(upgrade.container.as_deref(), Some("stalwart"));
        assert_eq!(
            upgrade.from_values,
            vec![PathBuf::from("values-upgrade-from.yaml")]
        );
        assert_eq!(upgrade.data_path.as_deref(), Some("/var/lib/stalwart"));
        assert_eq!(upgrade.timeout_seconds, 600);
        assert_eq!(upgrade.security_profile, UpgradeSecurityProfile::Restricted);
        assert!(upgrade.verify_pvc_retention);
        assert!(
            manifest
                .app("mosquitto")
                .expect("mosquitto")
                .upgrade
                .is_none()
        );
    }

    #[test]
    fn converts_sources_renderers_and_values() {
        let manifest =
            Manifest::load(repository_root().join("astrarium.toml")).expect("repository manifest");

        let mosquitto = manifest.app("mosquitto").expect("mosquitto");
        assert_eq!(mosquitto.namespace.as_str(), "mosquitto");
        assert_eq!(
            mosquitto.project.as_ref().expect("project").as_str(),
            "mosquitto"
        );
        assert_eq!(mosquitto.source, PathBuf::from("mosquitto/overlays/kind"));
        assert_eq!(mosquitto.watch, vec![PathBuf::from("mosquitto")]);
        assert_eq!(mosquitto.renderer, RendererKind::Kustomize);
        assert_eq!(mosquitto.values, Vec::<PathBuf>::new());

        let cert_manager = manifest.app("cert-manager").expect("cert-manager");
        assert_eq!(cert_manager.namespace.as_str(), "cert-manager");
        assert_eq!(cert_manager.source, PathBuf::from("cert-manager"));
        assert_eq!(cert_manager.renderer, RendererKind::Helm);
        assert_eq!(cert_manager.values, vec![PathBuf::from("values-kind.yaml")]);

        let vector_agent = manifest.app("vector-agent").expect("vector-agent");
        assert_eq!(
            vector_agent.values,
            vec![
                PathBuf::from("values-agent.yaml"),
                PathBuf::from("values-kind.yaml")
            ]
        );
        assert_eq!(vector_agent.sync_policy.retry_limit, 20);

        let gateway = manifest.app("gateway-api").expect("gateway-api");
        assert_eq!(
            gateway.annotations.get("argocd.argoproj.io/sync-wave"),
            Some(&"-1".to_owned())
        );
        assert_eq!(
            gateway.sync_policy.sync_options,
            vec!["ApplyOutOfSyncOnly=true", "ServerSideApply=true"]
        );

        let monitoring = manifest.app("monitoring").expect("monitoring");
        assert_eq!(monitoring.source, PathBuf::from("monitoring/overlays/kind"));
        assert_eq!(monitoring.renderer, RendererKind::Kustomize);
        assert_eq!(monitoring.watch, vec![PathBuf::from("monitoring")]);
    }

    #[test]
    fn converts_project_defaults_and_lists() {
        let manifest =
            Manifest::load(repository_root().join("astrarium.toml")).expect("repository manifest");

        let agentgateway = manifest.project("agentgateway").expect("agentgateway");
        assert_eq!(
            agentgateway.source_repositories,
            vec!["https://github.com/mkroman/k8s"]
        );
        assert_eq!(
            kinds(
                agentgateway
                    .namespace_resource_blacklist
                    .as_deref()
                    .expect("default blacklist")
            ),
            vec![
                ("", "ResourceQuota"),
                ("", "LimitRange"),
                ("networking.k8s.io", "NetworkPolicy")
            ]
        );
        assert!(agentgateway.namespace_resource_whitelist.is_none());

        let monitoring = manifest.project("monitoring").expect("monitoring");
        assert_eq!(
            kinds(
                monitoring
                    .namespace_resource_blacklist
                    .as_deref()
                    .expect("explicit opt-out")
            ),
            Vec::<(&str, &str)>::new()
        );

        let mosquitto = manifest.project("mosquitto").expect("mosquitto");
        assert!(mosquitto.namespace_resource_whitelist.is_some());
        assert!(mosquitto.namespace_resource_blacklist.is_none());

        let cloudnative_pg = manifest.project("cloudnative-pg").expect("cloudnative-pg");
        assert!(kinds(&cloudnative_pg.cluster_resource_whitelist).contains(&("", "Namespace")));

        let spegel_namespace = manifest.namespace("spegel").expect("spegel namespace");
        assert_eq!(
            spegel_namespace
                .labels
                .get("pod-security.kubernetes.io/enforce")
                .map(String::as_str),
            Some("privileged")
        );
        assert!(manifest.namespace("kube-system").is_some());
        assert!(manifest.namespace("cnpg-system").is_some());
    }

    #[test]
    fn rejects_mixed_native_and_catalog_definitions() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = format!(
            "{MANIFEST}\n[[apps]]\nname = \"alpha\"\nnamespace = \"alpha\"\nsource = \"alpha\"\n"
        );
        let error = Manifest::from_toml_str(&source, directory.path()).expect_err("mixed mode");
        assert!(
            matches!(error, ManifestError::Invalid { ref message } if message.contains("catalog mode")),
            "{error:?}"
        );
    }

    #[test]
    fn missing_e2e_application_is_reported() {
        let directory = tempfile::tempdir().expect("tempdir");
        write_catalogs(
            directory.path(),
            r"
projects:
  - name: alpha
    destinations:
      - namespace: alpha
namespaces:
  - name: alpha
applications:
  - name: alpha
    project: alpha
    source:
      path: alpha
",
            r"
apps:
  missing: {}
",
        );
        let error = Manifest::from_toml_str(MANIFEST, directory.path()).expect_err("missing app");
        assert!(
            matches!(
                error,
                ManifestError::Catalog(CatalogError::MissingApplication { ref app, .. })
                    if app == "missing"
            ),
            "{error:?}"
        );
    }

    #[test]
    fn missing_project_is_reported() {
        let directory = tempfile::tempdir().expect("tempdir");
        write_catalogs(
            directory.path(),
            r"
projects:
  - name: alpha
    destinations:
      - namespace: alpha
namespaces:
  - name: alpha
applications:
  - name: alpha
    project: ghost
    source:
      path: alpha
",
            r"
apps:
  alpha: {}
",
        );
        let error =
            Manifest::from_toml_str(MANIFEST, directory.path()).expect_err("missing project");
        assert!(
            matches!(
                error,
                ManifestError::Catalog(CatalogError::MissingProject { ref project, .. })
                    if project == "ghost"
            ),
            "{error:?}"
        );
    }

    #[test]
    fn missing_destination_namespace_is_reported() {
        let directory = tempfile::tempdir().expect("tempdir");
        write_catalogs(
            directory.path(),
            r"
projects:
  - name: alpha
    destinations: []
namespaces: []
applications:
  - name: alpha
    project: alpha
    source:
      path: alpha
",
            r"
apps:
  alpha: {}
",
        );
        let error =
            Manifest::from_toml_str(MANIFEST, directory.path()).expect_err("missing namespace");
        assert!(
            matches!(
                error,
                ManifestError::Catalog(CatalogError::MissingNamespace { ref app, ref project })
                    if app == "alpha" && project == "alpha"
            ),
            "{error:?}"
        );
    }

    #[test]
    fn wildcard_destination_namespace_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        write_catalogs(
            directory.path(),
            r#"
projects:
  - name: alpha
    destinations:
      - namespace: "*"
namespaceResourceBlacklist: []
applications:
  - name: alpha
    project: alpha
    source:
      path: alpha
"#,
            r"
apps:
  alpha: {}
",
        );
        let error =
            Manifest::from_toml_str(MANIFEST, directory.path()).expect_err("wildcard namespace");
        assert!(
            matches!(
                error,
                ManifestError::Catalog(CatalogError::WildcardNamespace { ref app, ref project })
                    if app == "alpha" && project == "alpha"
            ),
            "{error:?}"
        );
    }

    #[test]
    fn upgrade_defaults_fill_missing_app_values() {
        let upgrade = CatalogUpgrade {
            enabled: true,
            from_values: Vec::new(),
            container: Some("app".to_owned()),
            data_path: Some("/data".to_owned()),
            timeout_seconds: None,
            security_profile: None,
            verify_pvc_retention: None,
        };
        let defaults = CatalogUpgradeDefaults {
            timeout_seconds: Some(42),
            security_profile: Some("none".to_owned()),
            verify_pvc_retention: Some(false),
        };

        let spec = upgrade_spec("demo", &upgrade, &defaults).expect("upgrade spec");
        assert_eq!(spec.timeout_seconds, 42);
        assert_eq!(spec.security_profile, UpgradeSecurityProfile::None);
        assert!(!spec.verify_pvc_retention);
    }

    #[test]
    fn unknown_upgrade_security_profile_is_rejected() {
        let upgrade = CatalogUpgrade {
            enabled: true,
            from_values: Vec::new(),
            container: None,
            data_path: None,
            timeout_seconds: None,
            security_profile: Some("permissive".to_owned()),
            verify_pvc_retention: None,
        };
        let error = upgrade_spec("demo", &upgrade, &CatalogUpgradeDefaults::default())
            .expect_err("unknown profile");
        assert!(
            matches!(error, CatalogError::InvalidUpgradeProfile { ref app, ref value }
                if app == "demo" && value == "permissive"),
            "{error:?}"
        );
    }

    #[test]
    fn missing_e2e_prerequisite_is_reported() {
        let directory = tempfile::tempdir().expect("tempdir");
        write_catalogs(
            directory.path(),
            r"
projects:
  - name: alpha
    destinations:
      - namespace: alpha
namespaces:
  - name: alpha
applications:
  - name: alpha
    project: alpha
    source:
      path: alpha
",
            r"
apps:
  alpha:
    requires:
      - missing
",
        );
        let error =
            Manifest::from_toml_str(MANIFEST, directory.path()).expect_err("missing prerequisite");
        assert!(
            matches!(
                error,
                ManifestError::Catalog(CatalogError::MissingE2eApp { ref required, .. })
                    if required == "missing"
            ),
            "{error:?}"
        );
    }
}
