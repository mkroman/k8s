#![allow(clippy::unwrap_used, clippy::expect_used)]

//! CLI integration tests.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use gix::refs::transaction::{PreviousValue, RefEdit};
use predicates::prelude::*;

fn binary() -> Command {
    Command::cargo_bin("astrarium").expect("astrarium binary")
}

fn repository_manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../astrarium.toml")
}

struct Repo {
    directory: tempfile::TempDir,
    repository: gix::Repository,
    commits: Vec<gix::ObjectId>,
}

impl Repo {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let repository = gix::init(directory.path()).expect("git init");
        Self {
            directory,
            repository,
            commits: Vec::new(),
        }
    }

    fn commit(&mut self, files: &[(&str, &str)]) -> gix::ObjectId {
        let parent = self.commits.last().copied();
        let tree = match parent {
            Some(parent) => self
                .repository
                .find_commit(parent)
                .expect("parent")
                .tree_id()
                .expect("tree")
                .detach(),
            None => self.repository.empty_tree().id,
        };
        let mut editor = self.repository.edit_tree(tree).expect("tree editor");
        for (path, contents) in files {
            let blob = self
                .repository
                .write_object(gix::objs::Blob {
                    data: contents.as_bytes().to_vec(),
                })
                .expect("blob");
            editor
                .upsert(*path, gix::objs::tree::EntryKind::Blob, blob.detach())
                .expect("upsert");
        }
        let tree = editor.write().expect("write tree").detach();
        let signature = gix::actor::SignatureRef {
            name: "test".into(),
            email: "test@example.com".into(),
            time: "1700000000 +0000",
        };
        let commit = self
            .repository
            .new_commit_as(signature, signature, "test commit", tree, parent)
            .expect("commit");
        let id = commit.id;
        let reference = "refs/heads/main".try_into().expect("reference name");
        let edit = RefEdit::update(reference, id, PreviousValue::Any, "commit test");
        self.repository
            .edit_references_as([edit], Some(signature))
            .expect("update main");
        self.commits.push(id);
        id
    }

    fn write_manifest(&self, source: &str) -> PathBuf {
        let path = self.directory.path().join("astrarium.toml");
        std::fs::write(&path, source).expect("manifest");
        path
    }
}

fn demo_manifest() -> &'static str {
    r#"
[project]
name = "test"
root = "."
base_branch = "missing"

[argocd]
install = "argocd"

[telemetry]
enabled = false

[registry]
enabled = false
mirrors = []

[[projects]]
name = "demo"
destinations = [{ namespace = "demo" }]

[[namespaces]]
name = "demo"

[[apps]]
name = "demo"
namespace = "demo"
project = "demo"
source = "demo"
renderer = "helm"
"#
}

#[test]
fn version_subcommand_prints_version() {
    binary()
        .arg("version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn schema_is_valid_json() {
    let output = binary()
        .args(["manifest", "schema"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let schema: serde_json::Value = serde_json::from_slice(&output).expect("schema JSON");
    assert_eq!(schema["title"], "Manifest");
    assert!(schema["properties"]["apps"].is_object());
}

#[test]
fn validate_accepts_a_minimal_manifest() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(directory.path().join("argocd")).expect("argocd dir");
    let manifest = directory.path().join("astrarium.toml");
    std::fs::write(
        &manifest,
        r#"
[project]
name = "test"
root = "."

[argocd]
install = "argocd"

[telemetry]
enabled = false

[registry]
enabled = false
mirrors = []
"#,
    )
    .expect("manifest");

    binary()
        .args(["manifest", "validate", "--config"])
        .arg(&manifest)
        .assert()
        .success()
        .stdout(predicate::str::contains("valid:"));
}

#[test]
fn validate_rejects_unknown_fields() {
    let directory = tempfile::tempdir().expect("tempdir");
    let manifest = directory.path().join("astrarium.toml");
    std::fs::write(
        &manifest,
        r#"
[project]
name = "test"
root = "."
unknown = true
"#,
    )
    .expect("manifest");

    binary()
        .args(["manifest", "validate", "--config"])
        .arg(&manifest)
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown field"));
}

#[test]
fn plan_all_emits_matrix() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(directory.path().join("argocd")).expect("argocd dir");
    std::fs::create_dir(directory.path().join("demo")).expect("demo dir");
    std::fs::write(
        directory.path().join("demo/Chart.yaml"),
        "apiVersion: v2\nname: demo\nversion: 0.1.0\n",
    )
    .expect("chart");
    let manifest = directory.path().join("astrarium.toml");
    std::fs::write(
        &manifest,
        r#"
[project]
name = "test"
root = "."

[argocd]
install = "argocd"

[telemetry]
enabled = false

[registry]
enabled = false
mirrors = []

[[projects]]
name = "demo"
source_repositories = ["*"]
destinations = [{ namespace = "demo" }]

[[namespaces]]
name = "demo"

[[apps]]
name = "demo"
namespace = "demo"
source = "demo"
"#,
    )
    .expect("manifest");

    binary()
        .args(["plan", "--all", "--json", "--config"])
        .arg(&manifest)
        .env_remove("GITHUB_OUTPUT")
        .env_remove("GITHUB_STEP_SUMMARY")
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""app": "demo""#));
}

#[test]
fn plan_change_selection_writes_matrix_and_outputs() {
    let mut repository = Repo::new();
    let base = repository.commit(&[("demo/Chart.yaml", "v1")]);
    let head = repository.commit(&[("demo/Chart.yaml", "v2")]);
    let manifest = repository.write_manifest(demo_manifest());
    let github_output = repository.directory.path().join("github-output");
    let step_summary = repository.directory.path().join("step-summary.md");

    let stdout = binary()
        .args(["plan", "--json", "--config"])
        .arg(&manifest)
        .arg("--github-output")
        .arg(&github_output)
        .arg("--step-summary")
        .arg(&step_summary)
        .env_remove("GITHUB_OUTPUT")
        .env_remove("GITHUB_STEP_SUMMARY")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plan: serde_json::Value = serde_json::from_slice(&stdout).expect("plan JSON");
    assert_eq!(plan["base"], base.to_string());
    assert_eq!(plan["head"], head.to_string());
    assert_eq!(
        plan["matrix"],
        serde_json::json!({"include": [{"app": "demo"}]})
    );
    assert_eq!(plan["apps"][0]["reason"]["kind"], "changed");

    let output = std::fs::read_to_string(&github_output).expect("github output");
    assert!(output.contains(r#"matrix={"include":[{"app":"demo"}]}"#));
    assert!(output.contains("has_apps=true"));
    let summary = std::fs::read_to_string(&step_summary).expect("step summary");
    assert!(summary.contains("| demo | 0 | changed (demo/Chart.yaml) |"));
}

#[test]
fn plan_empty_selection_is_a_success() {
    let mut repository = Repo::new();
    repository.commit(&[("demo/Chart.yaml", "v1")]);
    repository.commit(&[("README.md", "unrelated")]);
    let manifest = repository.write_manifest(demo_manifest());
    let github_output = repository.directory.path().join("github-output");
    let step_summary = repository.directory.path().join("step-summary.md");

    let stdout = binary()
        .args(["plan", "--json", "--config"])
        .arg(&manifest)
        .arg("--github-output")
        .arg(&github_output)
        .arg("--step-summary")
        .arg(&step_summary)
        .env_remove("GITHUB_OUTPUT")
        .env_remove("GITHUB_STEP_SUMMARY")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plan: serde_json::Value = serde_json::from_slice(&stdout).expect("plan JSON");
    assert_eq!(plan["matrix"], serde_json::json!({"include": []}));
    assert_eq!(plan["has_apps"], false);

    let output = std::fs::read_to_string(&github_output).expect("github output");
    assert!(output.contains("has_apps=false"));
    let summary = std::fs::read_to_string(&step_summary).expect("step summary");
    assert!(summary.contains("No apps selected."));
}

#[test]
fn full_repository_manifest_validates() {
    binary()
        .args(["manifest", "validate", "--config"])
        .arg(repository_manifest())
        .assert()
        .success()
        .stdout(predicate::str::contains("17 apps"));
}

#[test]
fn full_repository_plan_contains_every_e2e_app() {
    let stdout = binary()
        .args(["plan", "--all", "--json", "--config"])
        .arg(repository_manifest())
        .env_remove("GITHUB_OUTPUT")
        .env_remove("GITHUB_STEP_SUMMARY")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plan: serde_json::Value = serde_json::from_slice(&stdout).expect("plan JSON");
    let mut apps: Vec<&str> = plan["apps"]
        .as_array()
        .expect("apps")
        .iter()
        .map(|app| app["name"].as_str().expect("app name"))
        .collect();
    apps.sort_unstable();
    assert_eq!(
        apps,
        vec![
            "agentgateway",
            "cert-manager",
            "cloudnative-pg",
            "cluster-exporters",
            "gateway-api",
            "grafana",
            "magistr",
            "monitoring",
            "mosquitto",
            "node-feature-discovery",
            "prometheus-operator",
            "sealed-secrets",
            "spegel",
            "traefik",
            "vector-agent",
            "vector-aggregator",
            "zigbee2mqtt",
        ]
    );
    assert_eq!(
        plan["matrix"]["include"].as_array().expect("include").len(),
        17
    );
}

#[test]
fn run_help_lists_selection_flags() {
    binary()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--app"))
        .stdout(predicate::str::contains("--all"))
        .stdout(predicate::str::contains("--keep-cluster"))
        .stdout(predicate::str::contains("--reuse-cluster"))
        .stdout(predicate::str::contains("--fast-fail"));
}

#[test]
fn run_without_app_or_all_fails_before_creating_a_cluster() {
    let directory = tempfile::tempdir().expect("tempdir");
    let manifest = directory.path().join("astrarium.toml");
    std::fs::write(&manifest, demo_manifest()).expect("manifest");

    binary()
        .args(["run", "--config"])
        .arg(&manifest)
        .env_remove("GITHUB_OUTPUT")
        .env_remove("GITHUB_STEP_SUMMARY")
        .assert()
        .failure()
        .stderr(predicate::str::contains("select apps with --all or --app"));

    assert!(
        !directory.path().join(".astrium").exists(),
        "the run must not prepare a cluster before validating the selection"
    );
}

#[test]
fn run_unknown_app_fails_without_creating_a_cluster() {
    let directory = tempfile::tempdir().expect("tempdir");
    let manifest = directory.path().join("astrarium.toml");
    std::fs::write(&manifest, demo_manifest()).expect("manifest");

    binary()
        .args(["run", "--app", "missing", "--config"])
        .arg(&manifest)
        .env_remove("GITHUB_OUTPUT")
        .env_remove("GITHUB_STEP_SUMMARY")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown app \"missing\""));

    assert!(!directory.path().join(".astrium").exists());
}

#[test]
fn render_unknown_app_fails() {
    let directory = tempfile::tempdir().expect("tempdir");
    let manifest = directory.path().join("astrarium.toml");
    std::fs::write(&manifest, demo_manifest()).expect("manifest");

    binary()
        .args(["render", "--app", "missing", "--config"])
        .arg(&manifest)
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not declared"));
}

#[cfg(unix)]
#[test]
fn render_app_packages_a_helm_chart() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("tempdir");
    let chart = directory.path().join("demo");
    std::fs::create_dir_all(chart.join("templates")).expect("chart");
    std::fs::write(
        chart.join("Chart.yaml"),
        "apiVersion: v2\nname: demo\nversion: 0.1.0\n",
    )
    .expect("Chart.yaml");
    std::fs::write(
        chart.join("templates/configmap.yaml"),
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: demo\n",
    )
    .expect("template");

    let fake_helm = directory.path().join("fake-helm");
    std::fs::write(
        &fake_helm,
        r#"#!/bin/sh
set -eu
case "${1:-}" in
  dependency) exit 0 ;;
  template)
    printf 'apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: demo\n'
    exit 0
    ;;
  package)
    mkdir -p "$4"
    touch "$4/demo-$6.tgz"
    exit 0
    ;;
  repo) exit 0 ;;
  *) exit 0 ;;
esac
"#,
    )
    .expect("fake helm");
    let mut permissions = std::fs::metadata(&fake_helm)
        .expect("fake helm metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_helm, permissions).expect("fake helm permissions");

    let manifest = directory.path().join("astrarium.toml");
    std::fs::write(
        &manifest,
        format!(
            r#"
[project]
name = "test"
root = "."

[argocd]
install = "argocd"

[render]
helm = "{}"
dry_run = true
timeout = "30s"

[telemetry]
enabled = false

[registry]
enabled = false
mirrors = []

[[projects]]
name = "demo"
destinations = [{{ namespace = "demo" }}]

[[namespaces]]
name = "demo"

[[apps]]
name = "demo"
namespace = "demo"
project = "demo"
source = "demo"
renderer = "helm"
"#,
            fake_helm.display()
        ),
    )
    .expect("manifest");

    let output = directory.path().join("out");
    binary()
        .args(["render", "--app", "demo", "--output"])
        .arg(&output)
        .arg("--config")
        .arg(&manifest)
        .assert()
        .success()
        .stdout(predicate::str::contains("chart name: demo"))
        .stdout(predicate::str::contains("chart version: 0.1.0"));

    assert!(
        output.join("demo-0.1.0.tgz").is_file(),
        "the rendered chart must be packaged in the output directory"
    );
}
