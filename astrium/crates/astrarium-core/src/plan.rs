//! Suite planning and change selection.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::manifest::{Manifest, ManifestError};
use crate::types::{AppName, Revision};

/// Why an app is part of a plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum SelectionReason {
    /// The app was selected on the command line.
    Explicit,
    /// Every app in the suite was selected.
    All,
    /// A file under the app's source or values changed.
    Changed(Vec<PathBuf>),
    /// A shared file changed, so every app is selected.
    SharedChanged(Vec<PathBuf>),
    /// The app failed, was cancelled or was skipped in a previous run.
    PreviousRun(PreviousRunStatus),
    /// The app is a prerequisite of another selected app.
    DependencyOf(AppName),
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
    /// The CI provider reported an unknown status.
    Unknown,
}

impl PreviousRunStatus {
    /// Returns true when a previous result should force a rerun.
    #[must_use]
    pub fn needs_rerun(self) -> bool {
        !matches!(self, Self::Success)
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
    /// The diff base, when known.
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

    /// Renders the plan as a human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut output = String::new();
        output.push_str("Astrarium plan\n");
        if let Some(base) = &self.base {
            output.push_str(&format!("  base: {base}\n"));
        }
        output.push_str(&format!("  head: {}\n", self.head));
        if self.apps.is_empty() {
            output.push_str("  apps: none\n");
            return output;
        }
        output.push_str("  apps:\n");
        for app in &self.apps {
            let reason = match &app.reason {
                SelectionReason::Explicit => "explicit".to_owned(),
                SelectionReason::All => "all".to_owned(),
                SelectionReason::Changed(paths) => {
                    format!(
                        "changed ({})",
                        paths
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
                SelectionReason::SharedChanged(paths) => {
                    format!(
                        "shared change ({})",
                        paths
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
                SelectionReason::PreviousRun(status) => format!("previous run: {status:?}"),
                SelectionReason::DependencyOf(app) => format!("required by {app}"),
            };
            output.push_str(&format!("    {} (wave {}, {reason})\n", app.name, app.wave));
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
    /// A requested app is not declared.
    #[error("unknown app {0:?}")]
    UnknownApp(String),
    /// A feature required for this request is not implemented yet.
    #[error("selection is not implemented yet: {0}")]
    Unsupported(&'static str),
    /// The previous-run JSON could not be read or parsed.
    #[error("failed to read previous results {path}: {source}")]
    PreviousResults {
        /// The JSON path.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The previous-run JSON is invalid.
    #[error("invalid previous results: {0}")]
    PreviousResultsJson(#[from] serde_json::Error),
    /// A Git operation failed.
    #[error("git operation failed: {0}")]
    Git(String),
}

/// Builds a plan containing every app.
///
/// # Errors
///
/// Returns [`SelectionError::Manifest`] for an invalid dependency graph.
pub fn select_all(manifest: &Manifest, head: Revision) -> Result<Plan, SelectionError> {
    let names: Vec<AppName> = manifest.apps.iter().map(|app| app.name.clone()).collect();
    let ordered = manifest.dependency_order(&names)?;
    let mut apps = Vec::with_capacity(ordered.len());
    for name in ordered {
        let spec = manifest
            .app(name.as_str())
            .ok_or_else(|| SelectionError::UnknownApp(name.to_string()))?;
        let depth = manifest.depth(&name)?;
        apps.push(PlannedApp {
            name,
            reason: SelectionReason::All,
            wave: -10 * i32::try_from(depth).unwrap_or(i32::MAX / 10),
            requires: spec.requires.clone(),
        });
    }
    Ok(Plan {
        base: None,
        head,
        apps,
    })
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
    let ordered = manifest.dependency_order(selected)?;
    let selected_set: std::collections::HashSet<&str> =
        selected.iter().map(AppName::as_str).collect();
    let mut apps = Vec::with_capacity(ordered.len());
    for name in ordered {
        let spec = manifest
            .app(name.as_str())
            .ok_or_else(|| SelectionError::UnknownApp(name.to_string()))?;
        let depth = manifest.depth(&name)?;
        let reason = if selected_set.contains(name.as_str()) {
            SelectionReason::Explicit
        } else {
            SelectionReason::DependencyOf(
                selected
                    .iter()
                    .find(|selected| {
                        manifest
                            .dependency_order(std::slice::from_ref(*selected))
                            .is_ok_and(|order| order.contains(&name))
                    })
                    .cloned()
                    .unwrap_or_else(|| name.clone()),
            )
        };
        apps.push(PlannedApp {
            name,
            reason,
            wave: -10 * i32::try_from(depth).unwrap_or(i32::MAX / 10),
            requires: spec.requires.clone(),
        });
    }
    Ok(Plan {
        base: None,
        head,
        apps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_shape_is_stable() {
        let plan = Plan {
            base: None,
            head: Revision::new("abc").unwrap(),
            apps: vec![PlannedApp {
                name: AppName::new("alpha").unwrap(),
                reason: SelectionReason::All,
                wave: 0,
                requires: Vec::new(),
            }],
        };
        assert_eq!(
            plan.matrix(),
            serde_json::json!({"include": [{"app": "alpha"}]})
        );
    }
}
