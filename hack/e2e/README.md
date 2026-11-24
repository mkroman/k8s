# Shared e2e inputs

Astrarium is the e2e runner. The remaining files in this directory are inputs
to it, not a separate harness:

- `apps.yaml` is the e2e metadata catalogue read by the typed
  `[catalog]` adapter. It selects the applications exercised by the suite and
  adds `requires`, probe/resource checks, Prometheus targets, timeouts,
  cold-image settings and Helm repositories.
- `kind.yaml` is the kind cluster layout, including the kubeadm and containerd
  settings used by the registry mirrors and the Spegel probe.
- `argocd/` is the headless Argo CD Kustomize overlay. Astrarium renders and
  applies it before creating child Applications.

Run a suite with:

```bash
astrarium run --app mosquitto
astrarium run --all
```

The per-app `values-kind.yaml` and `overlays/kind/` convention is still used
by the catalog adapter. For an app with no kind-specific files, the production
values and source are used.

<<<<<<< HEAD
The legacy shell runner (`run-app.sh`) and its change-selection script have
been removed. Use `astrarium plan`, `astrarium run` and `astrarium upgrade`
instead; see [`astrium/README.md`](../../astrium/README.md) and the RFDs in
[`astrium/rfd/`](../../astrium/rfd/).

## Probe contract

Charts with an `e2e` values key and a `values-kind.yaml` participate in the
shared probe contract: probe Jobs carry `app.kubernetes.io/component:
e2e-probe` and `e2e.uplink.io/app: <release>`, a bounded
`activeDeadlineSeconds`, and a hardened pod security context.
`hack/ci/validate-e2e-contract.sh` renders every participating chart and
enforces those rules. Shared probe image and timeout defaults live in
`values-common.yaml`; Astrarium injects them as `global.e2e`.

## Upgrade tests

An app can opt into the generic upgrade test with an `upgrade` block in
`apps.yaml`. `astrarium upgrade --app <name>` installs the chart into
`<namespace>-rollout` with the same kind values, restarts the workload,
and verifies that the pod was replaced, a marker below `data_path` survived,
and the PVC identities and bind state did not change. Unless
`verify_pvc_retention` is false, the PVCs must also survive `helm uninstall`.
=======
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
| `stalwart` | Helm | `cert-manager`, `prometheus-operator`; PostSync probe checks admin health, SMTP/HTTPS listeners; in-cluster upgrade test enabled |
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

## Shared e2e contract

The harness is intentionally data-driven. Application-specific behaviour lives
in each chart's `templates/e2e/` probe; the runner only knows the contract.

`hack/e2e/values-common.yaml` supplies `global.e2e.probeImage` and
`global.e2e.timeoutSeconds`. `run-app.sh` injects those values into every child
Application whose chart exposes an `e2e` key. Charts use:

```gotemplate
{{- $probeImage := required "..." (default .Values.global.e2e.probeImage .Values.e2e.probeImage) }}
```

An app-specific `e2e.probeImage` in `values-kind.yaml` still wins, which is how
probes that need curl, telemetrygen, or another tool override the default.

Every probe Job carries:

```yaml
labels:
  app.kubernetes.io/component: e2e-probe
  e2e.uplink.io/app: <release name>
annotations:
  helm.sh/hook: post-install,post-upgrade
  helm.sh/hook-delete-policy: before-hook-creation,hook-succeeded
```

The runner uses those labels to remove stale probe Jobs and to dump probe logs
during diagnostics. `stale_probe_resources` remains only for resources that
cannot carry the standard label.

`./hack/ci/validate-e2e-contract.sh` renders every Helm app that exposes `e2e`
and enforces the labels, hook annotations, security context, and probe image.
It runs in the Helm CI job, so a new service cannot silently drift.

## Generic upgrade test

`apps.yaml` can opt any Helm app into the in-cluster upgrade test:

```yaml
  my-app:
    upgrade:
      enabled: true
      data_path: /var/lib/my-app   # optional marker path
      container: my-app            # optional; defaults to the first container
```

After the Argo CD app is Synced and Healthy, `run-app.sh` calls
`./hack/e2e/run-upgrade.sh APP`. It installs the same chart and kind value
files into `<namespace>-rollout`, restarts the workloads, and verifies:

- pod replacement and readiness
- the restricted security profile
- a marker survives below `data_path`
- PVC identities are unchanged and Bound
- PVCs remain after `helm uninstall`

No workflow branch, second cluster, or app-specific shell script is needed.
`upgrade.enabled` is the only switch.

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
>>>>>>> 21a9ff5 (feat: add hardened Stalwart mail server chart)
