//! The local Helm chart repository served to Argo CD.
//!
//! A chart repository is a directory of `.tgz` files plus a generated
//! `index.yaml`. Astrarium binds an ephemeral TCP port, writes the index with
//! the reachable host address, and serves exactly that directory read-only.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use url::Url;

use crate::BoxFuture;
use crate::command::{CommandRunner, CommandSpec, RealCommandRunner, Tool};
use crate::manifest::Manifest;
use crate::render::RenderError;

/// The environment variable that overrides the chart repository host.
pub const HOST_IP_ENV: &str = "ASTRARIUM_HOST_IP";

/// A chart repository served over HTTP for Argo CD.
#[derive(Debug)]
pub struct ServedChartRepository {
    /// The repository URL, for example `http://192.0.2.10:41234`.
    pub url: String,
    /// The host address embedded in [`Self::url`].
    pub host: IpAddr,
    /// The bound TCP port.
    pub port: u16,
    /// The directory served by the repository.
    pub directory: PathBuf,
    /// A cancellation token that stops the server.
    pub shutdown: CancellationToken,
}

impl ServedChartRepository {
    /// Stops the HTTP server.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Returns the URL of a packaged chart in this repository.
    #[must_use]
    pub fn chart_url(&self, chart: &str, version: &str) -> String {
        format!("{}/{chart}-{version}.tgz", self.url)
    }
}

/// Starts a chart repository server.
pub trait ChartRepositoryServer: Send + Sync {
    /// Indexes `directory` for the bound URL and serves it on an ephemeral
    /// port.
    ///
    /// Call this after every chart has been packaged: `helm repo index` is run
    /// once and later packages are not added to `index.yaml` automatically.
    fn serve(
        &self,
        directory: PathBuf,
    ) -> BoxFuture<'_, Result<ServedChartRepository, RenderError>>;
}

/// The production chart repository server.
pub struct HttpChartRepository {
    runner: Arc<dyn CommandRunner>,
    host: Option<String>,
    bind: SocketAddr,
    helm_program: String,
    timeout: Duration,
}

impl HttpChartRepository {
    /// Creates a repository server that uses the ambient Helm executable.
    #[must_use]
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            host: None,
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            helm_program: Tool::Helm.default_program().to_owned(),
            timeout: Duration::from_secs(300),
        }
    }

    /// Builds a server from manifest render settings.
    ///
    /// The Helm executable and render timeout are taken from the manifest. A
    /// non-`auto` `registry.host` is used as the advertised host, which lets a
    /// suite pin the address kind nodes must use; otherwise
    /// `ASTRARIUM_HOST_IP` and automatic detection apply.
    #[must_use]
    pub fn for_manifest(runner: Arc<dyn CommandRunner>, manifest: &Manifest) -> Self {
        let mut server = Self::new(runner)
            .with_helm_program(manifest.render.helm.clone())
            .with_timeout(manifest.render.timeout.get());
        if !manifest.registry.host.eq_ignore_ascii_case("auto")
            && !manifest.registry.host.trim().is_empty()
        {
            server = server.with_host(manifest.registry.host.clone());
        }
        server
    }

    /// Overrides the host address advertised to Argo CD.
    ///
    /// Pass the manifest's registry or chart repository host here. The
    /// `ASTRARIUM_HOST_IP` environment variable is used when this is unset.
    #[must_use]
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Overrides the bind address. The default is `0.0.0.0:0`.
    #[must_use]
    pub fn with_bind(mut self, bind: SocketAddr) -> Self {
        self.bind = bind;
        self
    }

    /// Overrides the Helm executable used for `helm repo index`.
    #[must_use]
    pub fn with_helm_program(mut self, program: impl Into<String>) -> Self {
        self.helm_program = program.into();
        self
    }

    /// Overrides the `helm repo index` timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Writes `index.yaml` for `http://host:port`.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] when `helm repo index` fails.
    pub async fn index(&self, directory: &Path, url: &str) -> Result<(), RenderError> {
        let command = CommandSpec::with_program(Tool::Helm, &self.helm_program)
            .args(["repo", "index", &directory.display().to_string()])
            .arg("--url")
            .arg(url)
            .timeout(self.timeout);
        self.runner.run(command).await?;
        Ok(())
    }

    /// Serves a directory without rewriting `index.yaml`.
    ///
    /// This is useful for callers that already generated the index and for
    /// tests that do not want to invoke Helm.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] when the directory cannot be resolved or the
    /// socket cannot be bound.
    pub async fn serve_static(
        &self,
        directory: &Path,
    ) -> Result<ServedChartRepository, RenderError> {
        std::fs::create_dir_all(directory).map_err(|source| RenderError::io(directory, source))?;
        let root = std::fs::canonicalize(directory)
            .map_err(|source| RenderError::io(directory, source))?;
        let listener =
            TcpListener::bind(self.bind)
                .await
                .map_err(|source| RenderError::Listen {
                    address: self.bind.to_string(),
                    source,
                })?;
        let address = listener
            .local_addr()
            .map_err(|source| RenderError::Listen {
                address: self.bind.to_string(),
                source,
            })?;
        let host = resolve_host(self.host.as_deref())?;
        let url = format_url(host, address.port())?;
        let shutdown = spawn_server(listener, root.clone());
        debug!(%url, directory = %root.display(), "serving Helm chart repository");
        Ok(ServedChartRepository {
            url,
            host,
            port: address.port(),
            directory: root,
            shutdown,
        })
    }

    /// Binds, writes `index.yaml` for the bound URL and serves the directory.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] when the directory cannot be resolved, the
    /// socket cannot be bound, or `helm repo index` fails.
    pub async fn serve_with_index(
        &self,
        directory: &Path,
    ) -> Result<ServedChartRepository, RenderError> {
        std::fs::create_dir_all(directory).map_err(|source| RenderError::io(directory, source))?;
        let root = std::fs::canonicalize(directory)
            .map_err(|source| RenderError::io(directory, source))?;
        let listener =
            TcpListener::bind(self.bind)
                .await
                .map_err(|source| RenderError::Listen {
                    address: self.bind.to_string(),
                    source,
                })?;
        let address = listener
            .local_addr()
            .map_err(|source| RenderError::Listen {
                address: self.bind.to_string(),
                source,
            })?;
        let host = resolve_host(self.host.as_deref())?;
        let url = format_url(host, address.port())?;
        self.index(&root, &url).await?;
        let shutdown = spawn_server(listener, root.clone());
        debug!(%url, directory = %root.display(), "serving indexed Helm chart repository");
        Ok(ServedChartRepository {
            url,
            host,
            port: address.port(),
            directory: root,
            shutdown,
        })
    }
}

impl Default for HttpChartRepository {
    fn default() -> Self {
        Self::new(Arc::new(RealCommandRunner))
    }
}

impl ChartRepositoryServer for HttpChartRepository {
    fn serve(
        &self,
        directory: PathBuf,
    ) -> BoxFuture<'_, Result<ServedChartRepository, RenderError>> {
        Box::pin(async move { self.serve_with_index(&directory).await })
    }
}

/// Resolves the host address advertised to Argo CD.
///
/// The order is: the explicit value, `ASTRARIUM_HOST_IP`, the default-route
/// address reported by `local-ip-address`, then `127.0.0.1`. An explicit value
/// that is not an IP address is an error; the loopback fallback is only used
/// when automatic detection fails.
///
/// # Errors
///
/// Returns [`RenderError::Host`] when an explicit host is not an IP address.
pub fn resolve_host(explicit: Option<&str>) -> Result<IpAddr, RenderError> {
    if let Some(value) = explicit.and_then(usable_host) {
        return parse_host(value);
    }
    if let Ok(value) = std::env::var(HOST_IP_ENV)
        && let Some(value) = usable_host(&value)
    {
        return parse_host(value);
    }
    match local_ip_address::local_ip() {
        Ok(address) => Ok(address),
        Err(error) => {
            debug!(%error, "could not determine the host address; using loopback");
            Ok(IpAddr::V4(Ipv4Addr::LOCALHOST))
        }
    }
}

fn usable_host(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(value)
    }
}

fn parse_host(value: &str) -> Result<IpAddr, RenderError> {
    value.parse().map_err(|_| RenderError::Host {
        value: value.to_owned(),
        message: "expected an IPv4 or IPv6 address".to_owned(),
    })
}

/// Formats a repository URL.
///
/// # Errors
///
/// Returns [`RenderError::Host`] when the host or port cannot be represented
/// in an HTTP URL.
pub fn format_url(host: IpAddr, port: u16) -> Result<String, RenderError> {
    let host = match host {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    };
    let mut url = Url::parse("http://localhost").map_err(|error| RenderError::Host {
        value: host.clone(),
        message: error.to_string(),
    })?;
    url.set_host(Some(&host))
        .map_err(|error| RenderError::Host {
            value: host.clone(),
            message: error.to_string(),
        })?;
    url.set_port(Some(port)).map_err(|()| RenderError::Host {
        value: host,
        message: "invalid port".to_owned(),
    })?;
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

#[derive(Clone)]
struct RepositoryState {
    root: Arc<PathBuf>,
}

fn spawn_server(listener: TcpListener, root: PathBuf) -> CancellationToken {
    let shutdown = CancellationToken::new();
    let token = shutdown.clone();
    let state = RepositoryState {
        root: Arc::new(root),
    };
    let router = Router::new()
        .route("/", get(index_file))
        .route("/{*path}", get(repository_file))
        .with_state(state);

    tokio::spawn(async move {
        let result = axum::serve(listener, router)
            .with_graceful_shutdown(async move { token.cancelled().await })
            .await;
        if let Err(error) = result {
            warn!(%error, "chart repository server stopped with an error");
        }
    });
    shutdown
}

async fn index_file(State(state): State<RepositoryState>) -> Response {
    read_repository_file(&state.root, Path::new("index.yaml")).await
}

async fn repository_file(
    State(state): State<RepositoryState>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    let Some(relative) = sanitize_relative_path(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    read_repository_file(&state.root, &relative).await
}

fn sanitize_relative_path(value: &str) -> Option<PathBuf> {
    let mut relative = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            // Reject absolute paths, `.` and any `..` traversal outright.
            _ => return None,
        }
    }
    if relative.as_os_str().is_empty() {
        None
    } else {
        Some(relative)
    }
}

async fn read_repository_file(root: &Path, relative: &Path) -> Response {
    let candidate = root.join(relative);
    let Ok(canonical) = tokio::fs::canonicalize(&candidate).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !canonical.starts_with(root) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(metadata) = tokio::fs::metadata(&canonical).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !metadata.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(contents) = tokio::fs::read(&canonical).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut response = Response::new(Body::from(contents));
    *response.status_mut() = StatusCode::OK;
    if let Ok(value) = HeaderValue::from_str(content_type(&canonical)) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("yaml" | "yml") => "text/yaml; charset=utf-8",
        Some("tgz") => "application/gzip",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serves_only_the_repository_directory() {
        let parent = tempfile::tempdir().expect("tempdir");
        let directory = parent.path().join("repository");
        std::fs::create_dir_all(&directory).expect("repository");
        std::fs::write(
            directory.join("index.yaml"),
            "apiVersion: v1\nentries: {}\n",
        )
        .expect("index");
        std::fs::write(directory.join("demo-0.1.0.tgz"), b"chart bytes").expect("chart");
        std::fs::write(parent.path().join("outside.yaml"), "secret: true\n").expect("outside");

        let server = HttpChartRepository::default()
            .with_host("127.0.0.1")
            .serve_static(&directory)
            .await
            .expect("server");
        assert_eq!(server.host, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_ne!(server.port, 0);
        assert_eq!(server.url, format!("http://127.0.0.1:{}", server.port));

        let client = reqwest::Client::new();
        let index = client
            .get(format!("{}/index.yaml", server.url))
            .send()
            .await
            .expect("index request");
        assert_eq!(index.status(), reqwest::StatusCode::OK);
        assert!(
            index
                .text()
                .await
                .expect("index body")
                .contains("apiVersion: v1")
        );

        let chart = client
            .get(server.chart_url("demo", "0.1.0"))
            .send()
            .await
            .expect("chart request");
        assert_eq!(chart.status(), reqwest::StatusCode::OK);
        assert_eq!(chart.headers()[header::CONTENT_TYPE], "application/gzip");
        assert_eq!(
            chart.bytes().await.expect("chart body").as_ref(),
            b"chart bytes"
        );

        let root = client
            .get(format!("{}/", server.url))
            .send()
            .await
            .expect("root request");
        assert_eq!(root.status(), reqwest::StatusCode::OK);

        let traversal = client
            .get(format!("{}/%2e%2e%2foutside.yaml", server.url))
            .send()
            .await
            .expect("traversal request");
        assert_eq!(traversal.status(), reqwest::StatusCode::NOT_FOUND);

        let missing = client
            .get(format!("{}/missing.yaml", server.url))
            .send()
            .await
            .expect("missing request");
        assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

        server.shutdown();
    }

    #[test]
    fn sanitizes_relative_paths() {
        assert_eq!(
            sanitize_relative_path("charts/demo-0.1.0.tgz"),
            Some(PathBuf::from("charts/demo-0.1.0.tgz"))
        );
        assert!(sanitize_relative_path("../index.yaml").is_none());
        assert!(sanitize_relative_path("/etc/passwd").is_none());
        assert!(sanitize_relative_path("./index.yaml").is_none());
        assert!(sanitize_relative_path("").is_none());
    }

    #[tokio::test]
    async fn indexes_the_directory_with_the_bound_url() {
        let directory = tempfile::tempdir().expect("tempdir");
        let runner = std::sync::Arc::new(crate::render::testing::FakeCommandRunner::new());
        let server = HttpChartRepository::new(runner.clone())
            .with_host("127.0.0.1")
            .serve_with_index(directory.path())
            .await
            .expect("server");
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].args,
            vec![
                "repo".to_owned(),
                "index".to_owned(),
                server.directory.display().to_string(),
                "--url".to_owned(),
                server.url.clone(),
            ]
        );
        server.shutdown();
    }

    #[test]
    fn formats_ipv4_and_ipv6_urls() {
        assert_eq!(
            format_url(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 8080).expect("ipv4 URL"),
            "http://192.0.2.10:8080"
        );
        assert_eq!(
            format_url("::1".parse().expect("ipv6"), 8080).expect("ipv6 URL"),
            "http://[::1]:8080"
        );
    }

    #[test]
    fn explicit_hosts_must_be_ip_addresses() {
        assert_eq!(
            resolve_host(Some("127.0.0.1")).expect("host"),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        assert!(resolve_host(Some("example.test")).is_err());
        assert_eq!(
            resolve_host(Some("auto")).expect("auto"),
            resolve_host(None).expect("auto")
        );
    }
}
