//! Helm chart rendering.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracing::{debug, warn};

use crate::BoxFuture;
use crate::command::{CommandError, CommandOutput, CommandRunner, CommandSpec, Tool};
use crate::manifest::RendererKind;
use crate::render::chart::{ChartMetadata, repositories_for_chart, resolve_version};
use crate::render::{
    ChartRenderer, RenderError, RenderRequest, RenderedApp, content_hash, manifest_count,
    merged_values,
};
use crate::types::ContentHash;

/// The default number of `helm dependency build` attempts.
pub const DEFAULT_DEPENDENCY_ATTEMPTS: u32 = 5;

/// Bounded exponential retry settings for `helm dependency build`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The total number of attempts, including the first one.
    pub attempts: u32,
    /// The delay before the second attempt.
    pub initial_backoff: Duration,
    /// The upper bound for a single delay.
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: DEFAULT_DEPENDENCY_ATTEMPTS,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(8),
        }
    }
}

impl RetryPolicy {
    /// Creates a retry policy.
    #[must_use]
    pub const fn new(attempts: u32, initial_backoff: Duration, max_backoff: Duration) -> Self {
        Self {
            attempts,
            initial_backoff,
            max_backoff,
        }
    }

    /// Returns the delay before attempt `attempt` (1 is the first attempt).
    #[must_use]
    pub fn backoff(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(16);
        let multiplier = 1_u32 << exponent;
        self.initial_backoff
            .saturating_mul(multiplier)
            .min(self.max_backoff)
    }
}

/// An isolated Helm repository configuration and cache.
///
/// Astrarium registers chart dependency repositories in this home instead of
/// the user's `~/.config/helm`, so a test run does not mutate the ambient Helm
/// configuration. Sharing one home across renders keeps dependency downloads
/// warm within a run.
#[derive(Debug)]
pub struct HelmHome {
    root: PathBuf,
    _temporary: Option<TempDir>,
}

impl HelmHome {
    /// Creates a temporary Helm home that is removed on drop.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Io`] when the temporary directory cannot be
    /// created.
    pub fn temporary() -> Result<Self, RenderError> {
        let temporary = tempfile::Builder::new()
            .prefix("astrarium-helm-")
            .tempdir()
            .map_err(|source| RenderError::io(std::env::temp_dir(), source))?;
        let root = temporary.path().to_path_buf();
        Ok(Self {
            root,
            _temporary: Some(temporary),
        })
    }

    /// Uses `root` as a persistent Helm home.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            _temporary: None,
        }
    }

    /// Returns the home's root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn environment(&self) -> Result<Vec<(String, String)>, RenderError> {
        let repository = self.root.join("repository");
        let cache = self.root.join("cache");
        for path in [&self.root, &repository, &cache] {
            std::fs::create_dir_all(path)
                .map_err(|source| RenderError::io(path.clone(), source))?;
        }
        Ok(vec![
            (
                "HELM_REPOSITORY_CONFIG".to_owned(),
                self.root.join("repositories.yaml").display().to_string(),
            ),
            (
                "HELM_REPOSITORY_CACHE".to_owned(),
                repository.display().to_string(),
            ),
            ("HELM_CACHE_HOME".to_owned(), cache.display().to_string()),
        ])
    }
}

/// Renders Helm charts by copying them to a temporary directory, building
/// dependencies and packaging them.
pub struct HelmRenderer {
    runner: Arc<dyn CommandRunner>,
    retry: RetryPolicy,
    home: Option<HelmHome>,
}

impl HelmRenderer {
    /// Creates a renderer that uses the ambient Helm repository configuration.
    #[must_use]
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            retry: RetryPolicy::default(),
            home: None,
        }
    }

    /// Uses an isolated Helm repository configuration and cache.
    #[must_use]
    pub fn with_home(mut self, home: HelmHome) -> Self {
        self.home = Some(home);
        self
    }

    /// Replaces the `helm dependency build` retry policy.
    #[must_use]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    async fn render_inner(&self, request: &RenderRequest) -> Result<RenderedApp, RenderError> {
        let work = tempfile::Builder::new()
            .prefix("astrarium-chart-")
            .tempdir()
            .map_err(|source| RenderError::io(std::env::temp_dir(), source))?;
        let chart_directory = work.path().join("chart");
        copy_directory(&request.source, &chart_directory)?;
        let metadata = ChartMetadata::load(&chart_directory)?;

        let environment = match &self.home {
            Some(home) => home.environment()?,
            None => Vec::new(),
        };

        // Load values before touching the network so a bad values file fails
        // fast instead of after a dependency build.
        let values = merged_values(
            request.common_values.as_deref(),
            &request.values_files,
            &request.inline_values,
        )?;
        let values_bytes = serde_json::to_vec(&values)?;
        let values_path = work.path().join("values.json");
        std::fs::write(&values_path, &values_bytes)
            .map_err(|source| RenderError::io(values_path.clone(), source))?;

        self.register_repositories(request, &metadata, &environment)
            .await?;
        self.dependency_build(request, &chart_directory, &environment)
            .await?;

        let template_output = if request.dry_run {
            Some(
                self.template(request, &chart_directory, &values_path, &environment)
                    .await?,
            )
        } else {
            None
        };

        let version = resolve_version(&metadata, &chart_directory, request.chart_version.as_ref())?;
        std::fs::create_dir_all(&request.output_dir)
            .map_err(|source| RenderError::io(request.output_dir.clone(), source))?;
        let output_dir = std::fs::canonicalize(&request.output_dir)
            .map_err(|source| RenderError::io(request.output_dir.clone(), source))?;

        let mut package = CommandSpec::with_program(Tool::Helm, &request.helm_program)
            .args(["package", &chart_directory.display().to_string()])
            .arg("--destination")
            .arg(output_dir.display().to_string())
            .arg("--version")
            .arg(version.to_string())
            .timeout(request.timeout);
        for (key, value) in &environment {
            package = package.env(key.clone(), value.clone());
        }
        self.runner.run(package).await?;

        let chart_path = output_dir.join(format!("{}-{version}.tgz", metadata.name));
        let chart_files = read_tree(&chart_directory)?;
        let mut parts = vec![
            ("chart-name".to_owned(), metadata.name.as_bytes().to_vec()),
            ("chart-version".to_owned(), version.to_string().into_bytes()),
            ("values".to_owned(), values_bytes),
        ];
        parts.extend(
            chart_files
                .into_iter()
                .map(|(path, contents)| (format!("chart:{path}"), contents)),
        );
        if let Some(output) = &template_output {
            parts.push(("rendered".to_owned(), output.as_bytes().to_vec()));
        }
        let hash = render_hash(&parts)?;

        Ok(RenderedApp {
            app: request.app.clone(),
            renderer: RendererKind::Helm,
            chart_name: metadata.name,
            chart_version: version,
            chart_path,
            values,
            content_hash: hash,
            manifest_count: template_output.as_deref().map(manifest_count).transpose()?,
        })
    }

    async fn register_repositories(
        &self,
        request: &RenderRequest,
        metadata: &ChartMetadata,
        environment: &[(String, String)],
    ) -> Result<(), RenderError> {
        let repositories = repositories_for_chart(metadata, &request.repositories);
        for repository in repositories {
            let is_declared = request
                .repositories
                .iter()
                .any(|candidate| candidate.url == repository.url);
            debug!(
                repository = %repository.name,
                url = %repository.url,
                "registering Helm repository"
            );
            let mut command = CommandSpec::with_program(Tool::Helm, &request.helm_program)
                .args([
                    "repo",
                    "add",
                    &repository.name,
                    &repository.url,
                    "--force-update",
                ])
                // Repository index downloads are small; do not let a black-holed
                // endpoint consume the full render timeout on every retry.
                .timeout(request.timeout.min(Duration::from_secs(30)));
            for (key, value) in environment {
                command = command.env(key.clone(), value.clone());
            }
            match self.run_with_retries(command).await {
                Ok(_) => {}
                Err(error) if is_declared => return Err(error),
                Err(error) => {
                    // Helm can still download a direct URL dependency when the
                    // repository index cannot be registered; the dependency
                    // build below decides whether that was fatal.
                    warn!(
                        repository = %repository.name,
                        %error,
                        "could not register inferred Helm repository"
                    );
                }
            }
        }
        Ok(())
    }

    async fn dependency_build(
        &self,
        request: &RenderRequest,
        chart_directory: &Path,
        environment: &[(String, String)],
    ) -> Result<(), RenderError> {
        let mut command = CommandSpec::with_program(Tool::Helm, &request.helm_program)
            .args([
                "dependency",
                "build",
                &chart_directory.display().to_string(),
            ])
            .timeout(request.timeout);
        for (key, value) in environment {
            command = command.env(key.clone(), value.clone());
        }
        self.run_with_retries(command).await.map(|_| ())
    }

    async fn template(
        &self,
        request: &RenderRequest,
        chart_directory: &Path,
        values_path: &Path,
        environment: &[(String, String)],
    ) -> Result<String, RenderError> {
        let mut command = CommandSpec::with_program(Tool::Helm, &request.helm_program)
            .args(["template", &request.release_name])
            .arg(chart_directory.display().to_string())
            .arg("--namespace")
            .arg(request.namespace.as_str())
            .arg("--values")
            .arg(values_path.display().to_string())
            .arg("--include-crds")
            .timeout(request.timeout);
        for (key, value) in environment {
            command = command.env(key.clone(), value.clone());
        }
        let output = self.runner.run(command).await?;
        Ok(output.stdout)
    }

    async fn run_with_retries(&self, command: CommandSpec) -> Result<CommandOutput, RenderError> {
        let attempts = self.retry.attempts.max(1);
        let mut attempt = 1;
        loop {
            match self.runner.run(command.clone()).await {
                Ok(output) => return Ok(output),
                Err(error) if attempt < attempts && is_retryable(&error) => {
                    let delay = self.retry.backoff(attempt);
                    warn!(
                        attempt,
                        attempts,
                        delay = ?delay,
                        %error,
                        "Helm command failed; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(error) => return Err(RenderError::Command(error)),
            }
        }
    }
}

impl ChartRenderer for HelmRenderer {
    fn render(&self, request: RenderRequest) -> BoxFuture<'_, Result<RenderedApp, RenderError>> {
        Box::pin(async move { self.render_inner(&request).await })
    }
}

fn is_retryable(error: &CommandError) -> bool {
    matches!(
        error,
        CommandError::Failed { .. } | CommandError::Timeout { .. }
    )
}

fn render_hash(parts: &[(String, Vec<u8>)]) -> Result<ContentHash, RenderError> {
    content_hash(
        parts
            .iter()
            .map(|(label, contents)| (label.as_str(), contents.as_slice())),
    )
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), RenderError> {
    std::fs::create_dir_all(destination).map_err(|error| RenderError::io(destination, error))?;
    let mut entries = std::fs::read_dir(source)
        .map_err(|error| RenderError::io(source, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| RenderError::io(source, error))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| RenderError::io(&source_path, error))?;
        if file_type.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else {
            // `fs::copy` follows symlinks. Chart templates use regular files
            // in practice; a symlinked directory fails with a clear I/O error.
            std::fs::copy(&source_path, &destination_path)
                .map_err(|error| RenderError::io(&source_path, error))?;
        }
    }
    Ok(())
}

fn read_tree(root: &Path) -> Result<Vec<(String, Vec<u8>)>, RenderError> {
    fn walk(
        root: &Path,
        directory: &Path,
        files: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<(), RenderError> {
        let mut entries = std::fs::read_dir(directory)
            .map_err(|error| RenderError::io(directory, error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| RenderError::io(directory, error))?;
        entries.sort_by_key(std::fs::DirEntry::file_name);

        for entry in entries {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| RenderError::io(&path, error))?;
            if file_type.is_dir() {
                walk(root, &path, files)?;
            } else if file_type.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                let contents =
                    std::fs::read(&path).map_err(|error| RenderError::io(&path, error))?;
                files.push((relative, contents));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::render::testing::{FakeCommandRunner, request, transient_error, version};

    fn chart(directory: &Path, extra: &str) {
        std::fs::create_dir_all(directory.join("templates")).expect("templates");
        std::fs::write(
            directory.join("Chart.yaml"),
            format!("apiVersion: v2\nname: demo\nversion: 0.1.0\n{extra}"),
        )
        .expect("Chart.yaml");
        std::fs::write(
            directory.join("templates/configmap.yaml"),
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: demo\n",
        )
        .expect("template");
    }

    #[tokio::test]
    async fn dependency_build_is_retried_then_packaged() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("chart");
        chart(&source, "");
        let output = directory.path().join("out");

        let runner = Arc::new(FakeCommandRunner::new());
        runner.push_error_for("dependency", transient_error());
        runner.push_error_for("dependency", transient_error());
        let renderer = HelmRenderer::new(runner.clone()).with_retry_policy(RetryPolicy::new(
            5,
            Duration::ZERO,
            Duration::ZERO,
        ));
        let mut render_request = request("demo", &source, &output);
        render_request.dry_run = false;

        let chart_output = renderer
            .render(render_request)
            .await
            .expect("rendered chart");
        assert_eq!(runner.count_calls_containing("dependency"), 3);
        assert_eq!(chart_output.chart_name, "demo");
        assert_eq!(chart_output.chart_version.to_string(), "0.1.0");
        assert_eq!(
            chart_output
                .chart_path
                .file_name()
                .and_then(std::ffi::OsStr::to_str),
            Some("demo-0.1.0.tgz")
        );
        let package = runner
            .calls()
            .into_iter()
            .find(|call| call.args.first().is_some_and(|arg| arg == "package"))
            .expect("package call");
        assert!(package.args.iter().any(|argument| argument == "0.1.0"));
    }

    #[tokio::test]
    async fn registers_dependency_repositories() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("chart");
        chart(
            &source,
            "dependencies:\n  - name: dep\n    version: 1.0.0\n    repository: https://charts.example.test\n",
        );
        let output = directory.path().join("out");
        let runner = Arc::new(FakeCommandRunner::new());
        let renderer = HelmRenderer::new(runner.clone());
        let mut render_request = request("demo", &source, &output);
        render_request.dry_run = false;
        renderer.render(render_request).await.expect("rendered");

        let repository = runner
            .calls()
            .into_iter()
            .find(|call| call.args.first().is_some_and(|arg| arg == "repo"))
            .expect("repo add call");
        assert!(
            repository
                .args
                .iter()
                .any(|argument| argument.contains("--force-update"))
        );
        assert!(
            repository
                .args
                .iter()
                .any(|argument| argument == "https://charts.example.test")
        );
    }

    #[tokio::test]
    async fn template_output_sets_the_manifest_count() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("chart");
        chart(&source, "");
        let output = directory.path().join("out");
        let runner = Arc::new(FakeCommandRunner::new());
        runner.push_output_for(
            "template",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: one\n---\napiVersion: v1\nkind: Service\nmetadata:\n  name: two\n",
        );
        let renderer = HelmRenderer::new(runner);
        let mut render_request = request("demo", &source, &output);
        render_request.inline_values = serde_json::json!({"enabled": true, "replicas": 2});
        let chart_output = renderer.render(render_request).await.expect("rendered");
        assert_eq!(chart_output.manifest_count, Some(2));
        assert_eq!(chart_output.values["enabled"], true);
        assert_eq!(chart_output.values["replicas"], 2);
    }

    #[tokio::test]
    async fn rejects_non_mapping_values() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("chart");
        chart(&source, "");
        let values = directory.path().join("values.yaml");
        std::fs::write(&values, "- not\n- a mapping\n").expect("values");
        let output = directory.path().join("out");
        let runner = Arc::new(FakeCommandRunner::new());
        let renderer = HelmRenderer::new(runner);
        let mut render_request = request("demo", &source, &output);
        render_request.dry_run = false;
        render_request.values_files = vec![values];
        let error = renderer.render(render_request).await.unwrap_err();
        assert!(matches!(error, RenderError::Values { .. }), "{error}");
    }

    #[test]
    fn retry_backoff_is_bounded() {
        let policy = RetryPolicy::new(5, Duration::from_millis(500), Duration::from_secs(8));
        assert_eq!(policy.backoff(1), Duration::from_millis(500));
        assert_eq!(policy.backoff(2), Duration::from_secs(1));
        assert_eq!(policy.backoff(3), Duration::from_secs(2));
        assert_eq!(policy.backoff(4), Duration::from_secs(4));
        assert_eq!(policy.backoff(5), Duration::from_secs(8));
        assert_eq!(policy.backoff(6), Duration::from_secs(8));
    }

    #[test]
    fn explicit_versions_override_the_chart_version() {
        let metadata = ChartMetadata {
            api_version: Some("v2".to_owned()),
            name: "demo".to_owned(),
            version: Some("9.9.9".to_owned()),
            dependencies: Vec::new(),
        };
        let requested = version("1.2.3");
        assert_eq!(
            resolve_version(&metadata, Path::new("chart"), Some(&requested))
                .expect("version")
                .to_string(),
            "1.2.3"
        );
        assert_eq!(
            resolve_version(&metadata, Path::new("chart"), None)
                .expect("version")
                .to_string(),
            "9.9.9"
        );
    }
}
