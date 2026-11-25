//! Chart rendering and the local Helm chart repository.
//!
//! Astrarium turns every app in the suite into a Helm chart that Argo CD can
//! pull from an ephemeral HTTP repository:
//!
//! - a Helm app is copied to a temporary directory, its dependencies are
//!   built there, and the chart is packaged unchanged so Helm templating and
//!   Helm hooks keep working;
//! - a Kustomize app is rendered with `kustomize build` and wrapped in a tiny
//!   Helm chart whose template only copies the pre-rendered manifests.
//!
//! Values are merged as JSON, not as strings, so booleans and numbers survive
//! when they are sent to Argo CD as `source.helm.valuesObject`. The merge order
//! is: chart defaults (implicit, never read here), `common_values`, the listed
//! `values` files, then `inline_values`.

mod chart;
mod helm;
mod kustomize;
mod repository;
mod values;

#[cfg(test)]
pub(crate) mod testing;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

pub use chart::{ChartDependency, ChartMetadata};
pub use helm::{HelmHome, HelmRenderer, RetryPolicy};
pub use kustomize::{
    KustomizeRenderer, WRAPPER_CHART_TEMPLATE, WrapperChart, wrapper_chart_name,
    write_wrapper_chart,
};
pub use repository::{
    ChartRepositoryServer, HttpChartRepository, ServedChartRepository, resolve_host,
};
pub use values::{load_values_file, merge_values, merged_values};

use crate::BoxFuture;
use crate::command::CommandRunner;
use crate::manifest::{AppSpec, HelmRepository, Manifest, ManifestError, RendererKind};
use crate::types::{AppName, ChartVersion, ContentHash, NamespaceName, ValueError};

/// The version used for generated wrapper charts when no version is supplied.
///
/// It is deliberately a valid, ordinary semantic version so that it can be
/// used as the exact `targetRevision` of an Argo CD Application.
#[must_use]
pub fn default_chart_version() -> ChartVersion {
    ChartVersion::new(semver::Version::new(0, 0, 0))
}

/// A request to render one app into a Helm chart.
#[derive(Clone, Debug)]
pub struct RenderRequest {
    /// The app name.
    pub app: AppName,
    /// The chart or Kustomize source directory.
    pub source: PathBuf,
    /// The renderer.
    pub renderer: RendererKind,
    /// A values file shared by several apps, merged before `values_files`.
    pub common_values: Option<PathBuf>,
    /// Helm values files, merged in order.
    pub values_files: Vec<PathBuf>,
    /// Inline values, merged last.
    pub inline_values: serde_json::Value,
    /// The directory that receives the packaged chart.
    pub output_dir: PathBuf,
    /// The packaged chart version. `None` uses the chart's own version, or
    /// [`default_chart_version`] for generated wrappers.
    pub chart_version: Option<ChartVersion>,
    /// The Helm release name.
    pub release_name: String,
    /// The namespace used for rendering.
    pub namespace: NamespaceName,
    /// Run `helm template` and validate its output before packaging.
    pub dry_run: bool,
    /// The `helm` executable.
    pub helm_program: String,
    /// The `kustomize` executable.
    pub kustomize_program: String,
    /// Extra arguments passed to `kustomize build`.
    pub kustomize_args: Vec<String>,
    /// Repositories that must be registered before `helm dependency build`.
    pub repositories: Vec<HelmRepository>,
    /// The timeout for each external command.
    pub timeout: Duration,
}

impl RenderRequest {
    /// Builds a render request from a manifest entry.
    ///
    /// Repository-relative paths are resolved through
    /// [`Manifest::resolve_path`]. A listed values file that does not exist
    /// at the repository root is also looked up relative to the chart, which
    /// matches the per-chart `values-kind.yaml` convention. When a Kustomize
    /// source has a `overlays/kind` directory, that overlay is rendered
    /// instead of the base source.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] when a path escapes the project root or the
    /// source does not exist.
    pub fn from_manifest(
        manifest: &Manifest,
        app: &AppSpec,
        output_dir: PathBuf,
    ) -> Result<Self, RenderError> {
        let chart_source = manifest.resolve_path(&app.source)?;
        let mut source = chart_source.clone();
        if app.renderer == RendererKind::Kustomize {
            let overlay = source.join("overlays").join("kind");
            if is_kustomization_dir(&overlay) {
                source = overlay;
            }
        }
        if !source.exists() {
            return Err(RenderError::MissingSource { path: source });
        }

        let values_files = app
            .values
            .iter()
            .map(|path| resolve_values_file(manifest, &chart_source, path))
            .collect::<Result<Vec<_>, _>>()?;
        let common_values = app
            .common_values
            .as_ref()
            .map(|path| manifest.resolve_path(path))
            .transpose()?;
        let inline_values = serde_json::Value::Object(
            app.inline_values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );

        Ok(Self {
            app: app.name.clone(),
            source,
            renderer: app.renderer,
            common_values,
            values_files,
            inline_values,
            output_dir,
            chart_version: None,
            release_name: app.release_name().to_owned(),
            namespace: app.namespace.clone(),
            dry_run: manifest.render.dry_run,
            helm_program: manifest.render.helm.clone(),
            kustomize_program: manifest.render.kustomize.clone(),
            kustomize_args: app.kustomize.build_args.clone(),
            repositories: manifest.helm_repositories.clone(),
            timeout: manifest.render.timeout.get(),
        })
    }
}

/// A rendered and packaged chart.
#[derive(Clone, Debug)]
pub struct RenderedApp {
    /// The app name.
    pub app: AppName,
    /// The renderer that produced the chart.
    pub renderer: RendererKind,
    /// The chart name.
    pub chart_name: String,
    /// The chart version.
    pub chart_version: ChartVersion,
    /// The packaged `.tgz` path.
    pub chart_path: PathBuf,
    /// The merged Helm values, passed as `valuesObject`.
    pub values: serde_json::Value,
    /// A hash of the chart and values, used in diagnostics and tests.
    pub content_hash: ContentHash,
    /// The number of rendered Kubernetes objects, when known.
    pub manifest_count: Option<usize>,
}

/// An error while rendering or packaging a chart.
#[derive(Debug, Error)]
pub enum RenderError {
    /// An external command failed.
    #[error(transparent)]
    Command(#[from] crate::command::CommandError),
    /// The manifest could not be resolved.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A filesystem operation failed.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// YAML output could not be parsed.
    #[error("invalid rendered YAML: {0}")]
    Yaml(#[from] yaml_serde::Error),
    /// JSON values could not be serialized.
    #[error("invalid values JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A Helm chart is malformed.
    #[error("invalid Helm chart at {path}: {message}")]
    Chart {
        /// The chart path.
        path: PathBuf,
        /// The problem.
        message: String,
    },
    /// A values file is not a YAML mapping.
    #[error("invalid values {path}: {message}")]
    Values {
        /// The values file, or `<inline>` for inline values.
        path: String,
        /// The problem.
        message: String,
    },
    /// A source directory required by the request does not exist.
    #[error("render source {path} does not exist")]
    MissingSource {
        /// The missing path.
        path: PathBuf,
    },
    /// The renderer is not supported.
    #[error("unsupported renderer: {0}")]
    Unsupported(String),
    /// A host address override is invalid.
    #[error("invalid chart repository host {value:?}: {message}")]
    Host {
        /// The rejected host.
        value: String,
        /// The problem.
        message: String,
    },
    /// The chart repository HTTP server could not bind its socket.
    #[error("failed to start the chart repository server on {address}: {source}")]
    Listen {
        /// The bind address.
        address: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A generated value is invalid.
    #[error(transparent)]
    ValueError(#[from] ValueError),
}

impl RenderError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Renders one app into a packaged chart.
pub trait ChartRenderer: Send + Sync {
    /// Renders and packages an app.
    fn render(&self, request: RenderRequest) -> BoxFuture<'_, Result<RenderedApp, RenderError>>;
}

/// The production renderer set: Helm charts and Kustomize wrapper charts.
pub struct Renderers {
    helm: HelmRenderer,
    kustomize: KustomizeRenderer,
}

impl Renderers {
    /// Creates a renderer set that uses the ambient Helm configuration.
    #[must_use]
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            helm: HelmRenderer::new(Arc::clone(&runner)),
            kustomize: KustomizeRenderer::new(runner),
        }
    }

    /// Isolates Helm repositories and caches in `home`.
    #[must_use]
    pub fn with_helm_home(mut self, home: HelmHome) -> Self {
        self.helm = self.helm.with_home(home);
        self
    }

    /// Replaces the retry policy used for `helm dependency build`.
    #[must_use]
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.helm = self.helm.with_retry_policy(policy);
        self
    }
}

impl ChartRenderer for Renderers {
    fn render(&self, request: RenderRequest) -> BoxFuture<'_, Result<RenderedApp, RenderError>> {
        Box::pin(async move {
            match request.renderer {
                RendererKind::Helm => self.helm.render(request).await,
                RendererKind::Kustomize => self.kustomize.render(request).await,
            }
        })
    }
}

fn resolve_values_file(
    manifest: &Manifest,
    chart_source: &std::path::Path,
    path: &std::path::Path,
) -> Result<PathBuf, RenderError> {
    let from_root = manifest.resolve_path(path)?;
    if from_root.exists() || path.is_absolute() {
        return Ok(from_root);
    }
    let from_chart = chart_source.join(path);
    if from_chart.exists() {
        Ok(from_chart)
    } else {
        Ok(from_root)
    }
}

fn is_kustomization_dir(path: &std::path::Path) -> bool {
    ["kustomization.yaml", "kustomization.yml", "Kustomization"]
        .iter()
        .any(|name| path.join(name).is_file())
}

/// Splits rendered YAML into Kubernetes objects.
///
/// Empty documents are ignored. Every non-empty document must be a mapping
/// with string `apiVersion` and `kind` fields. A `kind: List` document is
/// expanded so that the result counts individual objects.
///
/// # Errors
///
/// Returns [`RenderError::Yaml`] when a document is not valid YAML and
/// [`RenderError::Values`] when a document is not a Kubernetes object.
pub fn split_kubernetes_documents(source: &str) -> Result<Vec<serde_json::Value>, RenderError> {
    use serde::Deserialize as _;

    let mut documents = Vec::new();
    for document in yaml_serde::Deserializer::from_str(source) {
        let value = serde_json::Value::deserialize(document)?;
        collect_documents(value, &mut documents)?;
    }
    Ok(documents)
}

fn collect_documents(
    value: serde_json::Value,
    documents: &mut Vec<serde_json::Value>,
) -> Result<(), RenderError> {
    match value {
        serde_json::Value::Null => Ok(()),
        serde_json::Value::Object(object) => {
            let kind = object
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| RenderError::Values {
                    path: "<rendered output>".to_owned(),
                    message: "a Kubernetes object needs a string `kind`".to_owned(),
                })?;
            let api_version = object
                .get("apiVersion")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| RenderError::Values {
                    path: "<rendered output>".to_owned(),
                    message: format!("{kind} needs a string `apiVersion`"),
                })?;
            if api_version.trim().is_empty() {
                return Err(RenderError::Values {
                    path: "<rendered output>".to_owned(),
                    message: format!("{kind} has an empty `apiVersion`"),
                });
            }

            if kind == "List" {
                let items = object
                    .get("items")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| RenderError::Values {
                        path: "<rendered output>".to_owned(),
                        message: "a List document needs an array `items`".to_owned(),
                    })?;
                for item in items {
                    collect_documents(item.clone(), documents)?;
                }
                Ok(())
            } else {
                documents.push(serde_json::Value::Object(object));
                Ok(())
            }
        }
        other => Err(RenderError::Values {
            path: "<rendered output>".to_owned(),
            message: format!(
                "expected a Kubernetes object mapping, found {}",
                json_type(&other)
            ),
        }),
    }
}

fn json_type(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "a sequence",
        serde_json::Value::Object(_) => "a mapping",
    }
}

/// Counts Kubernetes objects in rendered YAML.
///
/// # Errors
///
/// Returns the same errors as [`split_kubernetes_documents`].
pub fn manifest_count(source: &str) -> Result<usize, RenderError> {
    Ok(split_kubernetes_documents(source)?.len())
}

/// Computes a stable SHA-256 content hash from labelled byte chunks.
pub(crate) fn content_hash<'a>(
    parts: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<ContentHash, RenderError> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    for (label, bytes) in parts {
        hasher.update(label.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    let hash = hasher.finalize();
    let mut encoded = String::with_capacity(hash.len() * 2);
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    Ok(ContentHash::new(encoded)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_validates_documents() {
        let source = r"
# a comment
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: demo
---
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: demo
";
        let documents = split_kubernetes_documents(source).unwrap();
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0]["kind"], "ConfigMap");
        assert_eq!(documents[1]["kind"], "Deployment");
    }

    #[test]
    fn expands_list_documents() {
        let source = r"
apiVersion: v1
kind: List
items:
  - apiVersion: v1
    kind: ConfigMap
    metadata:
      name: one
  - apiVersion: v1
    kind: ConfigMap
    metadata:
      name: two
";
        assert_eq!(manifest_count(source).unwrap(), 2);
    }

    #[test]
    fn rejects_non_kubernetes_documents() {
        let error = split_kubernetes_documents("apiVersion: v1\nmetadata: {}\n").unwrap_err();
        assert!(matches!(error, RenderError::Values { .. }));
        let error = split_kubernetes_documents("- one\n- two\n").unwrap_err();
        assert!(matches!(error, RenderError::Values { .. }));
    }

    #[test]
    fn request_resolves_chart_relative_values_and_kind_overlays() {
        let directory = tempfile::tempdir().expect("tempdir");
        let chart = directory.path().join("chart");
        std::fs::create_dir_all(chart.join("templates")).expect("chart");
        std::fs::write(
            chart.join("Chart.yaml"),
            "apiVersion: v2\nname: chart\nversion: 0.1.0\n",
        )
        .expect("Chart.yaml");
        std::fs::write(chart.join("values-kind.yaml"), "replicas: 1\n").expect("values");
        let overlay = directory.path().join("overlay/overlays/kind");
        std::fs::create_dir_all(&overlay).expect("overlay");
        std::fs::write(
            overlay.join("kustomization.yaml"),
            "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources: []\n",
        )
        .expect("kustomization");
        std::fs::write(directory.path().join("common.yaml"), "global: {}\n").expect("common");

        let source = r#"
[project]
name = "test"
root = "."

[telemetry]
enabled = false

[registry]
enabled = false
mirrors = []

[[projects]]
name = "chart"
source_repositories = ["*"]
destinations = [{ namespace = "chart" }]

[[projects]]
name = "overlay"
source_repositories = ["*"]
destinations = [{ namespace = "overlay" }]

[[namespaces]]
name = "chart"

[[namespaces]]
name = "overlay"

[[apps]]
name = "chart"
namespace = "chart"
source = "chart"
common_values = "common.yaml"
values = ["values-kind.yaml"]

[[apps]]
name = "overlay"
namespace = "overlay"
source = "overlay"
renderer = "kustomize"
"#;
        let manifest = Manifest::from_toml_str(source, directory.path()).expect("manifest");
        let request = RenderRequest::from_manifest(
            &manifest,
            manifest.app("chart").expect("chart app"),
            directory.path().join("out"),
        )
        .expect("request");
        assert_eq!(request.values_files, vec![chart.join("values-kind.yaml")]);
        assert_eq!(
            request.common_values,
            Some(directory.path().join("common.yaml"))
        );

        let kustomize = RenderRequest::from_manifest(
            &manifest,
            manifest.app("overlay").expect("overlay app"),
            directory.path().join("out"),
        )
        .expect("request");
        assert_eq!(kustomize.source, overlay);
    }

    #[test]
    fn content_hash_is_stable_and_changes_with_input() {
        let first = content_hash([("values", b"{}".as_slice())]).unwrap();
        let second = content_hash([("values", b"{}".as_slice())]).unwrap();
        let third = content_hash([("values", b"{\"a\":1}".as_slice())]).unwrap();
        assert_eq!(first, second);
        assert_ne!(first, third);
        assert_eq!(first.as_str().len(), 64);
    }
}
