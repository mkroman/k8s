---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: prediscussion
discussion: ""
labels: [software, manifest]
---

# RFD 0003: Manifest and selection

## Summary

`astrarium.toml` is the source of truth for an Astrarium test suite. It
declares the project root, the kind cluster, the Argo CD installation, the
registry mirrors, the AppProjects, the Namespaces, and the applications with
their renderer, values, dependencies, probes, and expected resources.

A suite can declare those resources natively, as `[[projects]]`,
`[[namespaces]]`, `[[apps]]` and `[[helm_repositories]]` tables, or opt into a
`[catalog]` adapter that imports the repository's existing YAML catalogues:
`apps/values.yaml` for projects, namespaces and applications, and
`hack/e2e/apps.yaml` for the e2e subset and its metadata. The adapter is
read-only and runs before normal validation; it never deploys the production
`apps` chart. A manifest must use one mode or the other, never both.

This repository opts into the catalog adapter because its test suite is a
subset of the production app-of-apps catalogue. Other repositories that do not
have that catalogue shape keep using the native mode, which remains the
default.

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

A native-only manifest was the first design. It keeps the suite portable but
duplicates the test-relevant projects, namespaces and applications in TOML.
Because this repository already maintains those definitions in
`apps/values.yaml`, and the e2e subset in `hack/e2e/apps.yaml`, the manifest can
instead opt into a typed, read-only catalog adapter. The trade-off is explicit:
catalog mode removes the duplication but couples the manifest to those YAML
shapes and their conversion rules; native mode stays available for repositories
that do not have them.

## Guide-level explanation

A suite is one TOML file at the repository root. A suite whose repository
already keeps an app-of-apps catalogue can opt into the read-only adapter:

```toml
[project]
name = "k8s-home-lab"
root = "."
base_branch = "main"

[catalog]
apps = "apps/values.yaml"
e2e = "hack/e2e/apps.yaml"
```

Both paths are repository-relative and resolved through the manifest root. The
adapter imports only the applications listed in the e2e catalogue, reduces the
projects to those referenced by those applications, derives namespaces from
application and project destinations, and imports the e2e Helm repositories.
Applications, projects and namespaces from native tables must be absent in this
mode; mixing the two modes is a validation error. This repository uses the
catalog form above and keeps the cluster, Argo CD, render, telemetry and
registry tables.

A suite can instead declare the resources natively, which remains the default
and is the right choice when the repository has no compatible YAML catalogue:

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
| `[catalog]` | no | Read-only adapter for `apps/values.yaml` and `hack/e2e/apps.yaml`. Mutually exclusive with the native project, namespace, application and Helm repository tables. |
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

Native lists are serialized exactly as written. In catalog mode the adapter
reproduces the `apps` chart's template defaults: a project with neither cluster
list gets an empty cluster whitelist, and a project with neither namespace list
gets the default blacklist `ResourceQuota`, `LimitRange` and `NetworkPolicy`.
An explicit `namespaceResourceBlacklist: []` in the YAML remains an opt-out:
the adapter preserves it as an empty blacklist. Declaration order is preserved.

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
| `source` | path | required | Application directory, relative to `root`. In catalog mode this is the production `source.path` with the `overlays/kind` convention applied. |
| `renderer` | `helm` or `kustomize` | required natively; derived in catalog mode | Rendering strategy (RFD 0004). Catalog mode uses `helm` when the resolved source has `Chart.yaml`, otherwise `kustomize`. |
| `values`, `common_values`, `inline_values` | list of paths, path, table | `[]`, unset, `{}` | Helm values sources, merged in that order. Catalog mode copies `source.helm.valueFiles` and appends `values-kind.yaml` for Helm apps when that file exists and is not already listed. |
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

In catalog mode a production `syncPolicy` maps `automated`, `prune`,
`self_heal` and `sync_options`; an application without one keeps the defaults
above. The production retry block is not copied, because the e2e suite has its
own retry budget: the e2e `retry_limit` override wins (notably `spegel = 0`),
otherwise the default 20 attempts / 5s backoff / 5m maximum applies.

In catalog mode the `kind_source_path` convention is applied while converting
the application, and an `overlays/kind` source also adds the original catalog
path to `watch`, so a change to a base directory still selects the app.

For native `renderer = "kustomize"`, `source` is resolved in this order:

1. If `source` already contains `kustomization.yaml`, use it unchanged. This
   makes an explicit `source = "mosquitto/overlays/kind"` valid.
2. Otherwise, if `<source>/overlays/kind/kustomization.yaml` exists, use
   `<source>/overlays/kind`. This is the existing `kind_source_path`
   convention.
3. Otherwise, use `<source>`.

For native `renderer = "helm"`, `source` must contain `Chart.yaml`. A
`values-kind.yaml` is not appended automatically; list it in `values`. The
legacy harness appended it by filename, which made a file's presence a hidden
configuration input. Catalog mode deliberately matches the legacy harness
instead: it appends `values-kind.yaml` for a Helm app when the file exists and
is not already listed, after the production value files.

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

Catalog mode runs in the same load step. A manifest that combines `[catalog]`
with any native project, namespace, application or Helm repository table is
rejected. The adapter fails clearly when an e2e app has no production
application, an application references an undeclared project, an e2e
prerequisite is not declared, an application has no destination namespace and
its project has none or starts with `*`, or a catalog source does not exist or
has neither `Chart.yaml` nor a kustomization. Namespaces that are referenced
only by application or project destinations are synthesized as empty specs when
the production catalogue does not declare them, which covers cluster-created
namespaces such as `kube-system` and `cnpg-system`.

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
`argocd.install` path, every configured `project.shared_paths` entry, and the
Astrarium source tree when it is inside `project.root`. This repository lists
`apps/values.yaml` and `hack/e2e/apps.yaml` as shared paths, so a catalog change
selects every app; a change to the manifest itself already does.

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

In native mode the manifest is not generated from `apps/values.yaml`; later
production changes are ported by hand and the manifest contains only the
test-relevant applications. In catalog mode the adapter reads both catalogues
on every load, so there is no stored conversion to drift: the suite always
reflects the current e2e subset, project defaults and namespace labels. The
adapter is still read-only and never deploys the production `apps` chart. It
ignores production keys it does not consume, such as chart-wide repository
settings, so those cannot fail a suite. Native mode remains available for
repositories whose catalogue shape does not match this one.

## Drawbacks

- Catalog mode couples the suite to the production YAML shapes and to the
  adapter's conversion rules. A new production field is invisible until the
  adapter learns it, and the production `apps` chart remains an implicit
  schema. Native mode avoids that coupling but duplicates the test-relevant
  projects, namespaces and applications; TOML is also verbose for the nested
  Kubernetes-shaped lists and `[[apps.resources]]` entries.
- Catalog mode leaves `probe = false`: the adapter does not inspect charts for
  PostSync hooks. The run orchestrator does not use the flag, so this is
  metadata loss rather than a behaviour change.
- Catalog mode applies the legacy `values-kind.yaml` filename convention, so a
  file's presence is again a hidden input for Helm apps. This is deliberate
  parity with the shell harness.
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

**Native-only manifest.** A manifest that repeats every project, namespace and
application is fully self-contained and stays portable across repositories.
This was the first choice, and it remains the default and the right mode when a
repository has no compatible app-of-apps catalogue. Its cost is duplication:
every production project change and namespace label change must be ported by
hand, and the manifest is verbose.

**Typed catalog adapter.** This repository now opts into a read-only adapter
because its e2e suite is a subset of `apps/values.yaml` plus
`hack/e2e/apps.yaml`. The adapter imports the four resource lists before normal
validation, so selection, rendering and run code do not change. The trade-off
is coupling: the YAML shapes and the conversion rules become an implicit
schema, and a future production field is ignored until the adapter models it.
The earlier native-only decision was motivated by portability; keeping native
mode as the default preserves that, while this repository avoids a second copy
of its catalogue. The adapter is read-only and does not bootstrap the
production `apps` chart, so the deployment boundary is unchanged.

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

- A converter from a catalog-backed manifest to native TOML tables, for a
  repository that stops maintaining a compatible YAML catalogue.
- Adapter support for additional catalogue shapes, behind explicit `[catalog]`
  options rather than more production special cases.
- `astrarium plan --explain <app>` to print the full dependency and selection
  reasoning for one application.
- Manifest fragments (`astrarium.d/*.toml`) for suites that outgrow one file.
- Selection by Kubernetes object identity, so a changed CRD selects every app
  that renders that kind and closes the semantic gap that path selection leaves.
