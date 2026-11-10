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
  `CLUSTER_NAME=...`) and skips cluster creation and cleanup deletion.
- Reuse mode disables root auto-sync and deletes the previously selected child
  Applications before reapplying them, so stale sync operations and resources
  do not leak between runs. Namespaces stay in place.
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
settings are for local iterations only.

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

The current cluster runs containerd 2.3. `discard_unpacked_layers` was removed
there and defaults to false; verify `config_path`/certs.d instead of setting
the legacy variable.

## Chart conventions

- Wrapper charts pin their upstream dependency in `Chart.yaml`/`Chart.lock`.
- Do not set empty `image.tag`/`image.digest` overrides: an explicit empty
  parent value overrides the upstream default and disables digest pinning.
- Register new applications in `apps/values.yaml` (project, namespace,
  Application, and any project resource whitelist).
- Run `./hack/ci/validate-helm.sh <chart>` after chart changes.
