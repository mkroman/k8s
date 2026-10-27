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

## Currently covered

| Application | Source | Prerequisites |
| --- | --- | --- |
| `agentgateway` | Helm, OCI dependencies | `gateway-api`, `prometheus-operator` |
| `cert-manager` | Helm | none |
| `cluster-exporters` | Helm | `prometheus-operator` |
| `cloudnative-pg` | Kustomize | `cert-manager`, `prometheus-operator` |
| `gateway-api` | Kustomize | none |
| `grafana` | Helm | `prometheus-operator`; checks the renderer scrape path |
| `monitoring` | Kustomize | `prometheus-operator`; asserts the control-plane scrape targets are up |
| `prometheus-operator` | Kustomize | `cert-manager` |
| `sealed-secrets` | Kustomize | none |
| `traefik` | Helm | `prometheus-operator` |
| `vector-agent` | Helm | `prometheus-operator` |
| `vector-aggregator` | Helm | `prometheus-operator` |

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

Helm's own `helm.sh/hook: test` tests are only executed by `helm test`, and
Argo CD does not map the Helm test hooks. Mapping `post-install`/`post-upgrade`
to Argo CD's `PostSync` is the supported equivalent. The probes therefore use
`helm.sh/hook: post-install,post-upgrade`, are idempotent, have a bounded
`activeDeadlineSeconds`, and use `before-hook-creation,hook-succeeded` cleanup.
They test behaviour, not just Argo CD's resource-health status.

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
an ephemeral kind cluster does not have the production sealed-secrets key.

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
