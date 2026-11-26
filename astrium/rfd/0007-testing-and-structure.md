---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: prediscussion
discussion: ""
labels: [software, testing, process]
---

# RFD 0007: Testing and structure

## Summary

Astrarium is a Cargo workspace with a thin binary, a domain library, and a
test-support crate for fakes and fixtures. The library is split by domain
(`manifest`, `selection`, `render`, `cluster`, `argocd`, `kube`, `probe`,
`diagnostics`, `telemetry`, `command`), not by technical layer.

Modules use `foo.rs` plus `foo/` and never `foo/mod.rs`. The rule is enforced
with `clippy::mod_module_files`, which bans `mod.rs`. It is **not**
`self_named_module_files`, which enforces the opposite layout.

Unit tests use fakes for commands, the Kubernetes API, Docker, and time. They
never require a live cluster. Integration tests exercise the CLI and the
renderer, and e2e tests run Astrarium against kind. The legacy `hack/e2e` shell
harness stays until Astrarium passes the smoke set twice.

## Motivation

Astrarium will be written by several agents in parallel worktrees. Without a
fixed structure, every agent invents its own module boundaries, its own error
type, and its own way to test a command. The result compiles but is expensive to
review and impossible to extend.

The project also has an unusual testing constraint: most of its behaviour is
orchestration. The code decides what to render, in which order to apply it, how
long to wait, and what to report when it fails. Those decisions are exactly what
unit tests should pin, and they are exactly what a cluster-based test cannot
isolate. Fakes are not a shortcut; they are the only way to test a timeout
without waiting for one.

A third constraint is migration. The shell harness already encodes hard-won
behaviour: the dry-run gate, the revision race, the stale-probe cleanup, and
the CrashLoopBackOff fast-fail. The Rust implementation must be compared against
it before the shell is removed, not after.

## Guide-level explanation

The workspace lives under `astrium/`:

```text
astrium/
  Cargo.toml                 # workspace, resolver = "3"
  rust-toolchain.toml        # 1.100.0
  rustfmt.toml               # style_edition = "2024"
  compose.yaml
  crates/
    astrarium/               # binary: CLI, miette fancy, telemetry init
    astrarium-core/          # domain library
    astrarium-test-support/  # fakes, fixtures, snapshot helpers
```

A new module is a file, not a directory with a `mod.rs`. The layout is regular:

```text
crates/astrarium-core/src/
  lib.rs  error.rs  result.rs
  manifest.rs  manifest/schema.rs  manifest/validate.rs
  selection.rs  selection/diff.rs  selection/matrix.rs
  render.rs  render/helm.rs  render/kustomize.rs  render/chart_repo.rs
  cluster.rs  cluster/kind.rs  cluster/registry.rs
  argocd.rs  argocd/install.rs  argocd/application.rs
  kube.rs  probe.rs  probe/resources.rs  probe/prometheus.rs
  diagnostics.rs  diagnostics/redact.rs
  telemetry.rs  command.rs  command/runner.rs
```

`lib.rs` declares the modules and re-exports a minimal public API. The binary
crate parses arguments with `clap`, initializes telemetry once, calls the
library, and renders errors with `miette`. It contains no domain logic.

Tests are chosen by scope:

| Scope | Location | May use |
| --- | --- | --- |
| Unit | `#[cfg(test)] mod tests` in the same file | fakes, temp dirs, fixtures, `insta` |
| Integration | `crates/*/tests/` | real `helm`/`kustomize`/`kubeconform`, temp dirs, no cluster |
| E2E | CI, `astrarium run` | kind, Docker, Argo CD, registry mirrors |
| CLI | `crates/astrarium/tests/cli.rs` | `assert_cmd`, `predicates`, `insta` |

No unit test creates a kind cluster, calls a real registry, or reads the user's
kubeconfig. A test that needs a cluster is an e2e test and belongs in the
workflow, not in `cargo test`.

## Reference-level explanation

### Crates

| Crate | Published | Responsibility |
| --- | --- | --- |
| `astrarium` | binary | `clap` CLI, telemetry initialization, `miette` rendering, exit codes. |
| `astrarium-core` | library | Manifest, selection, render, cluster, Argo CD, kube, probes, diagnostics, telemetry, command, errors. |
| `astrarium-test-support` | no (v0.1) | Fake `CommandRunner`, `KubeApi`, `DockerApi`, `Clock`; manifest builders; fixtures; snapshot helpers. |

The binary depends on `astrarium-core`; the library never depends on the
binary. `astrarium-test-support` depends on the library for its types and is a
dev-dependency of both. No dependency cycles.

### Modules and `mod.rs`

Rust supports both `foo.rs` and `foo/mod.rs` for a module with submodules. The
[Rust Reference](https://doc.rust-lang.org/reference/items/modules.html#module-source-filenames)
encourages the self-named layout because it avoids many files named `mod.rs`.
The [2018 edition guide](https://doc.rust-lang.org/edition-guide/rust-2018/path-changes.html#no-more-modrs)
records the change, and [RFC 2126](https://rust-lang.github.io/rfcs/2126-path-clarity.html)
keeps `mod.rs` legal without deprecation.

Astrarium follows the encouragement:

- `src/foo.rs` is the root of module `foo`, whether or not it has submodules.
- `src/foo/bar.rs` is submodule `bar`.
- `src/foo/mod.rs` is never added. `foo.rs` and `foo/mod.rs` must not coexist;
  the compiler rejects that anyway.
- `#[path]` is not used to work around the layout.
- The rule is enforced by `clippy::mod_module_files`, a restriction lint that
  bans `mod.rs`. **Do not enable `self_named_module_files`**: it enforces the
  `mod.rs` layout and is the opposite of this rule.

This is a tooling policy, not a language requirement. `mod.rs` remains legal
Rust, and an external contribution that uses it is reformatted rather than
rejected on principle.

### Naming, newtypes, and visibility

- Types, traits, and enums are `UpperCamelCase`; modules, functions, and
  variables are `snake_case`. No `get_` getters; conversions use `as_`, `to_`,
  or `into_`.
- Semantic strings are newtypes: `AppName`, `ProjectName`, `NamespaceName`,
  `ChartVersion`, `Revision`, `ContentHash`. Constructors validate and return a
  typed error; `Display` and `AsRef<str>` are implemented; serde is
  `#[serde(transparent)]` where the wire format is the string.
- Booleans with meaning are enums. `renderer` is `Renderer::Helm` or
  `Renderer::Kustomize`, not a `bool`.
- Visibility is private by default. `pub(crate)` is for cross-module internals;
  `pub` is only for the library API. `lib.rs` re-exports a minimal surface; no
  glob re-exports.
- Public enums and structs that may grow are `#[non_exhaustive]`.
- Builders are used for complex configuration and are `#[must_use]`.

### Errors

Each module has one `thiserror` enum: `ManifestError`, `SelectionError`,
`RenderError`, `ClusterError`, `ArgoError`, `KubeError`, `ProbeError`,
`DiagnosticsError`, `TelemetryError`, `CommandError`. The library has a
crate-level `Error` and a `Result<T>` alias. The binary has `error.rs` and
`result.rs` for `miette` rendering.

```rust
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("helm dependency build failed for {app}")]
    DependencyBuild {
        app: AppName,
        #[source]
        source: CommandError,
    },
    #[error("kustomize produced no manifests for {app}")]
    EmptyRender { app: AppName },
}
```

External command errors include the program, the sanitized arguments, the exit
code, and the last lines of stderr. `miette` diagnostics add a code, a help
message, and a source span for TOML/YAML parse errors. No `unwrap` or `expect`
in production code; use `?` with context, `let else` for early returns, and let
chains for combined checks. Tests may `unwrap` with `#[allow]` at the test
module.

### Async

- No blocking calls in async functions. `gix` operations run in
  `spawn_blocking`; file copies and hashing use `tokio::fs` or a bounded
  blocking pool.
- Independent work uses `tokio::task::JoinSet`. Cancellation uses
  `CancellationToken` wired to Ctrl-C. Cleanup uses guards (`DropGuard`) so the
  chart server and temp dirs are released on every path.
- Traits with async methods are used with static dispatch (generics or enums),
  not `dyn`. `async-trait` is not a dependency. Native `async fn` in traits is
  used where the call site is monomorphized.
- Time is read through `Clock`, not scattered `SystemTime::now()`; `jiff` is
  used for timestamps and `Duration` for budgets.

### Lints and formatting

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"

[workspace.lints.clippy]
all = "warn"
pedantic = "warn"
unwrap_used = "deny"
expect_used = "deny"
mod_module_files = "deny"
```

Each crate has `[lints] workspace = true`. `rustfmt.toml` sets
`style_edition = "2024"`. CI runs `cargo fmt --check`, `cargo clippy --locked
--all-targets --all-features -- -D warnings`, and `cargo test --locked
--all-features`. Public items have rustdoc examples; `missing_docs` is a warning
in the library, not in tests.

### Fakes

The core traits are small and used with static dispatch:

```rust
pub trait CommandRunner: Send + Sync {
    async fn run(&self, command: Command) -> Result<Output, CommandError>;
}

pub trait Clock: Send + Sync {
    fn now(&self) -> jiff::Timestamp;
}

pub trait KubeApi: Send + Sync {
    async fn apply(&self, object: DynamicObject) -> Result<DynamicObject, KubeError>;
    async fn get(&self, reference: ObjectRef) -> Result<DynamicObject, KubeError>;
    async fn delete(&self, reference: ObjectRef) -> Result<(), KubeError>;
}
```

`Command` carries a `Program` enum (`Helm`, `Kustomize`, `Kind`) instead of an
arbitrary string, so a test cannot accidentally approve an unlisted executable.
`CommandRunner::run` returns a typed `Output` with exit status, stdout, and
stderr; the fake queues responses and records calls for assertions.

`KubeApi` is implemented over `kube::Client` and typed models in production.
The fake stores objects in a map keyed by group, version, kind, namespace, and
name, and serves a scripted sequence of statuses. That is how the revision race
and the timeout path are tested without a cluster.

`DockerApi` is the Docker boundary for network inspection, node exec, and
containerd verification. The fake returns canned networks and command output.
It exists because the binary may not invoke the `docker` CLI (RFD 0005).

`Clock` is fixed or advanced by the test. Wait loops compute deadlines from
`Clock::now`; a test advances the fake and asserts that the loop fails with
`timeout` after the expected number of polls.

### Snapshots and CLI tests

- `insta` is used for Application/AppProject/Namespace serialization, wrapper
  chart manifests, diagnostics JSON, and span attribute sets. Snapshots are
  committed under `snapshots/` and reviewed with `cargo insta review`.
- Span snapshots assert low-cardinality names, the `astrarium.*` namespace,
  standard `cicd.*`/`k8s.*` attributes, and the absence of app names in span
  names.
- `assert_cmd` tests run the real binary for `version`, `manifest validate`,
  `plan --json`, and `--help`. Output snapshots include exit codes.
- `astrarium render --app <fixture>` writes a chart that is validated with
  `helm template` and `kubeconform` against the Kubernetes schema. The fixture
  suite includes a Kustomize app, a Helm app, and a chart with a Helm hook.
- Integration tests that need `helm`, `kustomize`, or `kubeconform` skip with a
  clear message when the tool is absent, but CI installs the tools and fails if
  the tests skip. A skipped test is visible, not silent.

### E2E tests

E2E is `astrarium run`, not a second framework. The smoke set is `spegel`,
`mosquitto` (Kustomize, AppArmor), `cert-manager`, `monitoring` (health and
revision), and `stalwart` once its chart merges. The nightly matrix runs every
app; the `mosquitto` canary on `ubuntu-latest` catches OS/AppArmor drift.

An e2e test must be reproducible on a fresh cluster. A probe that depends on
cold node state declares `requires_fresh_image` and `stale_probe_resources`
(RFD 0003); a probe that depends on containerd settings declares
`requires_containerd_config`. A retry that reuses node state is not a pass.

### Migration from `hack/e2e`

The shell harness remains the reference until Astrarium passes the smoke set
twice on fresh clusters. `hack/e2e/run-app.sh`, `list-changed.sh`,
`root-app.yaml`, `project.yaml`, and `apps.yaml` stay in place; no new shell e2e
scripts are added. This repository uses the read-only `[catalog]` adapter to
import the test-relevant entries from `apps/values.yaml` and
`hack/e2e/apps.yaml`; native `astrarium.toml` mode remains supported for
repositories without those catalogues.

`astrarium plan` is compared with `list-changed.sh` on historical base/head
pairs; a difference is a bug until proven otherwise. A green smoke run means
every smoke app is Synced and Healthy, every declared resource and Prometheus
target passes, and the diagnostics artifact is readable. After two green runs,
delete the runner scripts and `apps.yaml`, keep `kind.yaml` and `argocd/` as
manifest inputs, and update `hack/e2e/README.md`, `AGENTS.md`, and the
repository README to point at Astrarium.

### Review and manager workflow

The manager defines shared interfaces before parallel work starts:

1. Workspace, `astrarium-core` traits and types, native manifest structs, CLI
   skeleton, RFD 0001/0002, compose skeleton.
2. A one-time conversion of the test-relevant apps into `astrarium.toml`.
3. Typed Namespace/AppProject/Application models and the
   `CommandRunner`/`Clock`/`KubeApi` interfaces.

Then agents work in separate worktrees by module: manifest/selection,
render/Argo, cluster/registry/telemetry, CI/release, QA. The manager merges in
dependency order (types, render, cluster, CI, QA) and resolves shared files
(`Cargo.toml`, `lib.rs`) itself.

Every review checks:

- the change matches the RFD that owns the interface;
- no `mod.rs`, no `utils`/`helpers`/`common`, no glob re-exports;
- errors carry context and no production `unwrap`/`expect`;
- spans are low-cardinality and attributes stay in the `astrarium.*` namespace;
- diagnostics contain no Secret values, kubeconfigs, or secret-shaped env vars;
- new behaviour has a unit test, a snapshot, or an e2e probe;
- `cargo fmt --check`, clippy with `-D warnings`, and tests pass; no new
  third-party Action, unpinned image, or plaintext Secret.

A module that only compiles in isolation is not merged. The manager runs the
smoke e2e locally after each merge.

## Drawbacks

- Three crates add ceremony for a young project. The split is justified by the
  binary/library boundary and the test-support crate, not by publishability.
- Hand-written fakes can drift from the real client; a fake that is too
  permissive can make a unit test pass while the real command fails. The
  integration and e2e layers are the only defence.
- Snapshot tests are brittle when output changes for a good reason. `cargo insta
  review` makes the update deliberate, but it is still a review step.
- The `mod.rs` ban is a style choice that external contributors may not expect.
  Enforcing it with a restriction lint is a policy, not a correctness fix.
- The migration period keeps two implementations alive, so a bug fixed in one
  must be checked in the other until the shell harness is removed.

## Rationale and alternatives

**One crate instead of a workspace.** Simpler to start, but the binary would
link `miette`'s fancy renderer, `clap`, and every domain module into every test.
The workspace keeps the library free of CLI concerns and lets
`astrarium-test-support` depend on the library without a cycle.

**`mod.rs` layout.** Legal and common, but the Rust Reference encourages
self-named modules and the repository has many modules with submodules. A
single rule (`foo.rs` + `foo/`) is easier for agents to follow than a mix; the
`mod_module_files` lint makes it mechanical.

**Trait objects for `CommandRunner`/`KubeApi`.** Dynamic dispatch would simplify
some signatures, but native async traits are not dyn-compatible without boxing.
Generics and enums keep call sites monomorphized and the fakes explicit.

**`mockall` instead of hand-written fakes.** `mockall` does not model a stateful
Kubernetes API or a scripted command sequence well, and it adds a proc-macro
dependency to every test. Shared hand-written fakes can encode state.

**Keep the shell harness forever.** It is a useful compatibility reference but a
second implementation of the same orchestration. Keeping it doubles maintenance
and lets the two drift. Two green smoke runs are the exit criterion.

## Prior art

- The [Rust API guidelines](https://rust-lang.github.io/api-guidelines/) inform
  naming, newtypes, and visibility; the [Rust Reference module layout](https://doc.rust-lang.org/reference/items/modules.html#module-source-filenames)
  and 2018 edition guide are the basis for the `foo.rs` + `foo/` rule.
- Clippy's `mod_module_files` restriction lint is the mechanical enforcement;
  `self_named_module_files` enforces the opposite and is intentionally absent.
- [kube-rs](https://kube.rs/) testing patterns inform the `KubeApi` fake, and
  [insta](https://insta.rs/) and [assert_cmd](https://docs.rs/assert_cmd/) are
  the snapshot and CLI testing tools.
- `hack/e2e/README.md` documents the behaviours that the Rust tests must
  preserve during migration.

## Unresolved questions

- **Must answer before `published`:** is `astrarium-test-support` published to
  crates.io, or is it a workspace-only dev crate? Today it is workspace-only;
  publishing it would make downstream fixture reuse possible but adds an API
  commitment.
- **Must answer before `published`:** should integration tests that need
  `helm`/`kustomize`/`kubeconform` be `#[ignore]` by default, or skip at runtime
  with a message? Skipping keeps `cargo test` green on a laptop; `#[ignore]`
  makes the omission visible in the test list.
- **Can be deferred:** should the workspace adopt `cargo-nextest` for CI? It is
  faster and has better per-test isolation, but it is another tool to install.
- **Can be deferred:** should the manifest parser have property-based tests for
  merge precedence and path validation? `proptest` would add a dependency; the
  current table-driven tests cover the known cases.

## Future possibilities

- Mutation testing for the selection and wait logic, where a surviving mutant
  often reveals a missing assertion.
- Fuzzing the TOML and YAML parsers with `cargo-fuzz`, especially the
  multi-document Kustomize output.
- A `cargo xtask` for the release packaging and fixture regeneration, replacing
  the shell script in RFD 0006.
