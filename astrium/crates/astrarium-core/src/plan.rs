//! Suite planning and change selection.
//!
//! A plan is the set of apps a run should deploy. Apps can be selected
//! explicitly, as the whole suite, or from the current Git change set. Change
//! selection is implemented in the private `selection` module with `gix`; it never shells out to
//! `git`.

mod selection;

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::manifest::{Manifest, ManifestError};
use crate::types::{AppName, Revision, ValueError};

/// Why an app is part of a plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum SelectionReason {
    /// The app was selected on the command line.
    Explicit,
    /// Every app in the suite was selected.
    All,
    /// A file under the app's source, watch, or values paths changed.
    Changed(Vec<PathBuf>),
    /// A path shared by every app changed.
    SharedChanged(Vec<PathBuf>),
    /// The app failed, was cancelled or was skipped in a previous run.
    PreviousRun(PreviousRunStatus),
    /// The app is a prerequisite of another selected app.
    DependencyOf(AppName),
}

impl fmt::Display for SelectionReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit => formatter.write_str("explicit"),
            Self::All => formatter.write_str("all apps"),
            Self::Changed(paths) => write!(formatter, "changed ({})", display_paths(paths)),
            Self::SharedChanged(paths) => {
                write!(formatter, "shared change ({})", display_paths(paths))
            }
            Self::PreviousRun(status) => write!(formatter, "previous run {}", status.as_str()),
            Self::DependencyOf(app) => write!(formatter, "required by {app}"),
        }
    }
}

fn display_paths(paths: &[PathBuf]) -> String {
    const MAX_PATHS: usize = 3;
    let mut parts = paths
        .iter()
        .take(MAX_PATHS)
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if paths.len() > MAX_PATHS {
        parts.push(format!("+{} more", paths.len() - MAX_PATHS));
    }
    parts.join(", ")
}

/// The result of an app in a previous run, as reported by CI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviousRunStatus {
    /// The app passed.
    Success,
    /// The app failed.
    Failure,
    /// The app was cancelled.
    Cancelled,
    /// The app was skipped.
    Skipped,
    /// The CI provider reported a status Astrarium does not know.
    #[serde(other)]
    Unknown,
}

impl PreviousRunStatus {
    /// Returns true when a previous result should force a rerun.
    #[must_use]
    pub fn needs_rerun(self) -> bool {
        !matches!(self, Self::Success)
    }

    /// Returns the status as a lowercase word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::Unknown => "unknown",
        }
    }
}

/// Previous-run results supplied by CI.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreviousResults {
    /// The head revision of the previous run.
    pub head_sha: Option<String>,
    /// The per-app results.
    #[serde(default)]
    pub apps: BTreeMap<String, PreviousRunStatus>,
}

/// A request to select apps.
#[derive(Clone, Debug, Default)]
pub struct SelectionRequest {
    /// The explicit diff base, if any.
    pub base: Option<Revision>,
    /// The head revision, if any. Defaults to `HEAD`.
    pub head: Option<Revision>,
    /// Path to the previous-run JSON.
    pub previous_results: Option<PathBuf>,
    /// Explicit apps selected on the command line.
    pub apps: Vec<AppName>,
    /// Select every app.
    pub all: bool,
}

/// An app in a plan, with the reason it was selected and its sync wave.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedApp {
    /// The app name.
    pub name: AppName,
    /// Why the app is in the plan.
    pub reason: SelectionReason,
    /// The sync wave derived from dependency depth.
    pub wave: i32,
    /// The app's direct prerequisites.
    pub requires: Vec<AppName>,
}

/// The selected suite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The diff base, when known. `None` means the diff was taken against an
    /// empty tree, for example for a repository's first commit.
    pub base: Option<Revision>,
    /// The head revision.
    pub head: Revision,
    /// The apps in dependency order (prerequisites first).
    pub apps: Vec<PlannedApp>,
}

impl Plan {
    /// Returns the app names in dependency order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.apps.iter().map(|app| app.name.as_str()).collect()
    }

    /// Returns true when at least one app is selected.
    #[must_use]
    pub fn has_apps(&self) -> bool {
        !self.apps.is_empty()
    }

    /// Renders the GitHub Actions matrix.
    #[must_use]
    pub fn matrix(&self) -> serde_json::Value {
        let include: Vec<serde_json::Value> = self
            .apps
            .iter()
            .map(|app| serde_json::json!({"app": app.name.as_str()}))
            .collect();
        serde_json::json!({ "include": include })
    }

    /// Renders the plan and its matrix as JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if the plan cannot be serialized.
    pub fn to_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        let mut value = serde_json::to_value(self)?;
        value["matrix"] = self.matrix();
        value["has_apps"] = serde_json::Value::Bool(self.has_apps());
        Ok(value)
    }

    /// Renders the plan as a human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut output = String::new();
        output.push_str("Astrarium plan\n");
        match &self.base {
            Some(base) => output.push_str(&format!("  base: {base}\n")),
            None => output.push_str("  base: (empty tree)\n"),
        }
        output.push_str(&format!("  head: {}\n", self.head));
        if self.apps.is_empty() {
            output.push_str("  apps: none\n");
            return output;
        }
        output.push_str(&format!(
            "  apps: {} (in dependency order)\n",
            self.apps.len()
        ));
        for app in &self.apps {
            output.push_str(&format!(
                "    {} (wave {}, {})\n",
                app.name, app.wave, app.reason
            ));
        }
        output
    }

    /// Renders the GitHub Actions outputs (`matrix` and `has_apps`).
    #[must_use]
    pub fn github_output(&self) -> String {
        let matrix = serde_json::to_string(&self.matrix())
            .unwrap_or_else(|_| r#"{"include":[]}"#.to_owned());
        format!("matrix={matrix}\nhas_apps={}\n", self.has_apps())
    }

    /// Renders a GitHub Actions step summary in Markdown.
    #[must_use]
    pub fn step_summary(&self) -> String {
        let mut output = String::new();
        output.push_str("## Astrarium plan\n\n");
        match &self.base {
            Some(base) => output.push_str(&format!("- Base: `{base}`\n")),
            None => output.push_str("- Base: empty tree\n"),
        }
        output.push_str(&format!("- Head: `{}`\n", self.head));

        if self.apps.is_empty() {
            output.push_str("\nNo apps selected.\n");
            return output;
        }

        output.push_str(&format!("\n{} app(s) selected:\n\n", self.apps.len()));
        output.push_str("| App | Wave | Reason |\n");
        output.push_str("| --- | ---: | --- |\n");
        for app in &self.apps {
            let reason = app
                .reason
                .to_string()
                .replace('|', "\\|")
                .replace('\n', " ");
            output.push_str(&format!("| {} | {} | {} |\n", app.name, app.wave, reason));
        }
        output
    }
}

/// An error while selecting apps.
#[derive(Debug, Error)]
pub enum SelectionError {
    /// The manifest is invalid.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A semantic value, such as a resolved revision, is invalid.
    #[error(transparent)]
    Value(#[from] ValueError),
    /// A requested app is not declared.
    #[error("unknown app {0:?}")]
    UnknownApp(String),
    /// `--all` and `--app` were both supplied.
    #[error("select either every app or specific apps, not both")]
    ConflictingSelection,
    /// The previous-run JSON could not be read.
    #[error("failed to read previous results {path}: {source}")]
    PreviousResults {
        /// The JSON path.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The previous-run JSON is invalid.
    #[error("invalid previous results: {0}")]
    PreviousResultsJson(#[from] serde_json::Error),
    /// A Git operation failed.
    #[error("git operation failed: {0}")]
    Git(String),
    /// The blocking selection task panicked or was cancelled.
    #[error("selection task failed: {0}")]
    Join(String),
    /// A GitHub Actions output file could not be written.
    #[error("failed to write {what} {path}: {source}")]
    Output {
        /// What was being written, for example `GitHub output`.
        what: &'static str,
        /// The output path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Builds a plan containing every app.
///
/// # Errors
///
/// Returns [`SelectionError::Manifest`] for an invalid dependency graph.
pub fn select_all(manifest: &Manifest, head: Revision) -> Result<Plan, SelectionError> {
    let selected = manifest
        .apps
        .iter()
        .map(|app| (app.name.clone(), SelectionReason::All))
        .collect();
    build_plan(manifest, selected, None, head)
}

/// Builds a plan for explicitly selected apps and their prerequisites.
///
/// # Errors
///
/// Returns [`SelectionError::UnknownApp`] for an undeclared app and
/// [`SelectionError::Manifest`] for an invalid dependency graph.
pub fn select_explicit(
    manifest: &Manifest,
    selected: &[AppName],
    head: Revision,
) -> Result<Plan, SelectionError> {
    for app in selected {
        if manifest.app(app.as_str()).is_none() {
            return Err(SelectionError::UnknownApp(app.to_string()));
        }
    }
    let selected = selected
        .iter()
        .map(|name| (name.clone(), SelectionReason::Explicit))
        .collect();
    build_plan(manifest, selected, None, head)
}

/// Selects apps synchronously. This performs blocking Git I/O and must not be
/// called from an async task without [`tokio::task::spawn_blocking`].
///
/// # Errors
///
/// Returns [`SelectionError`] when the request is invalid, the repository
/// cannot be read, or the manifest dependency graph is invalid.
pub fn select_blocking(
    manifest: &Manifest,
    request: &SelectionRequest,
) -> Result<Plan, SelectionError> {
    if request.all && !request.apps.is_empty() {
        return Err(SelectionError::ConflictingSelection);
    }
    if request.all {
        return select_all(manifest, head_or_default(request));
    }
    if !request.apps.is_empty() {
        return select_explicit(manifest, &request.apps, head_or_default(request));
    }
    selection::select_changes(manifest, request)
}

/// Selects apps without blocking the async runtime.
///
/// Explicit and whole-suite selections are resolved immediately. Change
/// selection runs the `gix` work in [`tokio::task::spawn_blocking`].
///
/// # Errors
///
/// Returns [`SelectionError`] when the request is invalid, the repository
/// cannot be read, or the manifest dependency graph is invalid.
pub async fn select(
    manifest: &Manifest,
    request: &SelectionRequest,
) -> Result<Plan, SelectionError> {
    if request.all || !request.apps.is_empty() {
        return select_blocking(manifest, request);
    }
    let manifest = manifest.clone();
    let request = request.clone();
    tokio::task::spawn_blocking(move || select_blocking(&manifest, &request))
        .await
        .map_err(|error| SelectionError::Join(error.to_string()))?
}

/// Writes `matrix` and `has_apps` to a GitHub Actions output file, appending
/// when the file already exists.
///
/// # Errors
///
/// Returns [`SelectionError::Output`] when the file cannot be written.
pub fn write_github_output(plan: &Plan, path: impl AsRef<Path>) -> Result<(), SelectionError> {
    append_output(path.as_ref(), &plan.github_output(), "GitHub output")
}

/// Appends a Markdown plan summary to a GitHub Actions step summary file.
///
/// # Errors
///
/// Returns [`SelectionError::Output`] when the file cannot be written.
pub fn append_step_summary(plan: &Plan, path: impl AsRef<Path>) -> Result<(), SelectionError> {
    let summary = format!("\n{}\n", plan.step_summary());
    append_output(path.as_ref(), &summary, "GitHub step summary")
}

fn append_output(path: &Path, contents: &str, what: &'static str) -> Result<(), SelectionError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| SelectionError::Output {
            what,
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(contents.as_bytes())
        .map_err(|source| SelectionError::Output {
            what,
            path: path.to_path_buf(),
            source,
        })
}

fn head_or_default(request: &SelectionRequest) -> Revision {
    request
        .head
        .clone()
        .unwrap_or_else(|| Revision::new("HEAD").unwrap_or_else(|_| unreachable!("HEAD is valid")))
}

/// Adds transitive prerequisites and orders the selected apps.
pub(crate) fn build_plan(
    manifest: &Manifest,
    selected: Vec<(AppName, SelectionReason)>,
    base: Option<Revision>,
    head: Revision,
) -> Result<Plan, SelectionError> {
    let mut reasons: BTreeMap<AppName, SelectionReason> = selected.into_iter().collect();
    let mut queue: VecDeque<AppName> = manifest
        .apps
        .iter()
        .filter(|app| reasons.contains_key(&app.name))
        .map(|app| app.name.clone())
        .collect();

    while let Some(name) = queue.pop_front() {
        let spec = manifest
            .app(name.as_str())
            .ok_or_else(|| SelectionError::UnknownApp(name.to_string()))?;
        for requirement in &spec.requires {
            if !reasons.contains_key(requirement) {
                reasons.insert(
                    requirement.clone(),
                    SelectionReason::DependencyOf(name.clone()),
                );
                queue.push_back(requirement.clone());
            }
        }
    }

    let selected_names: Vec<AppName> = manifest
        .apps
        .iter()
        .filter(|app| reasons.contains_key(&app.name))
        .map(|app| app.name.clone())
        .collect();
    let ordered = manifest.dependency_order(&selected_names)?;

    let mut apps = Vec::with_capacity(ordered.len());
    for name in ordered {
        let spec = manifest
            .app(name.as_str())
            .ok_or_else(|| SelectionError::UnknownApp(name.to_string()))?;
        let reason = reasons
            .remove(&name)
            .ok_or_else(|| SelectionError::UnknownApp(name.to_string()))?;
        let depth = manifest.depth(&name)?;
        apps.push(PlannedApp {
            name,
            reason,
            wave: sync_wave(depth),
            requires: spec.requires.clone(),
        });
    }

    Ok(Plan { base, head, apps })
}

fn sync_wave(depth: u32) -> i32 {
    -10 * i32::try_from(depth).unwrap_or(i32::MAX / 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan(apps: Vec<PlannedApp>) -> Plan {
        Plan {
            base: None,
            head: Revision::new("abc").unwrap(),
            apps,
        }
    }

    fn sample_app(name: &str, reason: SelectionReason) -> PlannedApp {
        PlannedApp {
            name: AppName::new(name).unwrap(),
            reason,
            wave: 0,
            requires: Vec::new(),
        }
    }

    #[test]
    fn matrix_shape_is_stable() {
        let plan = sample_plan(vec![sample_app("alpha", SelectionReason::All)]);
        assert_eq!(
            plan.matrix(),
            serde_json::json!({"include": [{"app": "alpha"}]})
        );
    }

    #[test]
    fn empty_matrix_is_empty_include() {
        let plan = sample_plan(Vec::new());
        assert_eq!(plan.matrix(), serde_json::json!({"include": []}));
        assert!(!plan.has_apps());
        assert!(plan.github_output().contains("has_apps=false"));
        assert!(plan.github_output().contains(r#"matrix={"include":[]}"#));
        assert!(plan.step_summary().contains("No apps selected."));
    }

    #[test]
    fn github_output_and_step_summary_include_apps() {
        let plan = sample_plan(vec![sample_app(
            "alpha",
            SelectionReason::Changed(vec![PathBuf::from("alpha/Chart.yaml")]),
        )]);
        let output = plan.github_output();
        assert!(output.contains(r#"matrix={"include":[{"app":"alpha"}]}"#));
        assert!(output.contains("has_apps=true"));
        let summary = plan.step_summary();
        assert!(summary.contains("| alpha | 0 | changed (alpha/Chart.yaml) |"));
        assert!(summary.contains("1 app(s) selected"));
    }

    #[test]
    fn outputs_append_to_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("github-output");
        let summary = dir.path().join("summary.md");
        std::fs::write(&output, "existing=1\n").unwrap();
        std::fs::write(&summary, "# previous\n").unwrap();

        let plan = sample_plan(vec![sample_app("alpha", SelectionReason::All)]);
        write_github_output(&plan, &output).unwrap();
        append_step_summary(&plan, &summary).unwrap();

        let output = std::fs::read_to_string(&output).unwrap();
        assert!(output.starts_with("existing=1\n"));
        assert!(output.contains("has_apps=true"));
        let summary = std::fs::read_to_string(&summary).unwrap();
        assert!(summary.starts_with("# previous\n"));
        assert!(summary.contains("## Astrarium plan"));
    }

    #[test]
    fn json_envelope_contains_the_matrix() {
        let plan = sample_plan(vec![sample_app("alpha", SelectionReason::All)]);
        let value = plan.to_json().unwrap();
        assert_eq!(
            value["matrix"],
            serde_json::json!({"include": [{"app": "alpha"}]})
        );
        assert_eq!(value["apps"][0]["name"], "alpha");
    }

    #[test]
    fn previous_run_status_parses_unknown_values() {
        let results: PreviousResults =
            serde_json::from_str(r#"{"head_sha": "abc", "apps": {"alpha": "timed_out"}}"#).unwrap();
        assert_eq!(results.apps["alpha"], PreviousRunStatus::Unknown);
        assert!(results.apps["alpha"].needs_rerun());
        assert!(!PreviousRunStatus::Success.needs_rerun());
    }
}
