//! Chart rendering and the local Helm chart repository.

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

use crate::BoxFuture;
use crate::manifest::RendererKind;
use crate::types::{AppName, ChartVersion, ContentHash, NamespaceName};

/// A request to render one app into a Helm chart.
#[derive(Clone, Debug)]
pub struct RenderRequest {
    /// The app name.
    pub app: AppName,
    /// The chart or Kustomize source directory.
    pub source: PathBuf,
    /// The renderer.
    pub renderer: RendererKind,
    /// Helm values files, merged in order.
    pub values_files: Vec<PathBuf>,
    /// Inline values, merged last.
    pub inline_values: serde_json::Value,
    /// The directory that receives the packaged chart.
    pub output_dir: PathBuf,
    /// The packaged chart version.
    pub chart_version: ChartVersion,
    /// The Helm release name.
    pub release_name: String,
    /// The namespace used for rendering.
    pub namespace: NamespaceName,
    /// The `helm` executable.
    pub helm_program: String,
    /// The `kustomize` executable.
    pub kustomize_program: String,
    /// The timeout for each external command.
    pub timeout: Duration,
}

/// A rendered and packaged chart.
#[derive(Clone, Debug)]
pub struct RenderedApp {
    /// The app name.
    pub app: AppName,
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
    /// A Helm chart is malformed.
    #[error("invalid Helm chart at {path}: {message}")]
    Chart {
        /// The chart path.
        path: PathBuf,
        /// The problem.
        message: String,
    },
    /// The renderer is not supported.
    #[error("unsupported renderer: {0}")]
    Unsupported(String),
    /// A generated value is invalid.
    #[error(transparent)]
    ValueError(#[from] crate::types::ValueError),
}

/// Renders one app into a packaged chart.
pub trait ChartRenderer: Send + Sync {
    /// Renders and packages an app.
    fn render(&self, request: RenderRequest) -> BoxFuture<'_, Result<RenderedApp, RenderError>>;
}

/// A chart repository served over HTTP for Argo CD.
#[derive(Debug)]
pub struct ServedChartRepository {
    /// The repository URL.
    pub url: String,
    /// The directory served by the repository.
    pub directory: PathBuf,
    /// A cancellation token that stops the server.
    pub shutdown: tokio_util::sync::CancellationToken,
}

impl ServedChartRepository {
    /// Stops the HTTP server.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

/// Starts a chart repository server.
pub trait ChartRepositoryServer: Send + Sync {
    /// Serves `directory` on an ephemeral port.
    fn serve(
        &self,
        directory: PathBuf,
    ) -> BoxFuture<'_, Result<ServedChartRepository, RenderError>>;
}
