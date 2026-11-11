# Application e2e tests

These scripts deploy real Argo CD Applications into a throwaway kind cluster.
They reuse the production `apps` chart, `AppProject` definitions and
application manifests instead of carrying a second copy of the app catalogue.

## Flow

1. `list-changed.sh` compares the pull request with its base and emits a
   GitHub Actions matrix. Only applications whose files or Application values
   changed are selected.
2. `run-app.sh APP` resolves the app's `requires` graph from `apps.yaml`.
3. A three-node kind cluster (control-plane plus two workers) is created from
   `kind.yaml`.
4. The minimal Argo CD control plane is installed from `argocd/` (the
   application controller, repo server and Redis; the UI and optional
   components are scaled to zero).
5. The `apps` chart is rendered into a root `Application` with an inline
   `valuesObject` containing only the selected app and its prerequisites.
   Child Applications are ordered with `argocd.argoproj.io/sync-wave`.
6. The runner waits for every prerequisite and the selected app to be
   `Synced` and `Healthy` at the tested revision.

Argo CD reads the checkout from a local Git daemon, so the test deploys the
exact commit under test, including a pull request's merge commit. No
prerequisite CRDs are copied or applied by hand.

## Local iteration

The first run creates the kind cluster and installs Argo CD, which dominates
local cycle time. Keep that cluster and reuse it for subsequent runs:

```bash
# First run: create the cluster and leave it behind.
KEEP_CLUSTER=1 ./hack/e2e/run-app.sh spegel

# Subsequent runs: skip cluster creation and reuse the installation.
REUSE_CLUSTER=1 ./hack/e2e/run-app.sh spegel
```

`REUSE_CLUSTER=1` requires the cluster to exist and skips the cleanup
deletion. The runner still reapplies Argo CD and the Application manifests, so
the tested revision is always the current checkout. It disables root auto-sync,
deletes every child Application (stale Applications first, then the current
dependency chain in reverse order), and fails loudly if an Application cannot
be deleted. Namespaces are left in place. Use the same `CLUSTER_NAME` (default
`e2e-<app>`) when reusing a cluster. `REUSE_CLUSTER` and `KEEP_CLUSTER` accept
`1`/`true`/`yes` and `0`/`false`/`no`, case-insensitively.

The runner establishes the node preconditions on every run, fresh or reused.
For an app with `requires_fresh_image` (currently `spegel`), it first removes
resources declared under `stale_probe_resources`: post-delete hook resources
that the previous Application deletion left behind, and force-deleted pods
matching the configured selectors (probe-created pods and leaked cleanup-hook
pods). It waits for them to disappear so their containers cannot keep the image
in use. It then removes the image from `.e2e.pullImage` in the app's
`values-kind.yaml` from every kind node. For an app with
`requires_containerd_config`, it verifies that
`/etc/containerd/config.toml` on every node has the mirror `config_path`, does
not set `discard_unpacked_layers = true`, and sets
`content_sharing_policy = "isolated"`. Argo CD retries use the app's
`retry_limit`; cold-image probes set `0` because a retry would reuse the image
pulled by the failed attempt. If the containerd check fails, recreate the
cluster so the patch from `kind.yaml` is applied. Tear it down with:

```bash
kind delete cluster --name e2e-spegel
```

For faster failure feedback while iterating, lower the local budgets:

```bash
REUSE_CLUSTER=1 \
  E2E_RETRY_LIMIT=1 \
  E2E_RETRY_MAX_DURATION=30s \
  E2E_TIMEOUT_SECONDS=300 \
  E2E_REFRESH_TIMEOUT_SECONDS=60 \
  ./hack/e2e/run-app.sh spegel
```

CI keeps the defaults: a fresh cluster, `E2E_RETRY_LIMIT=20`,
`E2E_RETRY_MAX_DURATION=5m`, `E2E_TIMEOUT_SECONDS=600`, and
`E2E_REFRESH_TIMEOUT_SECONDS=120`, so coverage is unchanged. `E2E_RETRY_LIMIT`
and `E2E_TIMEOUT_SECONDS` are defaults only: an app's explicit `retry_limit` or
`timeout_seconds` in `apps.yaml` takes precedence (`monitoring` uses 900).
`E2E_RETRY_MAX_DURATION` must be a Go duration such as `30s` or `5m`, and
`E2E_RETRY_LIMIT` must not have leading zeros.

## Currently covered

| Application | Source | Prerequisites |
| --- | --- | --- |
| `agentgateway` | Helm, OCI dependencies | `gateway-api`, `prometheus-operator` |
| `cert-manager` | Helm | none; ServiceMonitor disabled in kind |
| `cluster-exporters` | Helm | `prometheus-operator` |
| `cloudnative-pg` | Kustomize | `cert-manager`, `prometheus-operator` |
| `gateway-api` | Kustomize | none |
| `grafana` | Helm | `prometheus-operator`; checks the renderer scrape path |
| `monitoring` | Kustomize | `prometheus-operator`; asserts the control-plane scrape targets are up |
| `mosquitto` | Kustomize | none; PostSync probe does an MQTT publish/subscribe round-trip |
| `node-feature-discovery` | Helm | `prometheus-operator`; asserts the bundled NodeFeatureRule and PodMonitor |
| `prometheus-operator` | Kustomize | `cert-manager`; asserts its ServiceMonitor is deployed |
| `sealed-secrets` | Kustomize | `prometheus-operator`; asserts its PodMonitor is deployed |
| `spegel` | Helm, OCI dependencies | none; ServiceMonitor disabled in kind; PostSync probe runs a two-node P2P pull test |
| `traefik` | Helm | `prometheus-operator` |
| `vector-agent` | Helm | `prometheus-operator` |
| `vector-aggregator` | Helm | `prometheus-operator` |
| `zigbee2mqtt` | Helm | `mosquitto`; runs a Z-Stack emulator sidecar instead of real hardware; PostSync probe waits for the `zigbee2mqtt/bridge/state` topic |

`longhorn` is not e2e-enabled because kind cannot run it; its chart and
ServiceMonitor are covered by the Helm validation workflow instead.

`agentgateway` reuses the production OCI Helm repository registration. Its
chart dependencies are resolved by Argo CD, not by the runner.

## Behavioural checks

Charts can ship e2e-only probes as Helm hooks gated by `.Values.e2e.enabled`.
`grafana/templates/e2e/checks.yaml` is the reference: it runs PostSync Jobs
after Grafana is Healthy, checks that a Prometheus-labelled pod can scrape the
image renderer, checks that an unlabelled pod is still blocked by the
NetworkPolicy, and runs a control request against Grafana's health endpoint.
The Jobs are enabled from `values-kind.yaml`; Argo CD maps the Helm hook
annotations to its own PostSync hooks, so a failed probe fails the Application
sync. No runner-specific check code is needed.

Kustomize apps use the same pattern with Argo CD's own hook annotations:
`mosquitto/overlays/kind` renders a PostSync Job (annotated
`argocd.argoproj.io/hook: PostSync`) that publishes to an `e2e/probe` topic and
subscribes it back, proving the broker accepts connections. The overlay also
switches the Service to ClusterIP (kind has no LoadBalancer implementation),
replaces the SealedSecret — a fresh sealed-secrets controller generates its own
key pair and cannot decrypt secrets sealed for another cluster — with an
anonymous-listener config, and adds `fsGroup` so the local-path PVC is writable
by the non-root mosquitto user.

The `zigbee2mqtt` probe proves the whole bridge without Zigbee hardware: the
chart runs a Z-Stack coordinator emulator
([romanlum/zstack-adapter-emulator](https://github.com/romanlum/zstack-adapter-emulator))
as a sidecar, points `serial.port` at it over pod-local TCP through the
documented `adapter: zstack` path, and the PostSync Job subscribes to
`zigbee2mqtt/bridge/state` on the mosquitto broker. Zigbee2MQTT publishes that
topic only after MQTT and the Zigbee stack are up, so a healthy probe means the
deployment, PVC seeding, MQTT connectivity, and emulator startup all worked.
Values live in `zigbee2mqtt/values-kind.yaml`; production values are unchanged.

The `spegel` probe verifies behaviour end to end: pod A pulls a busybox image
that is cached nowhere in the fresh kind cluster (upstream pull, advertised by
the DHT), pod B is forced onto a different node by pod anti-affinity, and the
probe asserts that the mirror-hit counter for the tested registry (`docker.io`
for the configured image) increases by at least two cache hits. Requiring a
manifest plus a blob hit means a manifest-only response cannot satisfy the
assertion. The probe image is pulled before that baseline, so its own pull
cannot satisfy the assertion, and the runner clears the cold test image from
every node before the probe so a reused cluster cannot satisfy it from stale
containerd content. It depends on the containerd mirror patch in `kind.yaml`
(`config_path` plus `discard_unpacked_layers = false`), which is why the patch
lives in the cluster config and not in the chart.

Helm's own `helm.sh/hook: test` tests are only executed by `helm test`, and
Argo CD does not map the Helm test hooks. Mapping `post-install`/`post-upgrade`
to Argo CD's `PostSync` is the supported equivalent. The probes therefore use
`helm.sh/hook: post-install,post-upgrade`, `before-hook-creation,hook-succeeded`
cleanup, and a bounded `activeDeadlineSeconds`. They test behaviour, not just
Argo CD's resource-health status. A probe must either be idempotent or declare
its cold-start preconditions in `apps.yaml`; an Argo CD retry cannot recreate a
cold image that a failed attempt already pulled.

Kustomize apps can use `overlays/kind` for environment-specific composition;
`monitoring/overlays/kind` selects the production-free Prometheus base.

For runtime assertions, an app can declare `prometheus_targets` in `apps.yaml`.
After the Application is Synced and Healthy, `run-app.sh` port-forwards to the
configured Service and checks the target API. Every target for the listed jobs
must report `up`. This is generic; any app with a Prometheus-compatible Service
can use it:

```yaml
  my-app:
    prometheus_targets:
      namespace: monitoring
      service: prometheus
      port: 9090
      jobs:
        - kubelet
```

An app can also declare `resources` that must exist after it is Synced and
Healthy. The runner looks up each object by API version, kind, name and
namespace, which verifies that monitor CRs were actually applied and not just
rendered:

```yaml
  my-app:
    resources:
      - apiVersion: monitoring.coreos.com/v1
        kind: ServiceMonitor
        namespace: monitoring
        name: my-app
```

Resource checks run for prerequisites too, so declaring the object under the
Application that owns it verifies it in every test that deploys that app.

## Adding an app

Add the app to `apps.yaml`:

```yaml
apps:
  my-app: {}
```

If the app needs another e2e app first, declare it:

```yaml
  my-app:
    requires:
      - cert-manager
```

`requires` is transitive. The runner deploys the dependency as a real Argo CD
Application before the selected app.

## Making an app kind-compatible

The app's normal `Application` from `apps/values.yaml` is used. Only two
conventions are applied automatically:

- A Helm chart with `values-kind.yaml` in its chart directory gets that file
  appended to `helm.valueFiles`.
- A Kustomize app with `overlays/kind/kustomization.yaml` is deployed from
  `overlays/kind` instead of the app root.

Use these for environment-specific differences such as cluster-specific
secrets, storage classes, or load-balancer settings. For example,
`cert-manager/values-kind.yaml` disables the production `SealedSecret` because
an ephemeral kind cluster does not have the production sealed-secrets key, and
disables its ServiceMonitor because Prometheus Operator depends on
cert-manager and is therefore not deployed first.

## Running locally

The runner reads the committed `HEAD`, so commit app changes before testing:

```bash
./hack/e2e/run-app.sh sealed-secrets
./hack/e2e/run-app.sh vector-agent
```

Set `KEEP_CLUSTER=1` to leave the kind cluster behind for debugging.

Preview the selected apps for a branch:

```bash
./hack/e2e/list-changed.sh origin/main HEAD
```

To exercise the harness itself without an app change, use the workflow
dispatch input:

```bash
gh workflow run e2e-apps.yaml --ref <branch> -f app=vector-agent
```
