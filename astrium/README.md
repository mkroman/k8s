# Astrarium

Clockwork test orchestration for Kubernetes GitOps.

Astrarium is a generic end-to-end test orchestrator for Argo CD-based GitOps
repositories. It replaces the ad-hoc shell harness in `hack/e2e/` with a typed
Rust tool that renders an app's Helm chart or Kustomize overlay, serves the
packaged chart over HTTP, creates the Argo CD resources directly, waits for the
app to become healthy, and emits diagnostics and OpenTelemetry traces.

The project directory is `astrium/`; the binary, crate and release tag prefix
are `astrarium`, and the suite manifest is `astrarium.toml` at the repository
root.

## Status

The workspace, native manifest, command-line shell, telemetry bootstrap and
the local compose stack are in place. `astrarium run --app <name>` is working
end-to-end for the Kustomize and Helm smoke apps (`mosquitto`, `zigbee2mqtt`,
`cert-manager`, `prometheus-operator`, `monitoring`, and the cold-image
`spegel` probe). See `rfd/README.md` and the RFDs for the design, and
`../astrarium.toml` for this repository's suite.

## Quick start

```bash
# From this directory.
cargo build --release
./target/release/astrarium doctor

# From the repository root.
docker compose -f astrium/compose.yaml up -d
astrium/target/release/astrarium plan --all
astrium/target/release/astrarium run --app mosquitto
```

`astrarium doctor` checks `helm`, `kustomize`, `kind` and the manifest paths.
`plan` selects apps from a Git diff (M3) or explicitly with `--all`/`--app`.
`run` creates a kind cluster, installs Argo CD, and deploys the selected apps.

### Trace files

`run` can also write an OTLP JSON trace file:

```bash
astrarium run --app mosquitto --trace-file diagnostics/astrarium-trace.json
```

The file is valid OTLP JSON (`resourceSpans`/`scopeSpans`/`spans`) and can be
loaded into Jaeger UI with **Search → Upload** (the file upload control is
enabled by default). Jaeger 2.23 accepts OTLP JSON directly; no collector or
storage backend is needed to inspect an uploaded file. The e2e workflow uploads
the file as an `astrarium-<app>-trace` artifact on every run.

## Design

- **Manifest first.** `astrarium.toml` is the source of truth for the test
  suite. Native `[[projects]]`, `[[namespaces]]`, `[[apps]]` and
  `[[helm_repositories]]` tables remain supported. This repository opts into
  the read-only `[catalog]` adapter, which imports its existing
  `apps/values.yaml` and `hack/e2e/apps.yaml` before validation. Astrarium
  creates `Namespace`, `AppProject` and `Application` objects directly; it does
  not bootstrap the production `apps` chart.
- **Only three external CLI programs.** The Rust binary invokes `helm`,
  `kustomize` and `kind`. Git access uses `gix`, Kubernetes access uses `kube`,
  HTTP uses `reqwest`, and dates/times use `jiff`. Cold-image removal and node
  containerd checks use the Docker API through `bollard`, never the `docker`
  CLI.
- **A real chart repository.** Helm apps are packaged as-is and rendered by
  Argo CD with a typed `valuesObject`. Kustomize apps are rendered locally and
  wrapped in a tiny Helm chart so hooks and sync waves behave like Helm.
- **One trace per run.** OpenTelemetry spans follow the CI/CD semantic
  conventions (`cicd.pipeline.*`) with `astrarium.*` custom attributes and
  low-cardinality span names. Jaeger and registry pull-through caches live in
  `compose.yaml`.
- **Boring, explicit Rust.** Rust 2024, `foo.rs` + `foo/` module layout (never
  `foo/mod.rs`), domain modules, newtypes for semantic strings, `thiserror` in
  libraries, `miette` at the CLI boundary, and no `unwrap`/`expect` in
  production code.

## Workspace

```
Cargo.toml                  workspace, resolver = 3
rust-toolchain.toml          Rust 1.100.0
compose.yaml                 Jaeger + registry mirrors
rfd/                         Request for Discussion documents
crates/astrarium/            CLI, telemetry bootstrap, miette diagnostics
crates/astrarium-core/       manifest, selection, render, cluster, Argo, kube
crates/astrarium-test-support/ fakes and fixtures for tests
examples/minimal/            a minimal manifest
```

## Checks

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo build --locked --release
./target/release/astrarium doctor
```

The Astrarium CI and release workflows are in place. The e2e workflow is
manual until the first release is published, because it downloads a pinned
release binary. The legacy shell harness remains available until Astrarium has
passed the smoke set twice.

## CI and releases

- `.github/workflows/astrarium-ci.yaml` runs formatting, Clippy, tests, docs
  and a release build for changes under `astrium/`, `astrarium.toml`, and the
  Astrarium workflows/action.
- `.github/workflows/astrarium-release.yaml` runs on `astrarium-v*` tags. It
  checks the tag against the workspace version and publishes
  `astrarium-<version>-<target>.tar.gz` for `x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu`, with bash/zsh/fish completions and
  `SHA256SUMS`.
- `.github/workflows/astrarium-e2e.yaml` runs on app and harness changes. It
  uses the pinned release binary, lets the plan job select apps from the diff,
  and fans out one job per selected app. A manual dispatch runs the whole suite
  or one named app.
- `.github/actions/setup-astrarium/action.yml` downloads and verifies a
  released binary, and caches `~/.local/bin/astrarium` by version, OS and
  architecture. `.github/astrarium-version` pins the e2e version; bump it after
  cutting a release.

To cut a release, update the workspace version, commit, then push a matching
tag:

```bash
git tag astrarium-v0.1.1
git push origin astrarium-v0.1.1
```

## License

MIT OR Apache-2.0.
