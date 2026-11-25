//! Test doubles shared by the render modules.
//!
//! The command runner and transient error live in the core `testing` module
//! (re-exported by `astrarium-test-support`) so other crates can use the same
//! fakes. The request and version helpers stay here because they are specific
//! to the render tests.

pub use crate::testing::{FakeCommandRunner, transient_error};

use std::time::Duration;

use crate::types::ChartVersion;

/// Returns a render request for a fixture app.
#[must_use]
pub fn request(
    app: &str,
    source: &std::path::Path,
    output: &std::path::Path,
) -> crate::render::RenderRequest {
    use crate::manifest::RendererKind;
    use crate::types::{AppName, NamespaceName};

    crate::render::RenderRequest {
        app: AppName::new(app).expect("app name"),
        source: source.to_path_buf(),
        renderer: RendererKind::Helm,
        common_values: None,
        values_files: Vec::new(),
        inline_values: serde_json::Value::Object(serde_json::Map::new()),
        output_dir: output.to_path_buf(),
        chart_version: None,
        release_name: app.to_owned(),
        namespace: NamespaceName::new("default").expect("namespace"),
        dry_run: true,
        helm_program: "helm".to_owned(),
        kustomize_program: "kustomize".to_owned(),
        kustomize_args: Vec::new(),
        repositories: Vec::new(),
        timeout: Duration::from_secs(30),
    }
}

/// Parses a chart version in tests without repeating the error handling.
#[must_use]
pub fn version(value: &str) -> ChartVersion {
    value.parse().expect("chart version")
}
