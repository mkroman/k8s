#![allow(clippy::unwrap_used, clippy::expect_used)]

//! CLI integration tests.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use gix::refs::transaction::PreviousValue;
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
        self.repository
            .reference("refs/heads/main", id, PreviousValue::Any, "commit test")
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
