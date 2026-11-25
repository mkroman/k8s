//! Kustomize rendering through a generated wrapper chart.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::BoxFuture;
use crate::command::{CommandRunner, CommandSpec, Tool};
use crate::manifest::RendererKind;
use crate::render::{
    ChartRenderer, RenderError, RenderRequest, RenderedApp, content_hash, default_chart_version,
    manifest_count,
};
use crate::types::{AppName, ChartVersion};

/// The template file that copies the pre-rendered manifests verbatim.
///
/// `Files.Get` returns the file contents as a string; Helm renders that string
/// as part of the wrapper template without evaluating it as a template again.
pub const WRAPPER_CHART_TEMPLATE: &str = "{{ .Files.Get \"manifests.yaml\" }}";

const WRAPPER_MANIFESTS: &str = "manifests.yaml";
const WRAPPER_TEMPLATE_DIRECTORY: &str = "templates";
const WRAPPER_TEMPLATE_FILE: &str = "manifests.yaml";

/// Returns the wrapper chart name for an app.
#[must_use]
pub fn wrapper_chart_name(app: &AppName) -> String {
    format!("astrarium-{app}")
}

/// A generated wrapper chart on disk.
#[derive(Clone, Debug)]
pub struct WrapperChart {
    /// The chart directory.
    pub directory: PathBuf,
    /// The chart name.
    pub name: String,
    /// The chart version.
    pub version: ChartVersion,
    /// The number of Kubernetes objects in `manifests.yaml`.
    pub manifest_count: usize,
}

/// Writes a wrapper chart containing already rendered Kubernetes manifests.
///
/// The wrapper contains:
///
/// - `Chart.yaml` with the `astrarium-<app>` name and an exact semantic
///   version;
/// - `manifests.yaml` with the rendered YAML;
/// - `templates/manifests.yaml` containing exactly
///   [`WRAPPER_CHART_TEMPLATE`].
///
/// # Errors
///
/// Returns [`RenderError`] when the manifests are not valid Kubernetes YAML or
/// the files cannot be written.
pub fn write_wrapper_chart(
    directory: &Path,
    app: &AppName,
    version: &ChartVersion,
    manifests: &str,
) -> Result<WrapperChart, RenderError> {
    let count = manifest_count(manifests)?;
    let name = wrapper_chart_name(app);
    let template_directory = directory.join(WRAPPER_TEMPLATE_DIRECTORY);
    std::fs::create_dir_all(&template_directory)
        .map_err(|source| RenderError::io(&template_directory, source))?;

    let chart_yaml = format!(
        "apiVersion: v2\nname: {name}\ndescription: Rendered Kustomize manifests for {app}\ntype: application\nversion: {version}\n"
    );
    write_file(&directory.join("Chart.yaml"), chart_yaml.as_bytes())?;
    write_file(&directory.join(WRAPPER_MANIFESTS), manifests.as_bytes())?;
    write_file(
        &template_directory.join(WRAPPER_TEMPLATE_FILE),
        WRAPPER_CHART_TEMPLATE.as_bytes(),
    )?;

    Ok(WrapperChart {
        directory: directory.to_path_buf(),
        name,
        version: version.clone(),
        manifest_count: count,
    })
}

fn write_file(path: &Path, contents: &[u8]) -> Result<(), RenderError> {
    std::fs::write(path, contents).map_err(|source| RenderError::io(path, source))
}

/// Renders Kustomize directories and wraps the result in a Helm chart.
pub struct KustomizeRenderer {
    runner: Arc<dyn CommandRunner>,
}

impl KustomizeRenderer {
    /// Creates a Kustomize renderer.
    #[must_use]
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    async fn render_inner(&self, request: &RenderRequest) -> Result<RenderedApp, RenderError> {
        let build = CommandSpec::with_program(Tool::Kustomize, &request.kustomize_program)
            .args(["build", &request.source.display().to_string()])
            .args(request.kustomize_args.iter().cloned())
            .timeout(request.timeout);
        let rendered = self.runner.run(build).await?;

        let version = request
            .chart_version
            .clone()
            .unwrap_or_else(default_chart_version);
        let work = tempfile::Builder::new()
            .prefix("astrarium-wrapper-")
            .tempdir()
            .map_err(|source| RenderError::io(std::env::temp_dir(), source))?;
        let wrapper = write_wrapper_chart(
            &work.path().join("wrapper"),
            &request.app,
            &version,
            &rendered.stdout,
        )?;

        std::fs::create_dir_all(&request.output_dir)
            .map_err(|source| RenderError::io(request.output_dir.clone(), source))?;
        let output_dir = std::fs::canonicalize(&request.output_dir)
            .map_err(|source| RenderError::io(request.output_dir.clone(), source))?;
        let package = CommandSpec::with_program(Tool::Helm, &request.helm_program)
            .args(["package", &wrapper.directory.display().to_string()])
            .arg("--destination")
            .arg(output_dir.display().to_string())
            .arg("--version")
            .arg(version.to_string())
            .timeout(request.timeout);
        self.runner.run(package).await?;

        let chart_path = output_dir.join(format!("{}-{version}.tgz", wrapper.name));
        let hash_parts = [
            ("chart-name".to_owned(), wrapper.name.clone().into_bytes()),
            ("chart-version".to_owned(), version.to_string().into_bytes()),
            ("rendered".to_owned(), rendered.stdout.as_bytes().to_vec()),
        ];
        let hash = content_hash(
            hash_parts
                .iter()
                .map(|(label, contents)| (label.as_str(), contents.as_slice())),
        )?;

        Ok(RenderedApp {
            app: request.app.clone(),
            renderer: RendererKind::Kustomize,
            chart_name: wrapper.name,
            chart_version: version,
            chart_path,
            values: serde_json::Value::Object(serde_json::Map::new()),
            content_hash: hash,
            manifest_count: Some(wrapper.manifest_count),
        })
    }
}

impl ChartRenderer for KustomizeRenderer {
    fn render(&self, request: RenderRequest) -> BoxFuture<'_, Result<RenderedApp, RenderError>> {
        Box::pin(async move { self.render_inner(&request).await })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::manifest::RendererKind;
    use crate::render::testing::{FakeCommandRunner, request, version};

    const MANIFESTS: &str = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: one\n---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: two\n";

    #[test]
    fn wrapper_chart_contains_raw_manifests() {
        let directory = tempfile::tempdir().expect("tempdir");
        let app = AppName::new("demo").expect("app");
        let version = version("0.3.0");
        let wrapper =
            write_wrapper_chart(&directory.path().join("wrapper"), &app, &version, MANIFESTS)
                .expect("wrapper");

        assert_eq!(wrapper.name, "astrarium-demo");
        assert_eq!(wrapper.version.to_string(), "0.3.0");
        assert_eq!(wrapper.manifest_count, 2);
        assert_eq!(
            std::fs::read_to_string(wrapper.directory.join("Chart.yaml")).expect("Chart.yaml"),
            "apiVersion: v2\nname: astrarium-demo\ndescription: Rendered Kustomize manifests for demo\ntype: application\nversion: 0.3.0\n"
        );
        assert_eq!(
            std::fs::read_to_string(wrapper.directory.join("manifests.yaml")).expect("manifests"),
            MANIFESTS
        );
        assert_eq!(
            std::fs::read_to_string(wrapper.directory.join("templates/manifests.yaml"))
                .expect("template"),
            "{{ .Files.Get \"manifests.yaml\" }}"
        );
        assert_eq!(
            WRAPPER_CHART_TEMPLATE,
            "{{ .Files.Get \"manifests.yaml\" }}"
        );
    }

    #[tokio::test]
    async fn renders_and_packages_a_wrapper() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("overlay");
        std::fs::create_dir_all(&source).expect("overlay");
        let output = directory.path().join("out");
        let runner = Arc::new(FakeCommandRunner::new());
        runner.push_output_for("build", MANIFESTS);
        let renderer = KustomizeRenderer::new(runner.clone());

        let mut render_request = request("demo", &source, &output);
        render_request.renderer = RendererKind::Kustomize;
        render_request.kustomize_args = vec!["--load-restrictor".to_owned(), "none".to_owned()];
        let chart_output = renderer.render(render_request).await.expect("rendered");

        assert_eq!(chart_output.chart_name, "astrarium-demo");
        assert_eq!(chart_output.chart_version.to_string(), "0.0.0");
        assert_eq!(chart_output.manifest_count, Some(2));
        assert!(
            chart_output
                .values
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        );
        assert_eq!(
            chart_output
                .chart_path
                .file_name()
                .and_then(std::ffi::OsStr::to_str),
            Some("astrarium-demo-0.0.0.tgz")
        );

        let calls = runner.calls();
        let build = calls
            .iter()
            .find(|call| call.args.first().is_some_and(|arg| arg == "build"))
            .expect("build call");
        assert!(
            build
                .args
                .iter()
                .any(|argument| argument == "--load-restrictor")
        );
        let package = calls
            .iter()
            .find(|call| call.args.first().is_some_and(|arg| arg == "package"))
            .expect("package call");
        assert!(package.args.iter().any(|argument| argument == "0.0.0"));
    }
}
