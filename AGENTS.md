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
  instead of treating an empty scrape as success.
- Explicitly delete probe-created pods and poll until they are gone before
  reusing fixed names.
- The Spegel probe assumes a fresh kind cluster for its cold-image
  precondition. Reused clusters can retain images and DHT provider records;
  recreate the cluster when validating cold-start behavior.

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
