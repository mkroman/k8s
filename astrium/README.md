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
the local compose stack are in place. The first end-to-end app (`mosquitto`, a
Kustomize overlay) is the M1 milestone. See `rfd/README.md` and the RFDs for the
design, and `../astrarium.toml` for this repository's suite.

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

## Design

- **Manifest first.** `astrarium.toml` is the source of truth for the test
  suite. Astrarium creates `Namespace`, `AppProject` and `Application` objects
  directly; it does not bootstrap the production `apps` chart and does not read
  `apps/values.yaml` or `hack/e2e/apps.yaml` at runtime.
- **Only three external programs.** The Rust binary invokes `helm`,
  `kustomize` and `kind`. Git access uses `gix`, Kubernetes access uses `kube`,
  HTTP uses `reqwest`, and dates/times use `jiff`.
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

The e2e smoke set and CI workflows are added in milestones M2–M4. The legacy
shell harness remains available until Astrarium has passed the smoke set twice.

## License

MIT OR Apache-2.0.
