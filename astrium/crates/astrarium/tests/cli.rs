#![allow(clippy::unwrap_used, clippy::expect_used)]

//! CLI integration tests.

use assert_cmd::Command;
use predicates::prelude::*;

fn binary() -> Command {
    Command::cargo_bin("astrarium").expect("astrarium binary")
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
