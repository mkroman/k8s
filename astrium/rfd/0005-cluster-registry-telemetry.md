---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: published
discussion: "https://github.com/mkroman/k8s/pull/435"
labels: [software, observability, testing]
---

# RFD 0005: Cluster, registry, and telemetry

## Summary

Astrarium creates a three-node kind cluster, mounts containerd `hosts.toml`
files that point image pulls at the path-prefixed `registry.rwx.im` proxy,
installs a minimal Argo CD from a Kustomize overlay, and exports one OTLP trace
per run to Jaeger. `astrium/compose.yaml` provides Jaeger v2 with in-memory
storage. The binary talks to Docker through its API, not the `docker` CLI, and
invokes only `helm`, `kustomize`, and `kind`.

## Motivation

The legacy harness builds its kind config around Spegel (registry
`config_path`, layer retention, isolated content sharing), but CI pulls images
directly, so a Docker Hub rate limit or a flaky registry becomes an unrelated
e2e failure. There is also no trace: a failed run is console groups and, at
best, artifacts.

Three problems follow:

- Registry behaviour is part of what e2e tests. A pull-through mirror makes
  pulls deterministic and keeps the Spegel probe meaningful.
- A kind node is a container. `127.0.0.1` inside a node is the node itself, so
  the mirror address must be the host's address on the kind Docker network.
- A run has phases, applications, and retries. A flat log does not show which
  application consumed the deadline or whether the failure was render, sync, or
  probe. A trace with bounded attributes does.

## Guide-level explanation

Start the supporting services once per machine:

```bash
docker compose -f astrium/compose.yaml up -d
astrarium doctor
```

`doctor` checks that `helm`, `kustomize`, and `kind` are on `PATH`, that the
Docker API and Jaeger's OTLP port answer, and that each mirror responds. It
prints the resolved mirror endpoints, so a networking problem is visible
before the cluster exists.

Then run an application:

```bash
astrarium run --app mosquitto
```

The run creates `astrarium-<short-id>`, writes its kubeconfig to
`.astrium/kubeconfig`, installs Argo CD, deploys the application, and exports
spans to `http://localhost:4317`. Open <http://localhost:16686> and select the
`astrarium` service. One trace contains the pipeline span, one task span per
phase and application, and the error attributes for the first failure.

On success the cluster is deleted. On failure it is kept and the run prints:

```console
cluster astrarium-7f3c9a1e kept for debugging
kubeconfig .astrium/kubeconfig
```

## Reference-level explanation

### Cluster lifecycle

`[cluster]` in `astrarium.toml` supplies `provider` (only `kind` in v0.1),
`name`, `kind_config`, `kubeconfig`, `reuse`, and `keep`. `name` is the base
name and must start with `astrarium` in v0.1, so every generated name starts
with `astrarium-`:

- a fresh run creates `<name>-<short-id>`, where the short ID is derived from
  the run ID;
- `reuse = true` uses `<name>` exactly, requires it to exist, and never deletes
  it;
- `cluster create` creates `<name>-<short-id>`; `cluster list` shows `astrarium`
  and `astrarium-*`; `cluster delete` refuses a name without the `astrarium-`
  prefix (the reused `astrarium` cluster is never deleted by Astrarium).

The kind config is loaded from `cluster.kind_config`, parsed with `yaml_serde`,
and written to a generated file. Astrarium preserves the repository's nodes,
kubeadm patches, and containerd patches, and adds:

```yaml
containerdConfigPatches:
  - |-
    [plugins."io.containerd.grpc.v1.cri".registry]
      config_path = "/etc/containerd/certs.d"
    [plugins."io.containerd.grpc.v1.cri".containerd]
      discard_unpacked_layers = false
    [plugins."io.containerd.metadata.v1.bolt"]
      content_sharing_policy = "isolated"
extraMounts:
  - hostPath: <state>/certs.d
    containerPath: /etc/containerd/certs.d
    readOnly: true
```

The patch must not carry a `version` selector. kind silently skips a patch whose
version does not match the node's containerd config, which turns a broken mirror
setup into a test that passes for the wrong reason.

After creation, Astrarium verifies the **effective** configuration with
`containerd config dump` inside each node, through the Docker API: `config_path`,
`discard_unpacked_layers`, and `content_sharing_policy` when the bolt section
exists. Reading the file is not enough; kind merges patches, and the merged
result is what containerd uses. The same API removes `e2e.pullImage` from every
node for cold-image probes (`ctr -n k8s.io images rm --sync`, `crictl` fallback).

The cluster is created with `kind create cluster --config <generated> --wait
180s`; the kubeconfig is written to `cluster.kubeconfig` and the context is set
per run, so the user's current context is not changed.

### Registry mirrors

Workload images pull through `registry.rwx.im`, which stores repositories
under an upstream prefix:

```text
registry.rwx.im/docker.io/library/busybox
registry.rwx.im/ghcr.io/example/app
```

Containerd mirrors normally append `/v2` to the mirror endpoint and preserve
the repository path. That cannot address a prefixed repository directly, so
each mirror sets `override_path = true` and uses the proxy's API root as the
endpoint:

```toml
# <state>/certs.d/docker.io/hosts.toml
server = "https://registry-1.docker.io"

[host."https://registry.rwx.im/v2/docker.io"]
  capabilities = ["pull", "resolve"]
  override_path = true
```

The same pattern covers `ghcr.io`, `quay.io`, `registry.k8s.io`, `gcr.io`,
`mirror.gcr.io`, and `public.ecr.aws`. Image names in charts and manifests stay
unchanged; only the mirror endpoint carries the prefix.

Registry credentials use kind's recommended node-level mechanism. A plain
Docker `config.json` is named by `ASTRARIUM_REGISTRY_AUTH_FILE`; Astrarium adds
an `extraMount` to every node at `/var/lib/kubelet/config.json`. The file must
contain base64-encoded credentials in `auths`; credential helpers are not
available inside a node. CI writes it from the `REGISTRY_USER` and
`REGISTRY_PASSWORD` repository secrets.

**Host-side images bypass the node mirrors.** kind pulls `kindest/node` with
the host Docker daemon before any node exists. The e2e workflow pulls the
prefixed `kindest/node` and Jaeger images through the registry, retags them
with their original names, and then starts kind and compose. The image names
in `kind.yaml` and `compose.yaml` remain unchanged.

`astrarium doctor` still checks each configured mirror endpoint over TCP. It
does not exercise registry authentication; the first image pull does.

### Argo CD installation

Astrarium runs `kustomize build <argocd.install>`, applies the CRDs first with
server-side apply, waits for `Established`, then applies the rest with
server-side apply and `force = true`. The force is scoped to the Argo CD
installation, where a reused cluster may have fields from an older install; it
is not used for user applications.

The e2e overlay scales `argocd-server`, `argocd-dex-server`,
`argocd-notifications-controller`, and `argocd-applicationset-controller` to
zero. Astrarium waits for `statefulset/argocd-application-controller`,
`deployment/argocd-repo-server`, and Redis if the overlay keeps it.

No repository Secret is required: every chart is packaged with its dependencies
and served by URL. No Argo CD admin credential is used: Astrarium speaks the
Kubernetes API with the run's kubeconfig and never calls the Argo CD API or CLI.

### Telemetry

Astrarium creates **one** `SdkTracerProvider` per process, with a
`BatchSpanProcessor` and an OTLP/gRPC exporter built with
`SpanExporter::builder().with_tonic()`:

```rust
let exporter = SpanExporter::builder().with_tonic()
    .with_endpoint(&config.otlp_endpoint)
    .build()?;
let provider = SdkTracerProvider::builder()
    .with_batch_exporter(exporter)
    .with_resource(resource)
    .build();
let tracer = provider.tracer("astrarium");
let layer = tracing_opentelemetry::layer().with_tracer(tracer);
```

`provider.shutdown()` runs on every exit path. If the endpoint is unavailable,
Astrarium logs a warning and continues; a missing collector must not fail e2e.
Sampling is parent-based and always-on for e2e. There is no head sampler that
could drop a failed run.

#### Trace files

`--trace-file <path>` (or `ASTRARIUM_TRACE_FILE`) installs a second
`BatchSpanProcessor` whose exporter writes the same spans as OTLP JSON. The
file contains `resourceSpans`, `scopeSpans`, and `spans`; trace/span IDs are
lowercase hex, nanosecond timestamps and `intValue` attributes are decimal
strings, and the resource carries the same service and VCS attributes as the
network exporter. It is written atomically at shutdown, so a failed run still
leaves a file (the provider shutdown is best effort and does not change the
exit status).

The format is the OpenTelemetry OTLP JSON encoding. Jaeger 2.23's Search page
accepts OTLP JSON directly through its file upload control (enabled by default),
so the file can be inspected without a collector, storage backend, or network
access. The e2e workflow uploads `diagnostics/astrarium-trace.json` as an
artifact for every app job. A legacy Jaeger JSON file is not used because OTLP
JSON is already the native export of the SDK and remains useful to other OTLP
viewers (Grafana, otel-tui, and the OTLP JSON viewers).

Resource attributes:

| Attribute | Value |
| --- | --- |
| `service.name` | `astrarium` (or `[telemetry].service_name`) |
| `service.version` | Cargo package version |
| `service.instance.id` | Run ID |
| `deployment.environment.name` | `[telemetry].environment`, when set |
| `vcs.repository.url.full` | Repository URL, when known |
| `vcs.ref.head.revision` | `HEAD` revision from `gix` |

Spans use the OpenTelemetry CI/CD conventions. The root span is kind `SERVER`;
task spans are kind `INTERNAL`:

| Rust span | OTel span name | Kind | Attributes |
| --- | --- | --- | --- |
| `run` | `RUN astrarium` | `SERVER` | `cicd.pipeline.name`, `cicd.pipeline.run.id`, `cicd.pipeline.action.name=RUN`, `cicd.pipeline.result`, `error.type` |
| `cluster.create` | `create cluster` | `INTERNAL` | `astrarium.cluster.name`, `astrarium.cluster.host_ip` |
| `argocd.install` | `install argocd` | `INTERNAL` | `k8s.namespace.name` |
| `app.render` | `render helm chart` / `render kustomize app` | `INTERNAL` | `astrarium.app.name`, `astrarium.render.renderer`, `astrarium.chart.version` |
| `app.apply` | `apply application` | `INTERNAL` | `astrarium.app.name`, `astrarium.argocd.application` |
| `app.wait` | `wait application` | `INTERNAL` | `astrarium.app.name`, `astrarium.phase=wait`, `k8s.namespace.name` |
| `app.check` | `run probe` | `INTERNAL` | `astrarium.app.name`, `astrarium.phase=probe` |

A task span carries `cicd.pipeline.task.name` (the application or phase),
`cicd.pipeline.task.run.id`, `cicd.pipeline.task.run.result`, and
`cicd.pipeline.task.run.url.full` when a GitHub run URL is available.

Custom attributes use **only** the `astrarium.*` namespace:
`astrarium.app.name`, `astrarium.phase`, `astrarium.cluster.name`,
`astrarium.cluster.host_ip`, `astrarium.argocd.application`,
`astrarium.render.renderer`, `astrarium.chart.version`,
`astrarium.content.hash`. Standard attributes are used where they exist:
`k8s.namespace.name`, `k8s.pod.name`, `k8s.deployment.name`, `k8s.job.name`,
`error.type`, `exception.type`, `exception.message`. Astrarium never invents a
`k8s.*`, `cicd.*`, or `service.*` name.

Span names are low-cardinality verb + object. Application names, namespaces,
revision SHAs, pod names, chart versions, and error messages go in attributes,
never in span names. `otel.name` may override a Rust span name only for a stable
variant such as `render helm chart` versus `render kustomize app`.

`error.type` is a bounded enum, not a message: `manifest_invalid`,
`render_failed`, `cluster_create_failed`, `argocd_install_failed`,
`apply_failed`, `sync_failed`, `timeout`, `probe_failed`,
`diagnostics_failed`, `cancelled`. `cicd.pipeline.result` uses `success`,
`failure`, `timeout`, `cancellation`, `error`, or `skip`. A span status is set
to error, with `error.type`, only on failure; success does not set it.

Metrics are out of scope for v0.1. When they are added, they use the CI/CD
metric conventions (`cicd.pipeline.run.duration`,
`cicd.pipeline.run.active`, `cicd.pipeline.run.errors`) without a `_total`
suffix.

### Diagnostics and redaction

On failure, Astrarium collects:

- Application sync, health, operation phase, retry count, and revision;
- pod phase, container states, restart counts, exit codes, and node names;
- events sorted by last timestamp;
- current and previous logs, last 100 lines per container;
- Argo CD application-controller and repo-server error lines;
- rendered manifest and values hashes;
- phase timings.

It writes `diagnostics.json` and `summary.md` under the run directory. CI
uploads them on failure (RFD 0006) and appends `summary.md` to
`$GITHUB_STEP_SUMMARY`. The JSON is deliberately structured:

```json
{
  "run_id": "1764043200000-4242",
  "app": "mosquitto",
  "phase": "wait",
  "result": "failure",
  "error_type": "sync_failed",
  "application": { "health": "Degraded", "sync": "OutOfSync", "revision": "0.1.0-..." },
  "pods": [{ "namespace": "mosquitto", "name": "mosquitto-...", "restarts": 3 }],
  "events": [{ "reason": "BackOff", "message": "Back-off restarting failed container" }]
}
```

Redaction is applied before anything is written or exported:

- Secret `data`/`stringData` values are removed entirely, and environment
  variables matching `(?i)(password|token|key|secret)` become `<redacted>`.
- Command arguments are sanitized (`--password=<redacted>`), and kubeconfigs
  are never included; only the path is recorded.
- Helm values and span attributes are included only after the same key-based
  redaction. Attributes never contain raw output, YAML, kubeconfigs, Secret
  values, or full error messages; `exception.message` is never `error.type`.

Argo CD logs are filtered to error-shaped lines before they enter diagnostics,
because the repo server logs one JSON line per rendered manifest.

## Drawbacks

- The tool needs Docker, a compose stack, and a host address reachable from
  containers: heavier than the shell harness, which only needs Docker and kind.
- The chart server binds to all interfaces and lives for one run.
- Workload pulls depend on the external registry proxy and its availability.
  The mirror configuration and credentials are part of the suite.
- Jaeger v2 with in-memory storage loses traces when the container restarts.
  That is acceptable for a test tool but not for trend analysis.
- The CI/CD semantic conventions are still stabilizing. Attribute names may
  change, and Astrarium would either pin an old convention or rename attributes
  in a breaking change.
- The registry mirror is another cache to invalidate. Node images bypass the
  node mirrors, so CI pre-pulls them through the proxy and retags them before
  kind starts.

## Rationale and alternatives

**kind instead of k3d or a remote cluster.** kind is already the harness's
cluster provider, it supports containerd patches and extra mounts, and the
repository's config encodes real requirements (Spegel, kubeadm bind addresses,
three nodes). A remote cluster is not disposable.

**A path-prefixed hosted proxy instead of local caches.** An in-cluster
registry would need a node before the node can pull, which is circular. Local
caches keep the failure domain local, but duplicate the same proxy per
upstream and still need credential handling. The hosted proxy gives one
consistent source for CI and developers; `override_path = true` lets containerd
address the prefixed repositories without rewriting image names.

**Hardcode the host IP instead of `local-ip-address`.** A hardcoded address is
wrong on every machine except the author's. The crate's default-route address
matches what a container can reach in the common case; the gateway fallback
covers the multi-interface case, and `registry.host` is the escape hatch.

**Docker CLI instead of the Docker API.** Rejected because the binary may
invoke only `helm`, `kustomize`, and `kind`. The Docker API is the same socket
the CLI uses; `bollard` is the smallest client that covers inspection and exec.

**Jaeger v2 instead of Tempo or a hosted collector.** Jaeger v2 is an OTel
Collector distribution with a UI and in-memory storage, exactly the scope of one
run. Tempo would add object storage; a hosted collector would add credentials
and a network dependency.

**OTLP/gRPC instead of OTLP/HTTP.** gRPC is the default for the Rust SDK's tonic
exporter and uses port 4317, which Jaeger exposes. HTTP on 4318 is available but
is not the primary path.

## Prior art

- [kind's configuration documentation](https://kind.sigs.k8s.io/docs/user/configuration/)
  defines `containerdConfigPatches` and `extraMounts`, including the silent
  version-selector behaviour.
- [kind private registries](https://kind.sigs.k8s.io/docs/user/private-registries/)
  documents mounting a plain Docker `config.json` at
  `/var/lib/kubelet/config.json`.
- [containerd hosts.toml](https://containerd.io/docs/2.3/hosts/) defines
  `override_path`; the [kind-registry-mirror action](https://github.com/RocketPadPlatforms/github-actions/pull/22)
  uses the same `<mirror>/v2/<registry>` + `override_path = true` pattern.
- [Jaeger v2](https://www.jaegertracing.io/docs/2.0/) is an OTel Collector
  distribution; the all-in-one image accepts OTLP on 4317/4318 and serves the UI
  on 16686.
- [OpenTelemetry CI/CD semantic conventions](https://opentelemetry.io/docs/specs/semconv/cicd/cicd-spans/)
  define the pipeline and task spans, results, and `error.type` used here.

## Unresolved questions

- **Must answer before `published`:** is `bollard` acceptable, or should node
  inspection and exec go through a privileged DaemonSet with the containerd
  socket mounted? `bollard` adds no cluster resources but assumes a Docker
  socket.
- **Must answer before `published`:** does "Docker or Podman" include Podman?
  `bollard` speaks the Docker API; Podman's compatibility socket may work, but
  kind and compose are untested with it.
- **Can be deferred:** which version of the CI/CD semantic conventions is
  pinned? The names here are the current release-candidate set; a constant in
  the telemetry module should record the choice.

## Future possibilities

- Metrics for run duration, active runs, and per-application results.
- A remote OTLP endpoint for shared CI runs, with authentication and sampling
  configured per environment.
- Trace links from the run span to the GitHub Actions run and from each task
  span to the Argo CD Application URL.
- Parallel kind clusters for independent applications with a shared trace.
