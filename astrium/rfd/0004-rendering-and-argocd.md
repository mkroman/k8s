---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: published
discussion: "https://github.com/mkroman/k8s/pull/435"
labels: [software, argocd]
---

# RFD 0004: Rendering and Argo CD

## Summary

Astrarium renders every selected application into a self-contained Helm chart,
serves the charts from an ephemeral HTTP repository, and creates the Argo CD
`Namespace`, `AppProject`, and `Application` resources directly with typed
Kubernetes models. There is no root `apps` Application and no bootstrap chart.

Helm applications are packaged as their real upstream charts, with dependencies
built and values passed through `source.helm.valuesObject`. Kustomize
applications are built with `kustomize build` and wrapped in a generated chart
whose only template is `{{ .Files.Get "manifests.yaml" }}`.

## Motivation

The legacy harness serves the repository to Argo CD through a local Git daemon
and renders the production `apps` chart into a root Application with a filtered
`valuesObject`. That has four costs:

- Argo CD deploys the committed `HEAD`, not the working tree, so the runner must
  refuse to start when an application's path is dirty.
- The test is coupled to the production chart's root Application, sync-wave
  layout, and values model. Selecting one application means rendering the whole
  catalogue with most entries removed.
- Kustomize applications are rendered by the repo server from a Git path,
  dragging in its API-discovery cache and CRD-ordering races.
- The runner pauses children, fetches rendered manifests through the CLI, and
  unpauses them after a dry-run gate; self-heal can revert each step.

A chart repository removes the Git source entirely. Astrarium renders from the
working tree, so uncommitted changes are testable and the clean-tree
precondition disappears. Each Application has one immutable source: a chart
name and version in a run-scoped repository.

## Guide-level explanation

`astrarium run --app mosquitto` performs these steps:

1. Resolve the application and its transitive prerequisites (RFD 0003).
2. Render each application into `<work>/charts/`:
   - Helm: copy the chart to a temporary directory, run `helm dependency build`
     with bounded retries, then `helm package`.
   - Kustomize: run `kustomize build`, validate the YAML documents, write a
     wrapper chart, then `helm package`.
3. Write `index.yaml` with `helm repo index` pointing at
   `http://<host>:<port>/<run-id>/`, then start an axum server on `0.0.0.0:0`
   that serves only `<work>/charts/`. The bound port is the repository port.
4. Create the kind cluster and install Argo CD (RFD 0005).
5. Create the selected Namespaces with server-side apply.
6. Server-side dry-run the rendered resources. CRD-backed kinds that the
   cluster does not serve yet are skipped and listed; they belong to a
   prerequisite that has not synced.
7. Create the selected AppProjects, with the run's chart repository appended to
   `spec.sourceRepos` unless the project already allows `*`.
8. Create Applications in dependency order. An Application is created only
   after its prerequisites are healthy; independent Applications at the same
   depth are created concurrently. The sync-wave annotation is recorded for
   parity; this loop enforces the order.
9. Wait for each Application, run the declared resource and Prometheus checks,
   collect diagnostics, then stop the chart server and delete the cluster on
   success unless `--keep-cluster`.

`astrarium render --app mosquitto --output ./rendered` stops after step 2 and
writes the packaged chart and a `values.json` for inspection. It requires
`helm` and `kustomize`, not Docker or a cluster.

## Reference-level explanation

### Helm applications

A Helm application's `source` points at a chart directory containing
`Chart.yaml`. Astrarium copies the chart to a temporary directory so that
`helm dependency build` cannot modify the repository's working tree. It runs:

```console
helm dependency build <copy> --skip-refresh   # bounded retries on network errors
helm package <copy> --destination <work>/charts
```

HTTP repository dependencies are resolved after Astrarium registers each
`[[helm_repositories]]` entry; OCI dependencies use Helm's registry client.
`--skip-refresh` uses Helm's existing repository index; on a clean machine the
command is retried without it when it fails with an unknown-chart error. Charts
with OCI dependencies (`spegel`, `agentgateway`) resolve them through Helm's
registry client, and a dependency failure happens before the cluster exists.

The packaged chart keeps its upstream name and version, and `targetRevision` is
that exact version. Because the repository URL contains the run ID, two runs
with the same version but different content do not share a repo-server cache.

The final values are deep-merged in the order defined by RFD 0003: chart
defaults, `common_values`, `values` files, `inline_values`. Astrarium passes
them as `spec.source.helm.valuesObject`:

```yaml
spec:
  source:
    repoURL: http://192.168.1.10:41237/7f3c9a1e/
    chart: spegel
    targetRevision: 1.2.3
    helm:
      valuesObject: { serviceMonitor: { enabled: false } }
```

Astrarium does not pass `valueFiles`: the files were merged on the host, and a
second merge inside Argo CD would depend on two implementations. Helm's
precedence is therefore reduced to one layer.

### Kustomize applications

Astrarium runs `kustomize build <resolved-source>`, parses the output as a
multi-document YAML stream, rejects an empty stream, and writes a wrapper
chart:

```yaml
# <work>/wrapper/Chart.yaml
apiVersion: v2
name: astrarium-mosquitto
version: 0.1.0-astrarium.9f2c1a7b3d40
```

```yaml
# <work>/wrapper/templates/manifests.yaml
{{ .Files.Get "manifests.yaml" }}
```

The rendered documents are stored in `<work>/wrapper/manifests.yaml`, outside
`templates/`. Helm only evaluates templates under `templates/`, so the raw
manifests are not re-templated. This matters for resources that legitimately
contain `{{ ... }}` (Prometheus rules, Go-template ConfigMaps). The wrapper's
values are empty; it has no `valuesObject`.

The wrapper name is `astrarium-<app>` and the version is a fixed base plus the
first twelve hex characters of a SHA-256 over the rendered manifests and the
chart metadata. The hash makes a changed render produce a new chart version,
which makes stale repo-server caches visible in the Application status instead
of silent.

Kustomize plugins are not supported in v0.1. A source that requires one fails
validation with a message naming the plugin; silently rendering a partial
result would be worse.

### The chart repository

The repository directory contains one `.tgz` per selected application and an
`index.yaml` generated once:

```console
helm repo index <work>/charts \
  --url "http://<host>:<port>/<run-id>"
```

The axum server binds `0.0.0.0:0`, serves `/<run-id>/` read-only, answers only
`GET` and `HEAD`, sets `Cache-Control: no-store`, and returns 404 outside the
run directory. It starts after all charts are packaged and stops via a
`DropGuard` when the run ends.

The host address is the same address used for the registry mirrors (RFD 0005):
the LAN address by default, with the Docker bridge gateway as the fallback. It
must be reachable from the Argo CD repo-server pod, not just from the host.
Binding to all interfaces for the duration of a run is a deliberate trade-off;
the server exposes only generated charts, and the port is ephemeral.

### Direct Argo CD resources

Astrarium does not apply the `apps` chart. It creates three kinds with typed
models:

- `corev1::Namespace`, with the labels and annotations from `[[namespaces]]`;
- an `AppProject` `CustomResource` whose spec is a direct mapping of the
  manifest's project (RFD 0003);
- an `Application` `CustomResource` for each selected app.

The Application model is a `kube::CustomResource` derive, not an untyped JSON
blob:

```rust
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug)]
#[kube(group = "argoproj.io", version = "v1alpha1", kind = "Application", namespaced)]
#[kube(status = "ApplicationStatus")]
pub struct ApplicationSpec {
    pub project: String,
    pub source: ApplicationSource,
    pub destination: ApplicationDestination,
    pub sync_policy: Option<SyncPolicy>,
    pub operation: Option<Operation>,
}
```

The generated Application has:

- `metadata.namespace = [argocd].namespace`;
- `metadata.annotations["argocd.argoproj.io/sync-wave"] = -10 * height`, where
  `height` is the longest path to a leaf prerequisite. The annotation is inert
  without a root app-of-apps; it is kept for parity with production.
- `spec.project` from the app or `[argocd].project`;
- `spec.source.repoURL` = the run-scoped chart repository URL;
- `spec.source.chart` and `spec.source.targetRevision` = the packaged chart and
  version;
- `spec.source.helm.valuesObject` for Helm apps, omitted for wrappers;
- `spec.destination.server` and `.namespace` from the project destination that
  covers the app;
- `spec.syncPolicy` from the manifest, including `ServerSideApply=true` where
  declared.

Applications with `automated = false` are created with an explicit
`operation.sync` so the first sync still happens. The automated policy
controls later self-heal and prune, not whether the initial deployment occurs.

The AppProject's `sourceRepos` receives the run's chart URL because the URL is
not known until the server binds. AppProjects are applied after the server
starts and before the first Application. The project has no deletion finalizer.

### Hooks and sync waves

Helm hooks are preserved because the real chart is packaged. Argo CD maps them
as follows:

| Helm hook | Argo CD phase |
| --- | --- |
| `pre-install`, `pre-upgrade` | `PreSync` |
| `post-install`, `post-upgrade` | `PostSync` |
| `post-delete` | `PostDelete` |
| `helm.sh/hook: test` | not run |

A chart that uses Helm hooks **must not** also define an Argo CD hook for the
same Application. Defining any Argo hook causes Argo CD to ignore all Helm hooks
for that Application, which silently turns a `post-install` probe into an
ordinary resource. The wrapper charts contain raw manifests that may carry
`argocd.argoproj.io/hook` annotations; those are Argo hooks, not Helm hooks, and
must stay in the Kustomize source, not in a Helm chart.

Sync waves order resources inside one Application; they order Applications only
under a root app-of-apps, which Astrarium does not create. Astrarium orders
Applications with the create-after-prerequisite-healthy rule. A `post-install`
probe runs inside the sync operation, so "healthy" waits for it.

### Server-side dry-run

Before any Application is created, Astrarium applies each rendered document with
server-side apply in dry-run mode, unless `[render].dry_run = false`:

```rust
let params = PatchParams::apply("astrarium").dry_run().validation_strict();
let api = Api::<DynamicObject>::all(client.clone());
api.patch(name, &params, &Patch::Apply(&doc)).await?;
```

The dry-run runs after the Namespaces exist, so admission sees the right
namespace. It rejects a manifest with a hand-written Job `spec.selector`, a
mistyped field, or a rejected Pod Security level in seconds, before Argo CD
starts a sync that would retry for minutes.

CRDs themselves are dry-run normally: server-side apply does not create the
256 KiB `last-applied-configuration` annotation that breaks client-side apply of
large operator CRDs. A rendered resource whose group/version/kind is not served
by the cluster is skipped and reported in the run summary. That is expected for
operator custom resources owned by a prerequisite that has not synced yet; the
Application is created only after the prerequisite is healthy, at which point
Argo CD can apply the resource.

### Wait, revision, and fast-fail

The wait loop polls the Application resource, not the Argo CD CLI:

- Success requires `status.health.status == Healthy`,
  `status.sync.status == Synced`, and no operation in `Running`.
- An operation phase of `Failed` or `Error` is fatal only when
  `status.operationState.retryCount >= spec.syncPolicy.retry.limit` (a negative
  limit means unlimited).
- A transient `Degraded` health is not fatal. A new Prometheus CR is briefly
  `Degraded` while its operator reports `Available=False`; failing on the first
  transition would fail healthy runs. The deadline still applies.
- `--fast-fail` makes the first `Degraded` transition fatal for debugging.
- Independently of health, a container in an app namespace that is
  `CrashLoopBackOff` with at least three restarts fails the run immediately,
  with logs and events attached, instead of waiting out a progress deadline.

The **revision race** is the bug the shell harness fixed: Argo CD can report
`Healthy` and `Synced` while `status.sync.revision` still names the previous
sync. Astrarium polls for up to 30 seconds until the revision matches the
expected chart version (or, for a Helm HTTP source with no revision, until
`status.sync.comparedTo.source.targetRevision` matches). A mismatch after the
grace window is a failure, not a success with a stale revision.

Applications are waited in dependency order. Independent Applications at the
same depth are created concurrently, and their wait loops run in a `JoinSet`;
diagnostics are collected in parallel, but syncs are not reordered.

### Upgrade runs

`astrarium upgrade --app <name>` first deploys the app and its prerequisites
through the normal Argo CD run path. It then packages the app's Helm chart
with the same kind values, installs it into a disposable
`<namespace>-rollout` namespace, and runs the generic in-cluster test:

1. Wait for a Ready pod and assert the restricted security profile.
2. Snapshot the PVC names, UIDs and bind phases.
3. Optionally write a marker below `upgrade.data_path` in the configured
   container.
4. Run `helm upgrade`, delete the release pods so their controllers recreate
   them, and wait for a Ready pod with a new UID.
5. Check the marker survived and the PVC identity and bind state did not
   change.
6. Run `helm uninstall`; unless `verify_pvc_retention` is false, the PVCs must
   still exist.

The old `from_values`/`from_revision` fields remain in the manifest for a
future chart-diff upgrade fixture, but the catalog adapter does not set them.

### Cleanup

On success, the cluster is deleted unless `--keep-cluster` or `keep = true`. On
failure, the cluster is kept by default and the run prints its name and
kubeconfig path; deleting the evidence before diagnostics are read is worse.

Astrarium deletes only clusters whose name starts with `astrarium-`. A cluster
named by `cluster.reuse = true` is never deleted, even on success. On a reused
cluster, Applications no longer selected are deleted with automated sync
disabled first; namespaces are left in place. If the containerd configuration no
longer matches the manifest, the cluster is recreated rather than reused
(RFD 0005).

The chart server, temporary chart directory, and kubeconfig are released by
guards on every exit path, including Ctrl-C. The kubeconfig is removed only if
Astrarium created it.

## Drawbacks

- The wrapper chart hides Kustomize's own health and pruning semantics from Argo
  CD. Argo sees a Helm chart containing plain resources; it does not know that
  `kustomize build` produced them.
- Local rendering with Helm 3 while Argo CD renders with Helm 4 (Argo CD 3.5+)
  means packaging and rendering use different engines. A chart that relies on a
  Helm 4-only feature can pass packaging and fail at sync.
- `valuesObject` loses the provenance of which file set which value. Rendering
  a diff against the source values files is harder than with `valueFiles`.
- The chart repository URL and port are ephemeral. A failed run leaves an
  Application pointing at a dead URL; that is acceptable only because the
  cluster is disposable.
- Building Helm dependencies locally requires registry access from the host.
  Argo CD could resolve them from inside the cluster with its own repository
  configuration; Astrarium cannot.
- Creating Applications directly means Astrarium owns the app-of-apps subset.
  Features of the production `apps` chart that are not modelled in
  `astrarium.toml` (custom finalizers, ApplicationSets, extra annotations) do
  not exist in the test cluster.

## Rationale and alternatives

**Keep the Git daemon and the `apps` chart.** This preserves parity with
production and avoids a chart repository. Rejected because it forces the
clean-tree precondition, couples every test to the root Application, and makes
subset selection a filtering exercise inside the chart.

**OCI chart repository instead of HTTP.** OCI would avoid `helm repo index` and
give content-addressed storage, but it requires a registry that Argo CD can
reach. The local registry mirrors exist (RFD 0005), but using one for charts
would mix test images and test charts in the same pull-through cache. HTTP is
simpler and explicitly acceptable.

**Apply rendered manifests directly with `kubectl`/`kube`, without Argo CD.**
Faster and simpler, but it would stop testing Argo CD's health assessment,
sync waves, hook mapping, and CRD ordering. Those are exactly the behaviours
this project exists to test.

**Use `argocd app sync` and `argocd app wait`.** Rejected because the CLI
requires the Argo CD server, which the e2e overlay scales to zero, and because
`app wait --health` treats `Progressing -> Degraded` as fatal.

**Put the wrapper manifests directly in `templates/`.** Rejected because Helm
would evaluate `{{ ... }}` inside the Kustomize output; `.Files.Get` preserves
raw content.

## Prior art

- `hack/e2e/run-app.sh` contributes the dry-run gate, the revision check, and
  the CrashLoopBackOff fast-fail; the local dry-run replaces its pause trick.
- [Argo CD's Helm documentation](https://argo-cd.readthedocs.io/en/stable/user-guide/helm/)
  defines `valuesObject` precedence and the Helm-to-Argo hook mapping.
- [Helm's chart repository format](https://helm.sh/docs/topics/chart_repository/)
  defines `index.yaml` and the `--url` flag used to make archive URLs absolute.
- [Kustomize](https://kustomize.io/) defines the build semantics; the wrapper
  chart is a packaging detail, not a second renderer.
- [kube-rs](https://kube.rs/) provides the `CustomResource` derive and the
  server-side apply parameters used for the dry-run.

## Unresolved questions

- **Deferred:** should the upgrade fixture also support `from_values` (a
  second values file) or `from_revision` (the previous commit)? The current
  test exercises pod replacement, data persistence and PVC retention with the
  same chart; a chart-diff fixture would test the rendered manifest change.
- **Can be deferred:** should Astrarium register the chart repository as an
  Argo CD repository Secret so the UI shows a name and future authentication has
  a home? The direct URL works today.

## Future possibilities

- An OCI chart repository as an alternative transport, reusing the same
  packaging code.
- Multi-source Applications for charts that need a values file from a second
  repository, which the current single-source model cannot express.
