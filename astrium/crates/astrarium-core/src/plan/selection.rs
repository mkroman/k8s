//! Git-backed change selection.
//!
//! The diff is computed entirely with `gix`: discover the repository,
//! resolve the revisions, find a merge base, and diff the two trees. No `git`
//! executable is invoked.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use gix::ObjectId;
use gix::bstr::ByteSlice;

use crate::manifest::{AppSpec, Manifest};
use crate::plan::{
    Plan, PreviousResults, SelectionError, SelectionReason, SelectionRequest, build_plan,
};
use crate::types::{AppName, Revision};

/// Selects apps from the repository change set.
pub(super) fn select_changes(
    manifest: &Manifest,
    request: &SelectionRequest,
) -> Result<Plan, SelectionError> {
    let repository = gix::discover(&manifest.project.root).map_err(|error| {
        SelectionError::Git(format!(
            "failed to discover a Git repository at {}: {error}",
            manifest.project.root.display()
        ))
    })?;
    let work_dir = repository
        .workdir()
        .ok_or_else(|| SelectionError::Git("repository has no work tree".to_owned()))?
        .to_path_buf();

    let head_spec = request.head.as_ref().map_or("HEAD", Revision::as_str);
    let head = resolve_commit(&repository, head_spec)?;
    let previous = read_previous_results(request.previous_results.as_deref())?;
    for name in previous.apps.keys() {
        if manifest.app(name).is_none() {
            return Err(SelectionError::UnknownApp(name.clone()));
        }
    }
    let base = resolve_base(manifest, &repository, request, &previous, head)?;
    let changed = changed_paths(&repository, base, head)?;
    let shared = shared_paths(manifest, &work_dir);

    // Record the shared input that changed, not every file below it. This
    // keeps the reason readable when a whole source tree changed.
    let mut shared_changed: BTreeSet<PathBuf> = shared
        .iter()
        .filter(|candidate| changed.iter().any(|path| path_matches(path, candidate)))
        .cloned()
        .collect();
    let all_shared: Vec<PathBuf> = shared_changed.iter().cloned().collect();
    shared_changed.retain(|candidate| {
        !all_shared
            .iter()
            .any(|other| other != candidate && candidate.starts_with(other))
    });

    let selected = if shared_changed.is_empty() {
        select_from_changes(manifest, &work_dir, &changed, &previous)
    } else {
        let paths: Vec<PathBuf> = shared_changed.into_iter().collect();
        manifest
            .apps
            .iter()
            .map(|app| {
                (
                    app.name.clone(),
                    SelectionReason::SharedChanged(paths.clone()),
                )
            })
            .collect()
    };

    let base = base.map(|id| Revision::new(id.to_string())).transpose()?;
    let head = Revision::new(head.to_string())?;
    build_plan(manifest, selected, base, head)
}

fn select_from_changes(
    manifest: &Manifest,
    work_dir: &Path,
    changed: &BTreeSet<PathBuf>,
    previous: &PreviousResults,
) -> Vec<(AppName, SelectionReason)> {
    let app_paths: Vec<Vec<PathBuf>> = manifest
        .apps
        .iter()
        .map(|app| app_selection_paths(manifest, work_dir, app))
        .collect();

    // A changed path belongs to the most specific app path that contains it.
    // This keeps a nested app such as `monitoring/cluster-exporters` from also
    // selecting its parent `monitoring` app, while still selecting two apps
    // that declare the same path.
    let mut matched: BTreeMap<usize, BTreeSet<PathBuf>> = BTreeMap::new();
    for path in changed {
        let mut candidates: Vec<(usize, usize)> = Vec::new();
        for (index, paths) in app_paths.iter().enumerate() {
            if let Some(depth) = paths
                .iter()
                .filter(|candidate| path_matches(path, candidate))
                .map(|candidate| candidate.components().count())
                .max()
            {
                candidates.push((index, depth));
            }
        }
        if let Some(max_depth) = candidates.iter().map(|(_, depth)| *depth).max() {
            for (index, depth) in candidates {
                if depth == max_depth {
                    matched.entry(index).or_default().insert(path.clone());
                }
            }
        }
    }

    let mut selected = Vec::new();
    for (index, app) in manifest.apps.iter().enumerate() {
        let reason = match matched.get(&index) {
            Some(paths) => Some(SelectionReason::Changed(paths.iter().cloned().collect())),
            None => previous
                .apps
                .get(app.name.as_str())
                .copied()
                .filter(|status| status.needs_rerun())
                .map(SelectionReason::PreviousRun),
        };
        if let Some(reason) = reason {
            selected.push((app.name.clone(), reason));
        }
    }
    selected
}

fn app_selection_paths(manifest: &Manifest, work_dir: &Path, app: &AppSpec) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.push(repo_path(manifest, work_dir, &app.source));
    paths.extend(
        app.watch
            .iter()
            .map(|path| repo_path(manifest, work_dir, path)),
    );
    paths.extend(
        app.values
            .iter()
            .map(|path| repo_path(manifest, work_dir, &app.source.join(path))),
    );
    if let Some(common) = &app.common_values {
        paths.push(repo_path(manifest, work_dir, common));
    }
    if let Some(upgrade) = &app.upgrade {
        paths.extend(
            upgrade
                .from_values
                .iter()
                .map(|path| repo_path(manifest, work_dir, &app.source.join(path))),
        );
    }
    paths.retain(|path| !path.as_os_str().is_empty());
    paths.sort();
    paths.dedup();
    paths
}

fn shared_paths(manifest: &Manifest, work_dir: &Path) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();

    // The manifest is always shared: a change to the suite definition can
    // affect any app. This also covers manifests with a non-standard name.
    paths.insert(PathBuf::from("astrarium.toml"));

    if let Some(path) = &manifest.path
        && let Some(relative) = manifest_repo_path(path, work_dir)
    {
        paths.insert(relative);
    }
    if let Some(kind_config) = &manifest.cluster.kind_config {
        paths.insert(repo_path(manifest, work_dir, kind_config));
    }
    paths.insert(repo_path(manifest, work_dir, &manifest.argocd.install));
    for shared in &manifest.project.shared_paths {
        paths.insert(normalize_shared_path(repo_path(manifest, work_dir, shared)));
    }
    // A common values file is shared by every app that lists it, but the RFD
    // treats it as a suite-wide input: a change can alter any app's render.
    for app in &manifest.apps {
        if let Some(common) = &app.common_values {
            paths.insert(repo_path(manifest, work_dir, common));
        }
    }

    paths.retain(|path| !path.as_os_str().is_empty());
    paths.into_iter().collect()
}

fn normalize_shared_path(path: PathBuf) -> PathBuf {
    // Accept the `dir/**` notation used in documentation as a directory
    // prefix; shared entries themselves are plain repository paths.
    match path.to_str().and_then(|text| text.strip_suffix("/**")) {
        Some(directory) => PathBuf::from(directory),
        None => path,
    }
}

fn manifest_repo_path(path: &Path, work_dir: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::absolute(path).ok()?
    };
    absolute
        .strip_prefix(work_dir)
        .ok()
        .map(normalize_lexically)
}

fn repo_path(manifest: &Manifest, work_dir: &Path, path: &Path) -> PathBuf {
    let root = manifest
        .project
        .root
        .strip_prefix(work_dir)
        .unwrap_or_else(|_| Path::new(""));
    normalize_lexically(&root.join(path))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn path_matches(path: &Path, candidate: &Path) -> bool {
    path == candidate || path.starts_with(candidate)
}

fn read_previous_results(path: Option<&Path>) -> Result<PreviousResults, SelectionError> {
    let Some(path) = path else {
        return Ok(PreviousResults::default());
    };
    let source = fs::read_to_string(path).map_err(|source| SelectionError::PreviousResults {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&source).map_err(SelectionError::PreviousResultsJson)
}

fn resolve_base(
    manifest: &Manifest,
    repository: &gix::Repository,
    request: &SelectionRequest,
    previous: &PreviousResults,
    head: ObjectId,
) -> Result<Option<ObjectId>, SelectionError> {
    if let Some(base) = &request.base {
        return Ok(Some(merge_base_or_tip(repository, base.as_str(), head)?));
    }

    if let Some(previous_head) = previous
        .head_sha
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        && let Ok(previous_head) = resolve_commit(repository, previous_head)
        && repository
            .is_ancestor(previous_head, head)
            .map_err(git_error("check whether the previous head is an ancestor"))?
    {
        // A force-push makes the previous head unrelated to this run. Fall
        // through to the configured base instead of diffing unrelated trees.
        return Ok(Some(previous_head));
    }

    let branch = &manifest.project.base_branch;
    for candidate in [format!("refs/remotes/origin/{branch}"), branch.to_owned()] {
        if let Ok(tip) = resolve_commit(repository, &candidate) {
            return Ok(Some(merge_base_or_tip_id(repository, tip, head)?));
        }
    }

    // The parent of the resolved head is `HEAD^` when the request head is
    // `HEAD`. Using the parent directly also keeps `--head` consistent.
    let parent = repository
        .find_commit(head)
        .map_err(git_error("read the head commit"))?
        .parent_ids()
        .next()
        .map(gix::Id::detach);
    Ok(parent)
}

fn merge_base_or_tip(
    repository: &gix::Repository,
    spec: &str,
    head: ObjectId,
) -> Result<ObjectId, SelectionError> {
    let tip = resolve_commit(repository, spec).map_err(|_| {
        SelectionError::Git(format!("cannot resolve the diff base revision {spec:?}"))
    })?;
    merge_base_or_tip_id(repository, tip, head)
}

fn merge_base_or_tip_id(
    repository: &gix::Repository,
    tip: ObjectId,
    head: ObjectId,
) -> Result<ObjectId, SelectionError> {
    let base = repository
        .merge_base(tip, head)
        .map_err(git_error("find the merge base"))?;
    Ok(base.map_or(tip, gix::Id::detach))
}

fn resolve_commit(repository: &gix::Repository, spec: &str) -> Result<ObjectId, SelectionError> {
    let id = repository
        .rev_parse_single(spec)
        .map_err(|error| SelectionError::Git(format!("cannot resolve {spec:?}: {error}")))?;
    let commit = repository
        .find_object(id)
        .map_err(git_error("read a revision object"))?
        .peel_to_commit()
        .map_err(git_error("peel a revision to a commit"))?;
    Ok(commit.id)
}

fn changed_paths(
    repository: &gix::Repository,
    base: Option<ObjectId>,
    head: ObjectId,
) -> Result<BTreeSet<PathBuf>, SelectionError> {
    let new_tree = repository
        .find_object(head)
        .map_err(git_error("read the head tree"))?
        .peel_to_tree()
        .map_err(git_error("peel the head to a tree"))?;
    let old_tree = base
        .map(|id| {
            repository
                .find_object(id)
                .map_err(git_error("read the base tree"))
                .and_then(|object| {
                    object
                        .peel_to_tree()
                        .map_err(git_error("peel the base to a tree"))
                })
        })
        .transpose()?;

    // Rename tracking makes a move count at both the old and new path. The
    // tree diff still reports directory entries, which are filtered below.
    let options = gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites::default()));
    let changes = repository
        .diff_tree_to_tree(old_tree.as_ref(), &new_tree, options)
        .map_err(git_error("diff the base and head trees"))?;

    let mut paths = BTreeSet::new();
    for change in changes {
        // The tree diff also reports directories because their object ids
        // change when a child changes. Selection is about files.
        if change.entry_mode().is_tree() {
            continue;
        }
        insert_path(&mut paths, change.location());
        insert_path(&mut paths, change.source_location());
    }
    Ok(paths)
}

fn insert_path(paths: &mut BTreeSet<PathBuf>, location: &gix::bstr::BStr) {
    if location.is_empty() {
        return;
    }
    paths.insert(PathBuf::from(location.to_str_lossy().into_owned()));
}

fn git_error(context: &'static str) -> impl FnOnce(gix::Error) -> SelectionError {
    move |error| SelectionError::Git(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use gix::refs::transaction::PreviousValue;

    use super::*;
    use crate::plan::PreviousRunStatus;
    use crate::types::AppName;

    const MANIFEST: &str = r#"
[project]
name = "test"
root = "."
base_branch = "missing"
shared_paths = ["shared", "astrium"]

[[projects]]
name = "alpha"
source_repositories = ["*"]
destinations = [{ namespace = "alpha" }]

[[namespaces]]
name = "alpha"

[[apps]]
name = "alpha"
namespace = "alpha"
project = "alpha"
source = "alpha"

[[apps]]
name = "beta"
namespace = "alpha"
project = "alpha"
source = "beta"
values = ["values-kind.yaml"]
common_values = "common/values.yaml"
requires = ["alpha"]

[[apps]]
name = "gamma"
namespace = "alpha"
project = "alpha"
source = "gamma"
watch = ["gamma-watch"]
requires = ["beta"]

[[apps]]
name = "delta"
namespace = "alpha"
project = "alpha"
source = "delta"

[[apps]]
name = "vector-a"
namespace = "alpha"
project = "alpha"
source = "vector"
values = ["values-a.yaml"]

[[apps]]
name = "vector-b"
namespace = "alpha"
project = "alpha"
source = "vector"
values = ["values-b.yaml"]
"#;

    struct Fixture {
        directory: tempfile::TempDir,
        repository: gix::Repository,
        commits: Vec<ObjectId>,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let repository = gix::init(directory.path()).unwrap();
            Self {
                directory,
                repository,
                commits: Vec::new(),
            }
        }

        fn commit(&mut self, message: &str, files: &[(&str, &str)]) -> ObjectId {
            self.commit_with_removals(message, files, &[])
        }

        fn commit_with_removals(
            &mut self,
            message: &str,
            files: &[(&str, &str)],
            removals: &[&str],
        ) -> ObjectId {
            let parent = self.commits.last().copied();
            let tree = match parent {
                Some(parent) => self
                    .repository
                    .find_commit(parent)
                    .unwrap()
                    .tree_id()
                    .unwrap()
                    .detach(),
                None => self.repository.empty_tree().id,
            };
            let mut editor = self.repository.edit_tree(tree).unwrap();
            for path in removals {
                editor.remove(*path).unwrap();
            }
            for (path, contents) in files {
                let blob = self
                    .repository
                    .write_object(gix::objs::Blob {
                        data: contents.as_bytes().to_vec(),
                    })
                    .unwrap();
                editor
                    .upsert(*path, gix::objs::tree::EntryKind::Blob, blob.detach())
                    .unwrap();
            }
            let tree = editor.write().unwrap().detach();
            let signature = gix::actor::SignatureRef {
                name: "test".into(),
                email: "test@example.com".into(),
                time: "1700000000 +0000",
            };
            let commit = self
                .repository
                .new_commit_as(signature, signature, message, tree, parent)
                .unwrap();
            let id = commit.id;
            self.repository
                .reference(
                    "refs/heads/main",
                    id,
                    PreviousValue::Any,
                    format!("commit {message}"),
                )
                .unwrap();
            self.commits.push(id);
            id
        }

        fn reference(&self, name: &str, id: ObjectId) {
            self.repository
                .reference(name, id, PreviousValue::Any, "test reference")
                .unwrap();
        }

        fn manifest(&self) -> Manifest {
            Manifest::from_toml_str(MANIFEST, self.directory.path()).unwrap()
        }

        fn select(&self, request: &SelectionRequest) -> Plan {
            select_changes(&self.manifest(), request).unwrap()
        }

        fn previous(&self, head_sha: ObjectId, apps: &str) -> PathBuf {
            self.previous_sha(&head_sha.to_string(), apps)
        }

        fn previous_sha(&self, head_sha: &str, apps: &str) -> PathBuf {
            let path = self.directory.path().join("previous.json");
            fs::write(
                &path,
                format!(r#"{{"head_sha": "{head_sha}", "apps": {{{apps}}}}}"#),
            )
            .unwrap();
            path
        }
    }

    fn names(plan: &Plan) -> Vec<&str> {
        plan.names()
    }

    #[test]
    fn selects_the_changed_app_and_its_prerequisites() {
        let mut fixture = Fixture::new();
        fixture.commit(
            "initial",
            &[
                ("alpha/Chart.yaml", "alpha"),
                ("beta/Chart.yaml", "beta"),
                ("gamma/Chart.yaml", "gamma"),
                ("delta/Chart.yaml", "delta"),
                ("vector/values-a.yaml", "a"),
                ("vector/values-b.yaml", "b"),
            ],
        );
        fixture.commit("change gamma", &[("gamma/Chart.yaml", "gamma-v2")]);

        let manifest = fixture.manifest();
        let plan = select_changes(&manifest, &SelectionRequest::default()).unwrap();
        assert_eq!(names(&plan), vec!["alpha", "beta", "gamma"]);
        assert!(matches!(
            plan.apps[2].reason,
            SelectionReason::Changed(ref paths) if paths == &[PathBuf::from("gamma/Chart.yaml")]
        ));
        assert_eq!(
            plan.apps[0].reason,
            SelectionReason::DependencyOf(AppName::new("beta").unwrap())
        );
        assert_eq!(plan.apps[0].wave, 0);
        assert_eq!(plan.apps[1].wave, -10);
        assert_eq!(plan.apps[2].wave, -20);
    }

    #[test]
    fn explicit_base_uses_the_merge_base() {
        let mut fixture = Fixture::new();
        let first = fixture.commit("initial", &[("alpha/Chart.yaml", "alpha")]);
        let second = fixture.commit("second", &[("beta/Chart.yaml", "beta")]);
        fixture.commit("third", &[("gamma/Chart.yaml", "gamma")]);

        let request = SelectionRequest {
            base: Some(Revision::new(second.to_string()).unwrap()),
            ..SelectionRequest::default()
        };
        let plan = fixture.select(&request);
        assert_eq!(plan.base, Some(Revision::new(second.to_string()).unwrap()));
        assert_eq!(names(&plan), vec!["alpha", "beta", "gamma"]);
        assert_ne!(plan.base, Some(Revision::new(first.to_string()).unwrap()));
    }

    #[test]
    fn previous_head_is_used_when_it_is_an_ancestor() {
        let mut fixture = Fixture::new();
        let first = fixture.commit("initial", &[("alpha/Chart.yaml", "alpha")]);
        fixture.commit("second", &[("beta/Chart.yaml", "beta")]);
        fixture.commit("third", &[("gamma/Chart.yaml", "gamma")]);

        let request = SelectionRequest {
            previous_results: Some(fixture.previous(first, r#""delta": "failure""#)),
            ..SelectionRequest::default()
        };
        let plan = fixture.select(&request);
        assert_eq!(plan.base, Some(Revision::new(first.to_string()).unwrap()));
        assert_eq!(names(&plan), vec!["alpha", "beta", "gamma", "delta"]);
        assert_eq!(
            plan.apps[3].reason,
            SelectionReason::PreviousRun(PreviousRunStatus::Failure)
        );
    }

    #[test]
    fn previous_success_is_not_selected_without_a_change() {
        let mut fixture = Fixture::new();
        let first = fixture.commit("initial", &[("alpha/Chart.yaml", "alpha")]);
        fixture.commit("second", &[("beta/Chart.yaml", "beta")]);

        let request = SelectionRequest {
            previous_results: Some(fixture.previous(
                first,
                r#""alpha": "success", "delta": "success", "vector-a": "cancelled""#,
            )),
            ..SelectionRequest::default()
        };
        let plan = fixture.select(&request);
        assert_eq!(names(&plan), vec!["alpha", "beta", "vector-a"]);
        assert!(matches!(
            plan.apps[2].reason,
            SelectionReason::PreviousRun(PreviousRunStatus::Cancelled)
        ));
    }

    #[test]
    fn force_push_falls_back_to_the_head_parent() {
        let mut fixture = Fixture::new();
        let first = fixture.commit("initial", &[("alpha/Chart.yaml", "alpha")]);
        fixture.commit("second", &[("beta/Chart.yaml", "beta")]);
        let third = fixture.commit("third", &[("gamma/Chart.yaml", "gamma")]);

        // The previous run was on a force-pushed history that is not an
        // ancestor of the requested head.
        let request = SelectionRequest {
            head: Some(Revision::new(fixture.commits[1].to_string()).unwrap()),
            previous_results: Some(fixture.previous(third, r#""alpha": "failure""#)),
            ..SelectionRequest::default()
        };
        let plan = fixture.select(&request);
        assert_eq!(plan.base, Some(Revision::new(first.to_string()).unwrap()));
        assert_eq!(names(&plan), vec!["alpha", "beta"]);
        assert!(matches!(
            plan.apps[1].reason,
            SelectionReason::Changed(ref paths) if paths == &[PathBuf::from("beta/Chart.yaml")]
        ));
    }

    #[test]
    fn base_branch_prefers_the_remote_reference() {
        let mut fixture = Fixture::new();
        let first = fixture.commit("initial", &[("alpha/Chart.yaml", "alpha")]);
        fixture.commit("second", &[("beta/Chart.yaml", "beta")]);
        fixture.commit("third", &[("gamma/Chart.yaml", "gamma")]);
        fixture.reference("refs/remotes/origin/main", first);

        let mut manifest = fixture.manifest();
        manifest.project.base_branch = "main".to_owned();
        let plan = select_changes(&manifest, &SelectionRequest::default()).unwrap();
        assert_eq!(plan.base, Some(Revision::new(first.to_string()).unwrap()));
        assert_eq!(names(&plan), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn root_commit_diffs_against_an_empty_tree() {
        let mut fixture = Fixture::new();
        fixture.commit(
            "initial",
            &[
                ("alpha/Chart.yaml", "alpha"),
                ("beta/Chart.yaml", "beta"),
                ("gamma/Chart.yaml", "gamma"),
                ("delta/Chart.yaml", "delta"),
                ("vector/values-a.yaml", "a"),
                ("vector/values-b.yaml", "b"),
            ],
        );
        let plan = fixture.select(&SelectionRequest::default());
        assert_eq!(plan.base, None);
        assert_eq!(
            names(&plan),
            vec!["alpha", "beta", "gamma", "delta", "vector-a", "vector-b"]
        );
    }

    #[test]
    fn shared_paths_select_every_app() {
        let mut fixture = Fixture::new();
        fixture.commit("initial", &[("alpha/Chart.yaml", "alpha")]);
        fixture.commit("shared", &[("shared/kind.yaml", "kind")]);

        let plan = fixture.select(&SelectionRequest::default());
        assert_eq!(
            names(&plan),
            vec!["alpha", "beta", "gamma", "delta", "vector-a", "vector-b"]
        );
        assert!(plan.apps.iter().all(|app| matches!(
            app.reason,
            SelectionReason::SharedChanged(ref paths) if paths == &[PathBuf::from("shared")]
        )));
    }

    #[test]
    fn shared_paths_accept_prefix_notation() {
        let mut fixture = Fixture::new();
        fixture.commit("initial", &[("alpha/Chart.yaml", "alpha")]);
        fixture.commit("shared", &[("shared/kind.yaml", "kind")]);
        let source = MANIFEST.replace("\"shared\"", "\"shared/**\"");
        let manifest = Manifest::from_toml_str(&source, fixture.directory.path()).unwrap();
        let plan = select_changes(&manifest, &SelectionRequest::default()).unwrap();
        assert_eq!(plan.apps.len(), 6);
        assert!(matches!(
            plan.apps[0].reason,
            SelectionReason::SharedChanged(ref paths) if paths == &[PathBuf::from("shared")]
        ));
    }

    #[test]
    fn unrelated_changes_select_nothing() {
        let mut fixture = Fixture::new();
        fixture.commit("initial", &[("alpha/Chart.yaml", "alpha")]);
        fixture.commit("unrelated", &[("README.md", "hello")]);

        let plan = fixture.select(&SelectionRequest::default());
        assert_eq!(plan.apps.len(), 0);
        assert_eq!(plan.matrix(), serde_json::json!({"include": []}));
    }

    #[test]
    fn a_listed_values_file_is_more_specific_than_the_shared_chart() {
        let mut fixture = Fixture::new();
        fixture.commit(
            "initial",
            &[
                ("vector/Chart.yaml", "chart"),
                ("vector/values-a.yaml", "a"),
                ("vector/values-b.yaml", "b"),
            ],
        );
        fixture.commit("change a", &[("vector/values-a.yaml", "a-v2")]);
        let plan = fixture.select(&SelectionRequest::default());
        assert_eq!(names(&plan), vec!["vector-a"]);
    }

    #[test]
    fn watch_paths_are_selected() {
        let mut fixture = Fixture::new();
        fixture.commit(
            "initial",
            &[
                ("gamma/Chart.yaml", "gamma"),
                ("gamma-watch/kustomization.yaml", "watch"),
            ],
        );
        fixture.commit(
            "change watch",
            &[("gamma-watch/kustomization.yaml", "watch-v2")],
        );
        let plan = fixture.select(&SelectionRequest::default());
        assert_eq!(names(&plan), vec!["alpha", "beta", "gamma"]);
        assert!(matches!(
            plan.apps[2].reason,
            SelectionReason::Changed(ref paths)
                if paths == &[PathBuf::from("gamma-watch/kustomization.yaml")]
        ));
    }

    #[test]
    fn nested_app_paths_do_not_select_the_parent() {
        let manifest_source = MANIFEST
            .replace("source = \"alpha\"", "source = \"parent\"")
            .replace("source = \"delta\"", "source = \"parent/nested\"");
        let mut fixture = Fixture::new();
        fixture.commit(
            "initial",
            &[
                ("parent/Chart.yaml", "parent"),
                ("parent/nested/Chart.yaml", "nested"),
            ],
        );
        fixture.commit("second", &[("parent/nested/Chart.yaml", "nested-v2")]);
        let manifest = Manifest::from_toml_str(&manifest_source, fixture.directory.path()).unwrap();
        let plan = select_changes(&manifest, &SelectionRequest::default()).unwrap();
        assert_eq!(names(&plan), vec!["delta"]);
    }

    #[test]
    fn common_values_are_shared_inputs() {
        let mut fixture = Fixture::new();
        fixture.commit(
            "initial",
            &[("alpha/Chart.yaml", "alpha"), ("common/values.yaml", "a")],
        );
        fixture.commit("common", &[("common/values.yaml", "b")]);
        let plan = fixture.select(&SelectionRequest::default());
        assert_eq!(
            names(&plan),
            vec!["alpha", "beta", "gamma", "delta", "vector-a", "vector-b"]
        );
        assert!(plan.apps.iter().all(|app| matches!(
            app.reason,
            SelectionReason::SharedChanged(ref paths)
                if paths == &[PathBuf::from("common/values.yaml")]
        )));
    }

    #[test]
    fn renames_select_both_paths() {
        let mut fixture = Fixture::new();
        fixture.commit("initial", &[("alpha/old.yaml", "same contents")]);
        fixture.commit_with_removals(
            "rename",
            &[("alpha/new.yaml", "same contents")],
            &["alpha/old.yaml"],
        );
        let plan = fixture.select(&SelectionRequest::default());
        assert_eq!(names(&plan), vec!["alpha"]);
        assert!(matches!(
            plan.apps[0].reason,
            SelectionReason::Changed(ref paths)
                if paths == &[
                    PathBuf::from("alpha/new.yaml"),
                    PathBuf::from("alpha/old.yaml"),
                ]
        ));
    }

    #[test]
    fn previous_short_sha_resolves() {
        let mut fixture = Fixture::new();
        let first = fixture.commit("initial", &[("alpha/Chart.yaml", "alpha")]);
        fixture.commit("second", &[("beta/Chart.yaml", "beta")]);
        let short = &first.to_string()[..7];
        let request = SelectionRequest {
            previous_results: Some(fixture.previous_sha(short, r#""gamma": "failure""#)),
            ..SelectionRequest::default()
        };
        let plan = fixture.select(&request);
        assert_eq!(plan.base, Some(Revision::new(first.to_string()).unwrap()));
        assert!(names(&plan).contains(&"gamma"));
    }

    #[test]
    fn unknown_previous_apps_fail_the_plan() {
        let mut fixture = Fixture::new();
        let first = fixture.commit("initial", &[("alpha/Chart.yaml", "alpha")]);
        fixture.commit("second", &[("alpha/Chart.yaml", "alpha-v2")]);
        let request = SelectionRequest {
            previous_results: Some(fixture.previous(first, r#""removed-app": "failure""#)),
            ..SelectionRequest::default()
        };
        let error = select_changes(&fixture.manifest(), &request).unwrap_err();
        assert!(matches!(error, SelectionError::UnknownApp(ref name) if name == "removed-app"));
    }
}
