---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: published
discussion: "https://github.com/mkroman/k8s/pull/435"
labels: [software, architecture]
---

# RFD 0002: Astrarium overview

## Summary

Astrarium is a Rust command-line tool that orchestrates end-to-end tests for
Kubernetes GitOps repositories. It reads a native manifest, selects the
applications affected by a change, renders them into Helm charts, creates a
disposable kind cluster with Argo CD, applies the applications, waits for them
to become healthy, runs declared probes, and collects diagnostics and traces.

The tagline is **clockwork test orchestration for Kubernetes GitOps**. The
binary is `astrarium`, the suite manifest is `astrarium.toml` at the repository
root, and the source lives under `astrium/`.

## Motivation

The current harness in `hack/e2e/` works. It creates a three-node kind
cluster, installs a minimal Argo CD, renders the production `apps` chart into a
root Application, pauses the children, runs a server-side dry-run against the
real API server, then unpauses them in dependency order and waits for health
and probes. It has caught real regressions that static validation missed.

It is also at the end of its useful life as a general mechanism:

- The behaviour is bash plus `jq` plus `yq`. The dependency resolution, the
  dry-run gate, the revision race, and the diagnostics are hundreds of lines
  that cannot be unit tested without a cluster.
- It deploys the committed revision through a local Git daemon. Uncommitted
  changes under an application's path are silently absent, so the runner
  refuses to start unless those paths are clean. That is a poor inner loop.
- It is coupled to the production `apps` chart. The chart is the source of
  truth for deployment, which is correct, but it means the e2e harness inherits
  the chart's root Application, sync-wave layout, and Helm values model even
  when testing one app.
- It has no telemetry. A failed run produces console groups and, if the
  workflow uploads them, artifacts. There is no single trace that shows which
  phase consumed the time or which application failed first.
- It is specific to this repository. The scripts know about `apps/values.yaml`,
  `hack/e2e/kind.yaml`, and this repository's layout. Another GitOps repository
  cannot adopt it without forking and editing shell.

Astrarium replaces the mechanism, not the goal. The goal remains: deploy real
Argo CD Applications into a throwaway cluster, exercise the real charts and
hooks, and fail with diagnostics that explain what broke.

## Guide-level explanation

A developer runs:

```bash
astrarium plan --base origin/main
astrarium run --app mosquitto
astrarium run --all
```

`plan` reads `astrarium.toml`, finds the changed paths between the base and
`HEAD`, and prints the applications that need testing. In CI it also emits a
GitHub Actions matrix. `run` executes one application and its transitive
prerequisites. `run --all` executes the whole suite.

A single run has these phases:

1. Parse and validate `astrarium.toml`. Resolve paths under the project root.
2. Select the application and its `requires` closure.
3. Render each selected application into a self-contained Helm chart:
   Helm charts are packaged with their dependencies; Kustomize applications
   are built and wrapped in a generated chart (RFD 0004).
4. Serve the generated chart repository over HTTP on an ephemeral port.
5. Create a kind cluster with the registry and containerd configuration from
   RFD 0005.
6. Install Argo CD from the configured Kustomize overlay and wait for the
   application controller and repo server.
7. Create the selected Namespaces, AppProjects, and Applications directly from
   Rust. There is no root `apps` Application and no bootstrap chart.
8. Server-side dry-run the rendered resources against the cluster.
9. Create and wait for Applications in dependency order, observing Argo CD's
   health, sync, operation phase, and revision.
10. Run the declared resource and Prometheus target checks. Chart-owned
    PostSync probes run inside Argo CD's operation and are observed by the wait
    loop.
11. Write `diagnostics.json` and `summary.md`; export OTLP traces if a collector
    is reachable.
12. Delete the cluster on success unless `--keep-cluster`; keep it on failure
    and print the cluster name and kubeconfig path.

The tool is deliberately not a Kubernetes operator, not a replacement for Argo
CD, and not a production deployment mechanism. It is a test runner that happens
to speak Kubernetes and Argo CD.

## Reference-level explanation

### Name

An astrarium is a medieval astronomical clock: a clockwork model of the sky.
The name fits a tool that makes a deterministic, observable rehearsal of a
deployment. It is short, pronounceable, and free on crates.io. `orrery`, the
obvious alternative, is already taken by a diagram-language crate.

The spelling split is intentional and permanent:

| Thing | Spelling | Example |
| --- | --- | --- |
| Project, binary, crate, config, tag prefix | `astrarium` | `astrarium`, `astrarium.toml`, `astrarium-v0.1.0` |
| Source directory in this repository | `astrium` | `astrium/Cargo.toml`, `astrium/rfd/` |
| Runtime state directory at the repository root | `.astrium` | `.astrium/kubeconfig` |

`astrium/` is the directory this repository uses while the project is
co-located with the GitOps catalogue. It is not part of the public interface.
If the project moves to its own repository, the crate names, binary name,
config name, and tag prefix do not change. `.astrium/` is gitignored runtime
state: kubeconfigs, rendered charts, and run metadata. It is dot-prefixed so it
does not read as part of the source tree.

### Goals

- **G1 — Generic.** The tool is driven by `astrarium.toml`, not by this
  repository's paths. Another GitOps repository can adopt it by writing a
  manifest and pointing at its charts.
- **G2 — Real Argo CD.** Applications are deployed by a real Argo CD into a
  real cluster. Helm hooks, sync waves, health assessment, and CRD ordering are
  exercised, not simulated.
- **G3 — Incremental.** `plan` selects only the applications affected by a
  diff, plus the applications that failed or were cancelled in the previous
  run, plus their transitive prerequisites.
- **G4 — Testable.** The manifest, selection, rendering, and wait logic are
  unit-testable without a cluster. External commands, the Kubernetes API, and
  time are behind traits with fakes (RFD 0007).
- **G5 — Observable.** One trace per run, with CI/CD semantic-convention spans
  and bounded `astrarium.*` attributes. A failed run answers "which phase,
  which application, which error class" without reading raw logs (RFD 0005).
- **G6 — Bounded diagnostics.** On failure, collect application status, pods,
  events, logs, Argo CD controller errors, and content hashes into
  `diagnostics.json` and `summary.md`. Redact Secrets and secret-shaped values.
- **G7 — Fast inner loop.** Render from the working tree, not from a Git
  commit. Uncommitted changes are deployed; there is no clean-tree precondition
  and no local Git daemon.
- **G8 — Small tool surface.** The binary invokes only `helm`, `kustomize`, and
  `kind` on the host. Git is read with `gix`, Kubernetes with `kube`, and
  Docker through its API (RFD 0005). CI scripts may use `gh`, `docker compose`,
  `tar`, and `sha256sum`; the binary may not.

### Non-goals

- **N1 — Replace Argo CD or Helm.** Astrarium packages charts and creates
  Applications. Argo CD remains the deployer and the health authority.
- **N2 — A general-purpose Kubernetes operator.** There is no controller, no
  CRD, and no reconciliation loop in the cluster.
- **N3 — Long-running production observability.** Traces are for test runs.
  Metrics and alerting are out of scope for v0.1.
- **N4 — Reimplement Kubernetes API machinery.** `kube` and `k8s-openapi`
  provide discovery, apply, watch, and typed resources. Astrarium adds the
  orchestration policy, not another client.
- **N5 — Support every cluster provider.** v0.1 supports kind only. The
  `cluster.provider` field exists so a second provider can be added without
  changing the manifest shape, not because one is planned now.
- **N6 — Be a test framework.** Astrarium does not define test cases in a DSL.
  Probes are Kubernetes Jobs owned by the charts, declared `resources`, and
  Prometheus target assertions. The orchestration is the product.

### Architecture

```text
astrarium.toml
      |
      v
  manifest parse + validate            RFD 0003
      |
      v
  selection (gix diff + previous run)  RFD 0003
      |
      v
  render (Helm / Kustomize -> chart)   RFD 0004
      |
      v
  cluster + Argo CD + registry         RFD 0005
      |
      v
  apply + wait + probes                RFD 0004
      |
      v
  diagnostics + OTLP traces            RFD 0005
```

The dependency direction is one-way. `manifest` does not know about `render`;
`render` does not know about `cluster`; `selection` produces names and depths,
not Applications. The binary crate wires the modules together and owns
`miette` rendering, telemetry initialization, and the `clap` surface.

### Workspace crates

| Crate | Kind | Responsibility |
| --- | --- | --- |
| `astrarium` | binary | CLI parsing, telemetry initialization, `miette` error rendering, calls into the library. Thin by design. |
| `astrarium-core` | library | `manifest`, `selection`, `render`, `cluster`, `argocd`, `kube`, `probe`, `diagnostics`, `telemetry`, `command`, `error`. No CLI. |
| `astrarium-test-support` | dev/test library | Fake `CommandRunner`, `KubeApi`, `DockerApi`, `Clock`; fixtures; snapshot helpers; manifest builders. Not published in v0.1. |

Rust 2024, `rust-version = "1.100"`, Cargo `resolver = "3"`. The workspace
layout, lint policy, and module rules are RFD 0007.

### Relationship to the production `apps` chart

The production `apps` chart in `apps/` remains the app-of-apps source of truth
for the real cluster. Astrarium does not render or deploy the `apps` chart.
`astrarium.toml` is the suite definition: it can declare projects, namespaces
and applications natively, or opt into a read-only catalog adapter that reads
`apps/values.yaml` and `hack/e2e/apps.yaml`. This repository uses the catalog
adapter, limited to the applications listed in the e2e catalogue.

Native mode accepts duplication as a trade-off, not as a hidden mechanism:

- The manifest is the source of truth for tests, including values that are
  kind-specific.
- A change to a production Application does not automatically change a native
  test manifest. A human converts the test-relevant part.
- In catalog mode the adapter imports the same definitions at load time, so
  production renames and namespace label changes flow into the suite without a
  second copy. The trade-off is coupling to those YAML shapes.
- An optional CI-only consistency check can still compare a native manifest
  with `apps/values.yaml` to catch drift. It is a repository check, not a
  runtime adapter, and it does not feed the run.
- The `apps` chart is never used as an e2e bootstrap. There is no root `apps`
  Application in the test cluster (RFD 0004).

### Commands

| Command | Purpose |
| --- | --- |
| `astrarium plan [--base <sha>] [--previous-results <json>] [--all] [--json]` | Select applications and emit a human summary, JSON, `$GITHUB_OUTPUT`, and `$GITHUB_STEP_SUMMARY`. |
| `astrarium run --app <name>` | Run one application and its transitive prerequisites. |
| `astrarium run --all` | Run every application in the manifest. |
| `astrarium run --app <name> --keep-cluster` | Leave the cluster for inspection. |
| `astrarium run --app <name> --reuse-cluster` | Reuse an existing cluster named by the manifest. |
| `astrarium run --app <name> --fast-fail` | Treat the first `Degraded` health as fatal, in addition to the default fatal conditions. |
| `astrarium upgrade --app <name>` | Install, wait, trigger a second sync, and wait again to exercise upgrade hooks. |
| `astrarium render --app <name> [--output <dir>]` | Render and package without creating a cluster. |
| `astrarium cluster create\|delete\|list` | Manage kind clusters outside a run. |
| `astrarium doctor` | Check tools, Docker, Jaeger, and registry mirrors; print actionable fixes. |
| `astrarium manifest schema\|validate` | Emit the JSON Schema and validate the manifest. |
| `astrarium version` | Print the version. |

`--config <path>` overrides `astrarium.toml`; `ASTRARIUM_*` environment
variables override individual settings. The CLI uses `clap` derive. The
manifest schema is generated with `schemars` (RFD 0003).

### Milestones

| Milestone | Scope | Acceptance |
| --- | --- | --- |
| **M0 — RFDs and skeleton** | RFDs 0001–0007, workspace, CLI `version`/`doctor`, `compose.yaml`, telemetry init, `astrium/README.md`, `AGENTS.md` update. | `cargo test`, `cargo clippy`, and `cargo fmt --check` pass; `astrarium doctor` inspects the required tools; the RFD index exists and every RFD has a state. |
| **M1 — One Kustomize app** | `astrarium.toml` for `mosquitto`; `plan`; kind; Argo CD; chart repository; direct Namespace/AppProject/Application creation; probe; diagnostics. | `astrarium run --app mosquitto` passes locally on a fresh cluster; the same application passes through the legacy harness for comparison. |
| **M2 — Helm apps and prerequisites** | Helm packaging and `valuesObject` for `cert-manager`, `prometheus-operator`, `monitoring`, and `stalwart` once its chart merges; resource and Prometheus checks; fast-fail; upgrade. | The smoke set passes; `astrarium upgrade --app stalwart` exercises `post-upgrade` hooks. `stalwart` is planned work: its chart is on an unmerged PR (#430), not on this branch. |
| **M3 — Incremental selection and registry** | Previous-run JSON, `gix` diff, matrix output, registry `hosts.toml`, Jaeger traces with CI/CD semantic conventions, step summary and artifacts. | A one-app fix selects only affected applications and their prerequisites; a registry pull is served by the mirror; one trace per run is visible in Jaeger. |
| **M4 — CI, release, and migration** | Project CI, release workflow, local setup action, e2e workflow uses Astrarium; legacy harness kept as reference. | `astrarium-v0.1.0` publishes binaries and `SHA256SUMS`; e2e downloads the cached binary; two consecutive green smoke runs; RFDs updated to `published`/`committed`; `hack/e2e` shell runners removed. |

### Success criteria

The rewrite is accepted when:

1. A developer can run `astrarium run --app <name>` with only `helm`,
   `kustomize`, `kind`, and Docker or Podman installed; the binary invokes no
   other host executables.
2. `astrarium.toml` is the source of truth; suites use either the native
   tables or the read-only catalog adapter, never both. Namespaces,
   AppProjects, and Applications are created directly from Rust.
3. `astrarium plan` selects the same applications as `hack/e2e/list-changed.sh`
   for existing pull requests, plus the previous-run incremental behaviour.
4. A one-app fix after a completed run runs only the affected applications and
   what they require.
5. The smoke set (`spegel`, `mosquitto`, `cert-manager`, `monitoring`,
   `stalwart` when merged) passes on Ubuntu 24.04; `mosquitto` also passes on
   `ubuntu-latest` as an OS/AppArmor canary.
6. Release binaries are downloadable and cached; e2e jobs do not build the
   tool unless the tool itself changed.
7. Jaeger shows one trace per run with CI/CD pipeline and task spans, standard
   `astrarium.*`/`k8s.*` attributes, no high-cardinality span names, and no
   secrets.
8. Failure diagnostics are uploaded and readable, and contain no secrets.
9. RFDs are indexed and in the appropriate state; `cargo test`, clippy, and fmt
   pass.

## Drawbacks

- Astrarium is another tool to maintain, in a repository that already has a
  working harness. The migration must keep the old harness until the new one
  has passed the smoke set twice, which means both exist for a while.
- The native manifest duplicates the test-relevant part of the production
  catalogue. Drift is possible and only partly detectable.
- The tool requires Docker (for kind) and a local Helm and Kustomize. That is
  normal for this repository but is a higher bar than a pure Go binary with an
  embedded renderer.
- Rendering locally with Helm 3 while Argo CD renders with Helm 4 (RFD 0004)
  means packaging and rendering are not the same engine. The smoke suite is the
  only proof that a chart behaves the same in both.
- The binary's host-executable restriction is self-imposed and makes some
  operations (image removal from kind nodes, Docker network inspection) require
  the Docker API rather than a shell command. That is more code, not less.

## Rationale and alternatives

**Extend the shell harness.** The cheapest short-term option. Rejected because
the hard parts (dependency ordering, revision race, dry-run filtering,
diagnostics) are exactly the parts that are painful in bash, and because the
harness cannot be adopted by another repository without forking it.

**Use an existing test framework (kuttl, Chainsaw, Terratest).** These are good
tools, but they test manifests, not an app-of-apps deployment through Argo CD.
Chainsaw has no notion of an Argo CD Application health status or a Helm hook
mapped to PostSync. Terratest is a Go library and would still require writing
the orchestrator. The missing piece is the Argo CD-aware orchestration, not the
assertion syntax.

**Drive Argo CD through its API or CLI.** Astrarium uses the Kubernetes API and
a kubeconfig, not the Argo CD API. The Argo API would require port-forwarding,
admin credentials, and a second authentication model. The Application resource
already exposes the state the runner needs.

**Keep the Git daemon and the `apps` chart.** Rejected because it forces the
clean-tree precondition, couples the test to the production chart's root
Application, and makes selecting a subset an exercise in rendering the whole
catalogue with a filtered values object. The chart repository (RFD 0004)
removes both couplings.

**Write it in Go.** Go has mature Kubernetes and kind libraries. Rust was
chosen for the existing repository direction, strong typing for the manifest
and Argo models, and the ability to unit test the selection and rendering logic
without a cluster. The cost is a smaller ecosystem for kind orchestration,
which is mitigated by invoking `kind` rather than reimplementing it.

## Prior art

- `hack/e2e/run-app.sh` in this repository is the direct predecessor. Its
  dry-run gate, revision check, stale-probe cleanup, and fast-fail logic are
  preserved as requirements in RFD 0004.
- [kind](https://kind.sigs.k8s.io/) provides the cluster lifecycle and the
  containerd configuration hooks.
- [Argo CD](https://argo-cd.readthedocs.io/) provides Application health,
  sync waves, hooks, and the repo server that renders charts.
- [kube-rs](https://kube.rs/) provides the typed Kubernetes client and the
  `CustomResource` derive used for Argo CD's CRDs.
- [gix](https://github.com/GitoxideLabs/gitoxide) provides repository discovery
  and tree diffs without shelling out to `git`.
- [kuttl](https://kuttl.dev/) and [Chainsaw](https://kyverno.github.io/chainsaw/)
  are prior art for declarative Kubernetes e2e tests; Astrarium borrows the
  idea of a declarative suite and rejects their manifest-only scope.

## Unresolved questions

- **Must answer before `published`:** is the `astrium/` directory name worth
  the permanent spelling split? The alternative is to rename the directory to
  `astrarium/` and accept a longer path in exchange for one spelling. The
  current decision is `astrium/`, confirmed for v0.1, but the RFD records the
  cost explicitly.
- **Must answer before `published`:** does "Docker or Podman" in the success
  criteria mean the binary must work with a Podman socket? v0.1 uses the Docker
  API for kind and registry inspection. Podman's Docker-compatible socket may
  be enough, but it is untested. If it is not, the criterion should say Docker
  only.
- **Can be deferred:** should `cluster.name` accept any base name? v0.1
  requires it to start with `astrarium`, so the deletion guard is a simple
  `astrarium-` prefix; an arbitrary base would need a managed-by label instead.
- **Can be deferred:** when should the project move from `astrium/` to its own
  repository? The crate API is designed to allow it; the migration itself is
  not scheduled.
- **Can be deferred:** should the binary be published to crates.io, or only
  distributed as GitHub release binaries? The release workflow in RFD 0006
  assumes binaries and does not publish to crates.io.
- **Can be deferred:** is the name `astrarium` sufficiently distinct for
  search? The crates.io name is free, but there are existing astronomical-clock
  projects with similar names. The binary and tag prefix disambiguate within
  this repository; the wider ecosystem is not under our control.

## Future possibilities

- A second cluster provider (k3d, a remote test cluster) behind the same
  `cluster.provider` field.
- Metrics for pipeline duration, active runs, and per-application outcomes,
  using the OpenTelemetry CI/CD conventions once they stabilize (RFD 0005).
- A `astrarium watch` mode that reruns the affected applications when files
  change, using the same selection logic as `plan`.
- Publishing the manifest JSON Schema so editors can validate
  `astrarium.toml` without installing the binary.
- Reusing the render and wait layers for a local "preview" command that deploys
  a branch into a personal namespace without the full test harness.
