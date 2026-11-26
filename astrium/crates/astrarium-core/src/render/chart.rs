//! Helm chart metadata and repository discovery.

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use url::Url;

use crate::manifest::HelmRepository;
use crate::render::RenderError;
use crate::types::ChartVersion;

/// The metadata Astrarium reads from `Chart.yaml`.
///
/// Helm charts contain many more fields; unknown fields are ignored so newer
/// chart API versions keep working.
#[derive(Clone, Debug, Deserialize)]
pub struct ChartMetadata {
    /// The chart API version, for example `v2`.
    #[serde(rename = "apiVersion", default)]
    pub api_version: Option<String>,
    /// The chart name.
    pub name: String,
    /// The chart version as written in `Chart.yaml`.
    #[serde(default)]
    pub version: Option<String>,
    /// Chart dependencies.
    #[serde(default)]
    pub dependencies: Vec<ChartDependency>,
}

/// A Helm chart dependency.
#[derive(Clone, Debug, Deserialize)]
pub struct ChartDependency {
    /// The dependency chart name.
    pub name: String,
    /// The dependency version constraint.
    #[serde(default)]
    pub version: Option<String>,
    /// The repository URL, `file://` URL or `oci://` reference.
    #[serde(default)]
    pub repository: Option<String>,
    /// An optional alias.
    #[serde(default)]
    pub alias: Option<String>,
}

impl ChartMetadata {
    /// Loads `Chart.yaml` from a chart directory.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Chart`] when the file cannot be read or parsed.
    pub fn load(chart_directory: &Path) -> Result<Self, RenderError> {
        let path = chart_directory.join("Chart.yaml");
        let source = std::fs::read_to_string(&path).map_err(|source| RenderError::Io {
            path: path.clone(),
            source,
        })?;
        let metadata: Self = yaml_serde::from_str(&source).map_err(|error| RenderError::Chart {
            path: path.clone(),
            message: error.to_string(),
        })?;
        if metadata.name.trim().is_empty() {
            return Err(RenderError::Chart {
                path,
                message: "the chart name must not be empty".to_owned(),
            });
        }
        Ok(metadata)
    }

    /// Returns the chart's semantic version.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Chart`] when `version` is missing or is not a
    /// semantic version.
    pub fn version(&self, chart_directory: &Path) -> Result<ChartVersion, RenderError> {
        let version = self.version.as_deref().ok_or_else(|| RenderError::Chart {
            path: chart_directory.join("Chart.yaml"),
            message: "the chart has no `version`".to_owned(),
        })?;
        // Helm tolerates a leading `v` in Chart.yaml; the packaged chart and
        // Argo CD targetRevision use the canonical semantic version.
        let normalized = version.strip_prefix('v').unwrap_or(version);
        normalized
            .parse()
            .map_err(|error: semver::Error| RenderError::Chart {
                path: chart_directory.join("Chart.yaml"),
                message: format!("version {version:?} is not a semantic version: {error}"),
            })
    }

    /// Returns repository URLs declared by the chart dependencies.
    #[must_use]
    pub fn dependency_repositories(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.dependencies
            .iter()
            .filter_map(|dependency| dependency.repository.as_deref())
            .filter(|repository| {
                Url::parse(repository).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
                })
            })
            .filter(|repository| seen.insert((*repository).to_owned()))
            .map(ToOwned::to_owned)
            .collect()
    }
}

/// Returns the repositories that must be registered before a dependency
/// build.
///
/// Manifest repositories are returned first and keep their configured names.
/// Repository URLs used directly by chart dependencies are added afterwards
/// with deterministic generated names, because Helm 4 resolves direct URLs
/// from the local repository cache. `file://` and `oci://` dependencies need
/// no HTTP repository. URLs already covered by a manifest repository are not
/// duplicated.
#[must_use]
pub fn repositories_for_chart(
    metadata: &ChartMetadata,
    declared: &[HelmRepository],
) -> Vec<HelmRepository> {
    let declared_urls = declared
        .iter()
        .map(|repository| repository.url.as_str())
        .collect::<HashSet<_>>();

    let mut repositories = declared.to_vec();
    for url in metadata.dependency_repositories() {
        if declared_urls.contains(url.as_str()) {
            continue;
        }
        repositories.push(HelmRepository {
            name: generated_repository_name(&url),
            url,
        });
    }
    repositories
}

fn generated_repository_name(url: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(url.as_bytes());
    let mut suffix = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write as _;
        let _ = write!(suffix, "{byte:02x}");
    }
    format!("astrarium-{suffix}")
}

/// Returns the version to use when the request has no explicit override.
pub(super) fn resolve_version(
    metadata: &ChartMetadata,
    chart_directory: &Path,
    requested: Option<&ChartVersion>,
) -> Result<ChartVersion, RenderError> {
    match requested {
        Some(version) => Ok(version.clone()),
        None => metadata.version(chart_directory),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_metadata_and_dependency_repositories() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            directory.path().join("Chart.yaml"),
            r"
apiVersion: v2
name: demo
version: 1.2.3
dependencies:
  - name: first
    version: 1.0.0
    repository: https://charts.example.test
  - name: local
    version: 1.0.0
    repository: file://../local
  - name: oci
    version: 1.0.0
    repository: oci://registry.example.test/charts
  - name: first-again
    version: 1.0.0
    repository: https://charts.example.test
",
        )
        .expect("Chart.yaml");

        let metadata = ChartMetadata::load(directory.path()).expect("metadata");
        assert_eq!(metadata.name, "demo");
        assert_eq!(
            metadata.version(directory.path()).unwrap().to_string(),
            "1.2.3"
        );
        assert_eq!(
            metadata.dependency_repositories(),
            vec!["https://charts.example.test".to_owned()]
        );
    }

    #[test]
    fn normalizes_a_leading_v_in_the_chart_version() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            directory.path().join("Chart.yaml"),
            "apiVersion: v2\nname: demo\nversion: v1.2.3\n",
        )
        .expect("Chart.yaml");
        let metadata = ChartMetadata::load(directory.path()).expect("metadata");
        assert_eq!(
            metadata
                .version(directory.path())
                .expect("version")
                .to_string(),
            "1.2.3"
        );
    }

    #[test]
    fn declared_repositories_win_over_generated_names() {
        let metadata = ChartMetadata {
            api_version: Some("v2".to_owned()),
            name: "demo".to_owned(),
            version: Some("0.1.0".to_owned()),
            dependencies: vec![ChartDependency {
                name: "dep".to_owned(),
                version: None,
                repository: Some("https://charts.example.test".to_owned()),
                alias: None,
            }],
        };
        let declared = vec![HelmRepository {
            name: "example".to_owned(),
            url: "https://charts.example.test".to_owned(),
        }];
        let repositories = repositories_for_chart(&metadata, &declared);
        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0].name, "example");

        let generated = repositories_for_chart(&metadata, &[]);
        assert_eq!(generated.len(), 1);
        assert!(generated[0].name.starts_with("astrarium-"));
        assert_eq!(generated[0].url, "https://charts.example.test");
    }
}
