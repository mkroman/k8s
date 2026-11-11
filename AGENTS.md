# Agent notes

This repository is a GitOps app-of-apps for a Kubernetes cluster. Charts live
under `<app>/`, the application catalogue is `apps/values.yaml`, and the
kind-based application tests live in `hack/e2e/`.

## Fast local checks

Run the checks that cover the changed area before committing:

```bash
# YAML syntax
yamllint --no-warnings --config-file hack/ci/yamllint.yaml <files>

# One Helm chart: dependency build, lint, render, kubeconform
./hack/ci/validate-helm.sh spegel

# Every chart (CI parity)
./hack/ci/validate-helm.sh

# Shell scripts
bash -n hack/e2e/run-app.sh
shellcheck hack/ci/*.sh hack/e2e/*.sh

# Whitespace
git diff --check
```

`validate-helm.sh [CHART ...]` accepts repository-relative chart directories.
With no arguments it validates every chart, which is what CI runs.

## E2E development loop

`hack/e2e/run-app.sh APP` creates a three-node kind cluster, installs Argo CD,
renders the real `apps` chart, and waits for the selected app to be Synced and
Healthy. Cluster creation and the Argo CD install dominate local iteration.

First run, leaving the cluster behind:

```bash
KEEP_CLUSTER=1 ./hack/e2e/run-app.sh spegel
```

Subsequent runs reuse the cluster and installed control plane:

```bash
REUSE_CLUSTER=1 ./hack/e2e/run-app.sh spegel
```

- `REUSE_CLUSTER=1` requires an existing kind cluster named `e2e-<app>` (or
  `CLUSTER_NAME=...`) and skips cluster creation and cleanup deletion. Both
  `REUSE_CLUSTER` and `KEEP_CLUSTER` accept 1/true/yes or 0/false/no.
- Reuse mode disables root auto-sync, deletes every child Application (stale
  apps first, then the current chain in reverse order), and fails if any
  Application cannot be deleted. Namespaces stay in place.
- The runner removes `e2e.pullImage` from `values-kind.yaml` on every kind node
  for apps with `requires_fresh_image`, on fresh and reused clusters alike.
  Before that it removes resources declared under `stale_probe_resources`
  (post-delete hook leftovers and force-deleted probe/cleanup pods) and waits
  for them to disappear, so stale containers cannot pin the image or confuse
  the probe. It verifies the `kind.yaml` containerd patch for apps with
  `requires_containerd_config`; recreate the cluster if that check fails.
- Argo CD uses an app's `retry_limit` when set (Spegel uses 0 because a retry
  cannot recreate a cold image) and otherwise the runner's `E2E_RETRY_LIMIT`.
  An app's `timeout_seconds` overrides `E2E_TIMEOUT_SECONDS`.
- Argo CD and the Applications are reapplied every run; the tested revision is
  always the current checkout.
- Tear down with `kind delete cluster --name e2e-spegel`.

Faster failure feedback (local only):

```bash
REUSE_CLUSTER=1 \
  E2E_RETRY_LIMIT=1 \
  E2E_RETRY_MAX_DURATION=30s \
  E2E_TIMEOUT_SECONDS=300 \
  E2E_REFRESH_TIMEOUT_SECONDS=60 \
  ./hack/e2e/run-app.sh spegel
```

CI keeps the defaults: fresh cluster, `E2E_RETRY_LIMIT=20`,
`E2E_RETRY_MAX_DURATION=5m`, `E2E_TIMEOUT_SECONDS=600`, and
`E2E_REFRESH_TIMEOUT_SECONDS=120`. Do not lower these in CI; the faster
settings are for local iterations only. `E2E_RETRY_MAX_DURATION` must be a Go
duration such as `30s` or `5m`, and `E2E_RETRY_LIMIT` must not have leading
zeros.

## E2E probe notes

- Chart probes are Helm PostSync hooks gated by `.Values.e2e.enabled`. CI maps
  `helm.sh/hook: post-install,post-upgrade` to Argo CD PostSync hooks.
- Keep supporting probe RBAC as normal resources rather than `hook-succeeded`
  hooks. Argo CD's hook cleanup for passive resources follows sync semantics,
  not Helm's per-hook lifecycle, so normal resources are the safe way to
  guarantee RBAC exists while the Job runs.
- The Kubernetes API may return indented JSON. Parse fields with
  whitespace-tolerant patterns such as `"phase"[[:space:]]*:` or use a JSON
  tool; do not assume compact JSON.
- Probe scripts run with `/bin/sh` (BusyBox ash) in CI. Use
  `set -euo pipefail`, and make metric checks fail when the metric is absent
  instead of treating an empty scrape as success. Match the specific metric
  labels under test (for Spegel, select the tested `registry` and count only
  `cache="hit"`), and distinguish an absent metric from a zero value.
- Read listener ports from chart values instead of hardcoding them. The Spegel
  probe takes the registry and metrics ports from `.Values.spegel.service`.
- Never set `spec.selector` on a probe Job. The API server rejects a Job whose
  selector was not auto-generated ("selector not auto-generated"), which
  static validation cannot see; the runner's server-side dry-run gate catches
  it before the sync, and the generated selector (controller-uid/job-name)
  can never match another workload's pods.
- Check the application's AppProject whitelist before adding probe or policy
  resources. The default namespace blacklist denies ResourceQuota, LimitRange
  and NetworkPolicy; probe Jobs are `batch/Job` and need an explicit entry in
  `apps/values.yaml` (`projects[].namespaceResourceWhitelist`). A whitelist
  is exclusive: it replaces the default allow-all behavior, so it must list
  every kind the application renders, including `bitnami.com/SealedSecret`
  for an application that ships one.
- Explicitly delete probe-created pods and poll until they are gone before
  reusing fixed names. Treat only HTTP 404 as gone; 401/403/429/5xx responses
  and timeouts are not evidence that a pod was deleted.
- The Spegel probe assumes the configured image is cold. The e2e runner clears
  the cold test image from the nodes before the probe, so a reused cluster
  cannot satisfy the assertion from stale containerd content. Recreate the
  cluster when validating cold-image or DHT provider behavior, because reuse
  can also retain unrelated content and provider records.

## Cluster preparation

This repository does not ship Ansible or Kubespray playbooks. Document required
containerd/Kubernetes settings in the relevant chart README and leave node
preparation to the cluster operator.

The current cluster runs containerd 2.3, which still parses
`discard_unpacked_layers`; the field is unsupported with the transfer service,
not removed. Keep it false or omitted: with `true`, containerd GCs layer blobs
and Spegel cannot serve them. Kubespray only renders
`containerd_discard_unpacked_layers` for containerd < 2.1, so master omits it
(default false); kind's base config sets it true, which is why the e2e kind
patch sets false. Verify `config_path`/certs.d and the effective value
(`containerd config dump`), not just a config file.

## Chart conventions

- Wrapper charts pin their upstream dependency in `Chart.yaml`/`Chart.lock`.
- Do not set empty `image.tag`/`image.digest` overrides: an explicit empty
  parent value overrides the upstream default and disables digest pinning.
- Register new applications in `apps/values.yaml` (project, namespace,
  Application, and any project resource whitelist).
- Run `./hack/ci/validate-helm.sh <chart>` after chart changes.
