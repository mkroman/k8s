---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: prediscussion
discussion: ""
labels: [software, manifest]
---

# RFD 0003: Manifest and selection

## Summary

`astrarium.toml` is the native source of truth for an Astrarium test suite. It
declares the project root, the kind cluster, the Argo CD installation, the
registry mirrors, the AppProjects, the Namespaces, and the applications with
their renderer, values, dependencies, probes, and expected resources.

There is no runtime adapter that reads `apps/values.yaml` or
`hack/e2e/apps.yaml`. Those files remain the production catalogue and the
legacy harness's configuration. The manifest duplicates the test-relevant
subset, and that duplication is accepted.

`astrarium plan` selects applications by diffing Git trees with `gix`, by
comparing manifest entries, and by unioning the failures and cancellations from
a previous run. It emits a human summary, a JSON document, and a GitHub Actions
matrix.

## Motivation

The current harness derives the test suite from two YAML files with different
shapes: the production `apps/values.yaml` describes every Application and
AppProject, and `hack/e2e/apps.yaml` adds e2e-only metadata such as `requires`,
`prometheus_targets`, `resources`, `retry_limit`, and `requires_fresh_image`.
Selection is a 300-line shell script that parses both with `yq`, compares
normalized JSON blobs, and special-cases shared paths.

That works, but the schema is implicit, the suite cannot be adopted elsewhere
without adopting this repository's `apps` chart, and the selection rules cannot
be unit tested. A native manifest also lets the test suite carry kind-specific
facts that production should not contain: a disposable PostgreSQL fixture, a
Z-Stack emulator sidecar, an anonymous MQTT listener, a cold-pull image.

## Guide-level explanation

A suite is one TOML file at the repository root:

```toml
[project]
name = "k8s-home-lab"
root = "."
base_branch = "main"

[cluster]
provider = "kind"
name = "astrarium"
kind_config = "hack/e2e/kind.yaml"
kubeconfig = ".astrium/kubeconfig"

[argocd]
install = "hack/e2e/argocd"
namespace = "argocd"
project = "e2e"
repo_name = "astrarium"

# [render], [telemetry], and [registry] are omitted here; see the reference
# section and RFD 0005.

[[projects]]
name = "mosquitto"
source_repositories = ["*"]
destinations = [{ server = "https://kubernetes.default.svc", namespace = "mosquitto" }]
namespace_resource_whitelist = [
  { group = "apps", kind = "Deployment" },
  { group = "", kind = "ConfigMap" },
  { group = "", kind = "Service" },
  { group = "networking.k8s.io", kind = "NetworkPolicy" },
  { group = "batch", kind = "Job" },
  { group = "", kind = "PersistentVolumeClaim" },
]
namespace_resource_blacklist = []

[[namespaces]]
name = "mosquitto"
labels = { "pod-security.kubernetes.io/enforce" = "restricted", "pod-security.kubernetes.io/enforce-version" = "latest" }

[[apps]]
name = "mosquitto"
namespace = "mosquitto"
project = "mosquitto"
source = "mosquitto"
renderer = "kustomize"
probe = true

[[apps]]
name = "zigbee2mqtt"
namespace = "zigbee2mqtt"
project = "zigbee2mqtt"
source = "zigbee2mqtt"
renderer = "helm"
values = ["values-kind.yaml"]
requires = ["mosquitto"]
probe = true
timeout_seconds = 900

[[apps]]
name = "spegel"
namespace = "spegel"
project = "spegel"
source = "spegel"
renderer = "helm"
values = ["values-kind.yaml"]
requires_fresh_image = true
requires_containerd_config = true

# Projects/namespaces for the last two apps are omitted; see sync_policy below.
```

`astrarium manifest validate` parses the file, checks every path, resolves the
`requires` DAG, and reports all errors with `miette` spans. `astrarium manifest
schema` prints the JSON Schema generated from the Rust types.

A Helm app's values merge, in order: the chart's `values.yaml` (by Helm),
`common_values`, each file in `values`, then `inline_values`. Maps merge
recursively; arrays and scalars replace. An explicit `null` removes a key during
Helm coalescing; `{}` merges and cannot remove keys. Charts are designed so
empty means "feature off" (`zigbee2mqtt` `coordinator.devicePath`), because
Helm cannot clear a base value with `{}`.

`astrarium plan` answers "what changed?" without creating a cluster:

```console
$ astrarium plan --base origin/main
changed: mosquitto/overlays/kind/kustomization.yaml
selected: mosquitto (changed), zigbee2mqtt (previous failure)
matrix: {"include":[{"app":"mosquitto"},{"app":"zigbee2mqtt"}]}
```

## Reference-level explanation

### Top-level tables

| Table | Required | Purpose |
| --- | --- | --- |
| `[project]` | yes | Suite name, repository root, base branch. |
| `[cluster]` | yes | kind cluster settings (RFD 0005). |
| `[argocd]` | yes | Argo CD installation overlay and namespace (RFD 0004). |
| `[render]` | no | Executable names, render namespace, dry-run toggle, and command timeout. Defaults: `helm`, `kustomize`, `kind`, `default`, `true`, `10m`. |
| `[telemetry]` | no | OTLP endpoint, service name, environment, and enabled toggle (RFD 0005). |
| `[registry]` | no | Pull-through mirror settings (RFD 0005). Enabled by default; `enabled = false` pulls directly. |
| `[[helm_repositories]]` | no | Named Helm chart repositories registered before dependency builds. |

`[project]` fields are `name` (required; used as `cicd.pipeline.name`), `root`
(default `.`; canonicalized, and every path must stay inside it), and
`base_branch` (default `main`; used to find `origin/<base_branch>`).

`[argocd]` fields are `install`, `namespace`, `project` (suite-level
AppProject name), `repo_name` (chart repository label), and `server` (the
in-cluster API URL, default `https://kubernetes.default.svc`).

### Projects

| Field | Type | Default | Mapping |
| --- | --- | --- | --- |
| `name` | string | required | `AppProject.metadata.name` |
| `description` | string | `""` | `spec.description` |
| `source_repositories` | list of strings | `["*"]` | `spec.sourceRepos`; the run's chart repository URL is appended unless the list contains `"*"` |
| `destinations` | list of `{ server, name, namespace }` | required | `spec.destinations`; `server` defaults to `https://kubernetes.default.svc` |
| `cluster_resource_whitelist`, `cluster_resource_blacklist` | lists of `{ group, kind }` | `[]` | `spec.clusterResourceWhitelist`, `spec.clusterResourceBlacklist` |
| `namespace_resource_whitelist`, `namespace_resource_blacklist` | lists of `{ group, kind }` | unset | `spec.namespaceResourceWhitelist`, `spec.namespaceResourceBlacklist` |

Lists are serialized exactly as written. Astrarium does not reproduce the
`apps` chart's template defaults: the conversion from `apps/values.yaml` copies
the rendered lists, including the default blacklist for `ResourceQuota`,
`LimitRange`, and `NetworkPolicy`. Validation warns when a project sets neither
namespace list, because the resulting permission set is easy to misread.

### Namespaces

Namespaces carry `name`, `labels`, and `annotations`. Astrarium creates them
with server-side apply before any Application. Applications never use
`CreateNamespace=true`; a namespace missing from the manifest is a validation
error. Pod Security labels live here, not in the chart, so a privileged
namespace (`spegel`, `longhorn`) is an explicit manifest decision.

### Applications

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | Application name and default chart name. Unique. |
| `namespace` | string | required | Destination namespace; must be declared in `[[namespaces]]`. |
| `project` | string | app name | AppProject name; must be declared in `[[projects]]`. |
| `source` | path | required | Application directory, relative to `root`. |
| `renderer` | `helm` or `kustomize` | required | Rendering strategy (RFD 0004). |
| `values`, `common_values`, `inline_values` | list of paths, path, table | `[]`, unset, `{}` | Helm values sources, merged in that order. |
| `requires` | list of strings | `[]` | Applications that must be healthy first. |
| `probe` | boolean | `false` | The chart owns a PostSync probe; Astrarium does not reimplement it. |
| `timeout_seconds` | integer | from `[render].timeout` | Wait budget in seconds; overrides the render timeout. |
| `resources` | array of tables | `[]` | Objects that must exist after the app is healthy. |
| `prometheus_targets` | table | unset | Prometheus jobs whose targets must all be `up`. |
| `upgrade` | table | unset | Optional upgrade test: `from_values` or `from_revision` (RFD 0004). |
| `requires_fresh_image` | boolean | `false` | The probe needs an image absent from every node. |
| `requires_containerd_config` | boolean | `false` | The probe depends on the node's registry and content-store settings. |
| `stale_probe_resources` | table | unset | Resources and pods to delete before a cold-image run. |
| `sync_policy` | table | see below | Argo CD sync policy. |
| `annotations` | table | `{}` | Extra Application annotations, for example a custom sync wave. |

`resources` entries use a Kubernetes identity, not a file path:

```toml
[[apps.resources]]
api_version = "monitoring.coreos.com/v1"
kind = "ServiceMonitor"
namespace = "monitoring"
name = "prometheus"
```

`prometheus_targets` declares `namespace`, `service`, `port`, and `jobs`.
Every target for every listed job must
report `up`; a missing job is a failure, not a pass, and an empty job list is a
failure. The check uses the Kubernetes service proxy rather than a port-forward
where possible, so it does not depend on host networking.

### Sync policy and source resolution

The sync policy defaults to `automated = true`, `prune = true`,
`self_heal = true`, `sync_options = []`, `retry_limit = 20`,
`retry_backoff = "5s"`, and `retry_max_duration = "5m"`. `retry_limit = 0`
makes the first failed sync fatal, which cold-image probes require.
`ServerSideApply=true` is per app, matching the production Application that
needs it; `CreateNamespace=true` is rejected.

For `renderer = "kustomize"`, `source` is resolved in this order:

1. If `source` already contains `kustomization.yaml`, use it unchanged. This
   makes an explicit `source = "mosquitto/overlays/kind"` valid.
2. Otherwise, if `<source>/overlays/kind/kustomization.yaml` exists, use
   `<source>/overlays/kind`. This is the existing `kind_source_path`
   convention.
3. Otherwise, use `<source>`.

For `renderer = "helm"`, `source` must contain `Chart.yaml`. A
`values-kind.yaml` is not appended automatically; list it in `values`. The
legacy harness appended it by filename, which made a file's presence a hidden
configuration input.

### Validation

`astrarium manifest validate` is the only gate between TOML and a run. It
rejects unknown fields at every level (`deny_unknown_fields`); duplicate names;
an app whose namespace or project is undeclared; a project destination that
does not cover the app namespace (`namespace = "*"` is allowed); an unknown
name or cycle in `requires`, including self-dependency; path escapes (`..`,
absolute paths, symlinks resolving outside `root`); a missing source, chart, or
values file; a duration that is not a positive integer followed by `ms`, `s`,
`m`, or `h`; `CreateNamespace=true`; `requires_fresh_image` without an
`e2e.pullImage` value in the merged Helm values or with a Kustomize renderer;
and `stale_probe_resources` without a namespace or a selector.

Validation does not prove that `probe = true` is backed by a real PostSync
hook. That is a review obligation, and the smoke run is the proof.

### Selection

`astrarium plan` discovers the repository with `gix::discover`, parses and
validates the manifest, and determines the base revision in this order:
`--base <sha>`; the previous run's `head_sha` if it is an ancestor of `HEAD`;
`origin/<base_branch>`; `HEAD^`; the empty tree. It diffs the base tree to
`HEAD` with rename tracking, so a rename counts at both paths.

An application is selected when a changed path is equal to or under its
resolved source path (unless the path belongs to a nested application, in which
case only the longest matching source path selects); when a changed `values`
file, `common_values`, or the app's own manifest entry changes; when a project
or namespace it uses changes; when a shared input changes; or when its previous
status was `failure`, `cancelled`, or `skipped`. Shared inputs are
`astrarium.toml`, every `common_values` file, `cluster.kind_config`, the
`argocd.install` path, and the Astrarium source tree when it is inside
`project.root`.

Selecting an application adds its transitive `requires` closure to the
execution set, but the matrix contains only selected applications; `run --app`
resolves prerequisites at run time because each CI job gets its own cluster.
`plan --all` emits every app for the M0/M1 bring-up; it is a migration
convenience, not a replacement for diff selection.

The previous-run file is plain JSON produced by CI, not by the binary:

```json
{
  "head_sha": "abc1234",
  "apps": { "mosquitto": "success", "zigbee2mqtt": "failure", "monitoring": "cancelled" }
}
```

If `head_sha` is not an ancestor of `HEAD` (force-push or rebase), the data is
ignored and the base diff is used. If the file is malformed or names an unknown
application, the plan fails: a broken incremental input is a CI bug, not a
reason to silently run everything.

### Output and drift

By default `plan` writes a human summary to stdout. `--json` writes one object
to stdout and human diagnostics to stderr:

```json
{
  "base": "4a1c9e2", "head": "b7d0f31",
  "apps": [
    { "name": "mosquitto", "reasons": ["changed:mosquitto/overlays/kind/kustomization.yaml"] },
    { "name": "zigbee2mqtt", "reasons": ["previous-run:failure", "requires:mosquitto"] }
  ],
  "matrix": { "include": [{ "app": "mosquitto" }, { "app": "zigbee2mqtt" }] },
  "has_apps": true
}
```

When `$GITHUB_OUTPUT` is set, the tool appends `matrix=<json>` and
`has_apps=true|false`. When `$GITHUB_STEP_SUMMARY` is set, it appends a table
with the application, the reason, and the dependency depth. An empty selection
is a success with `has_apps=false`, not an error.

The manifest is not generated from `apps/values.yaml`. The conversion is a
one-time manual edit, and later production changes are ported by hand. The
manifest contains only the test-relevant applications, and a CI-only
consistency test may compare names, projects, and namespaces with
`apps/values.yaml` using `yaml_serde`; it never feeds a run and is not part of
the shipped binary. The legacy harness stays until Astrarium passes the smoke
set twice, so a drift bug has a second implementation to compare against.

## Drawbacks

- The manifest duplicates the production catalogue: a project rename or
  namespace label change must be made twice. TOML is also verbose for the
  nested Kubernetes-shaped lists and `[[apps.resources]]` entries.
- The source-path convention is hidden. A reader must know that `source =
  "mosquitto"` becomes `mosquitto/overlays/kind`; plan output should print the
  resolved path.
- Selection is path-based. A shared template outside an app's source path, or a
  semantic dependency not declared in `requires`, is missed. The shared-input
  list mitigates the common cases, not all of them.
- The previous-run union depends on CI reporting statuses correctly. A workflow
  bug that reports `success` for a cancelled job silently loses coverage.
- Validation cannot prove that a probe exists or that a `resources` entry is
  owned by the application. Those remain review and smoke-test concerns.

## Rationale and alternatives

**Reuse the YAML catalogues at runtime.** The shell harness does this and keeps
one source of truth. Rejected by the confirmed project decision: the suite must
be adoptable by other repositories, and a production values model is not a test
manifest. Duplication is visible and bounded; an adapter would be invisible and
would grow special cases.

**YAML instead of TOML.** TOML has no significant indentation, fewer
type-coercion surprises (`yes` is a string, not a boolean), better comments, and
good parser spans for `miette`. The manifest is configuration, not a Kubernetes
object.

**A hand-written JSON Schema.** The schema is generated from the Rust types with
`schemars`, so it cannot drift from the parser; `astrarium manifest schema`
exposes it to editors and CI without a second source of truth.

**`git` CLI plus `git diff --name-only`.** Rejected because the binary may
invoke only `helm`, `kustomize`, and `kind`, and because `gix`'s tree diff is a
better interface than parsing command output.

## Prior art

- `hack/e2e/list-changed.sh` is the direct predecessor: normalized manifest
  comparison, nested application ownership, shared-file escalation, and the
  `{"include":[{"app":"..."}]}` matrix shape.
- Argo CD's app-of-apps pattern informs the project/namespace/application
  split; Astrarium keeps the resources but creates them directly (RFD 0004).
- Nx, Turborepo, and Bazel provide prior art for affected-project selection and
  reverse-dependency queries; Astrarium's `requires` DAG is a much smaller graph
  with the same precision/coverage trade-off.

## Unresolved questions

- **Must answer before `published`:** should selection include reverse
  dependents (apps that require a changed app)? The acceptance criteria say
  "affected apps and dependents", while parity with `list-changed.sh` says no.
  The proposal is to keep parity in v0.1 and add `--with-dependents` later if
  the smoke runs show a missed prerequisite.
- **Must answer before `published`:** should the matrix contain prerequisites
  as separate jobs, or only selected applications? Today each job runs its own
  prerequisite closure, which duplicates work but keeps jobs independent.
- **Can be deferred:** does the manifest need a `manifest_version` field, so a
  future Astrarium can reject an old manifest with a clear message?
- **Can be deferred:** should the drift test be a hard failure or a warning? A
  hard failure catches drift but blocks legitimate intentional divergence.

## Future possibilities

- A one-time converter from the YAML catalogues to `astrarium.toml`, used only
  during migration and then deleted.
- `astrarium plan --explain <app>` to print the full dependency and selection
  reasoning for one application.
- Manifest fragments (`astrarium.d/*.toml`) for suites that outgrow one file.
- Selection by Kubernetes object identity, so a changed CRD selects every app
  that renders that kind and closes the semantic gap that path selection leaves.
