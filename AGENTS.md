# Agent notes

GitOps app-of-apps for a home-lab Kubernetes cluster managed by Argo CD.
Deeper docs: `hack/ci/README.md` (CI scripts, tool manifest),
`hack/e2e/README.md` (kind e2e harness), `spegel/README.md` (containerd).

## How it is wired

- `argo-cd/overlays/production` (Kustomize, vendored `install-v*.yaml`)
  installs Argo CD plus the root `apps` Application.
- `apps/` is a Helm chart, and `apps/values.yaml` is the catalogue:
  - `projects`: AppProjects
  - `namespaces`: every Namespace, with Pod Security labels. Applications do
    not use `CreateNamespace`, so a new namespace must be added here.
  - `newApplications`: local apps
  - `applications`: external repos via `apps-of-apps/<name>.yaml`
- A chart directory with no `newApplications` entry is not deployed (for
  example `immich`, `kanidm`, `flaresolverr`).
- AppProject defaults in `apps/templates/appproject.yaml`: if
  `clusterResourceWhitelist` is omitted it renders `[]`, so no cluster-scoped
  kinds are allowed. If neither namespace list is set, ResourceQuota,
  LimitRange and NetworkPolicy are denied. A `namespaceResourceWhitelist` is
  exclusive and must list every kind the app renders, including
  `bitnami.com/SealedSecret` and probe `batch/Job`. To opt out, set
  `namespaceResourceBlacklist: []`.
- Large-CRD apps need `ServerSideApply=true` in their `syncPolicy`.
- The root `apps` Application uses `ServerSideApply=true` and
  `ServerSideDiff=true`. Argo CD adds pre/post-delete finalizers to child
  Applications whose charts render delete hooks (currently NFD, Spegel and
  Longhorn). SSA keeps those controller-owned finalizers out of the
  app-of-apps diff. Do not remove these options; if client-side apply is ever
  restored, declare the matching `pre-delete-finalizer.*` or
  `post-delete-finalizer.*` entries in `apps/values.yaml` instead.

## Checks

Install the pinned CI tools with
`./hack/ci/install-tools.sh ~/.local/bin [tool ...]`. Helm, kubectl and
yamllint must be installed separately.

```bash
./hack/ci/lint-yaml.sh                      # plain YAML; skips templates/, charts/, workflows
./hack/ci/validate-helm.sh spegel           # one chart: dep build, lint, render, kubeconform
./hack/ci/validate-helm.sh                  # every chart (CI)
./hack/ci/validate-kustomize.sh             # every kustomization.yaml
./hack/ci/validate-app-of-apps.sh           # root app keeps SSA + server-side diff
./hack/ci/validate-dashboards.py            # grafana/dashboards/*.json; datasources must use $datasource
python3 hack/ci/test-vector-config.py       # vector/quickwit; needs helm repos from hack/e2e/apps.yaml, vector, PyYAML
shellcheck hack/ci/*.sh hack/e2e/*.sh
git diff --check
```

- `validate-helm.sh` renders each chart only with its default `values.yaml`.
  Other values files are never validated: `values-prod.yaml`,
  `values-magistr.yaml`, `values-topic-metrics.yaml` and `values-kind.yaml`.
  When you change one, check it with
  `helm template <chart> -f <chart>/<file>`.
- CI also runs actionlint, zizmor and
  `renovate-config-validator --strict renovate.json`. Pin workflow actions to
  commit SHAs.
- Tools are pinned in `hack/ci/tools.txt`. Keep `repo`, `version` and
  `sha256` directly after the `[tool]` header, because Renovate's regex
  depends on that order.

## E2E (kind + Argo CD)

`hack/e2e/run-app.sh APP` runs one entry from `hack/e2e/apps.yaml`. It needs
docker, kind, kubectl, argocd, yq and jq.

- It deploys the committed HEAD from a local git daemon and refuses to run if
  the app's paths are dirty. Commit before testing.
- Iterate locally with `KEEP_CLUSTER=1` on the first run, then
  `REUSE_CLUSTER=1`. The cluster is named `e2e-<app>`; tear it down with
  `kind delete cluster --name e2e-<app>`.
- For faster local failures, set `E2E_RETRY_LIMIT=1 E2E_RETRY_MAX_DURATION=30s
  E2E_TIMEOUT_SECONDS=300 E2E_REFRESH_TIMEOUT_SECONDS=60`. Never lower the CI
  defaults (20, 5m, 600, 120). An app's `retry_limit` or `timeout_seconds` in
  `apps.yaml` overrides these.
- Kind overrides are picked up automatically: `<chart>/values-kind.yaml` for
  Helm, `<app>/overlays/kind/` for Kustomize. CI only runs apps whose path,
  Application values or `apps.yaml` entry changed. Preview the selection
  with `./hack/e2e/list-changed.sh origin/main HEAD`.
- Kind cannot decrypt production SealedSecrets. Disable them or replace them
  in the kind values/overlay.
- Helm cannot clear base values: `{}` merges and `null` is dropped. Design
  values so that empty means "feature off" (for example
  `zigbee2mqtt` `coordinator.devicePath`).
- Before syncing, the runner does a server-side dry-run with strict
  validation. It catches errors static checks miss, such as a hand-written
  Job `spec.selector`. Never set `spec.selector` on probe Jobs.

## Probe conventions

- Probes are Helm hooks with `post-install,post-upgrade` (Argo CD maps these
  to PostSync), delete policy `before-hook-creation,hook-succeeded`, and a
  bounded `activeDeadlineSeconds`. Gate them on `.Values.e2e.enabled`.
  `helm.sh/hook: test` never runs under Argo CD. Reference implementations:
  `grafana/templates/e2e/`, `spegel/templates/e2e/checks.yaml`.
- Probe RBAC must be normal resources, not hooks. Argo CD's hook cleanup for
  passive resources does not follow Helm's per-hook lifecycle.
- Scripts run under BusyBox `/bin/sh`:
  - Use `set -euo pipefail`.
  - Parse API JSON in a whitespace-tolerant way (the output may be indented).
  - Treat only HTTP 404 as "deleted".
  - A metric check must fail when the metric is absent, not treat it as zero.
  - Read ports from chart values instead of hardcoding them.
- A probe must be idempotent, or declare its cold-start preconditions in
  `apps.yaml` (`requires_fresh_image`, `retry_limit: 0`,
  `stale_probe_resources`), as Spegel does. Recreate the cluster rather than
  reusing it when validating cold-image or DHT behaviour.

## Chart conventions

- Wrapper charts pin their upstream dependency in `Chart.yaml`/`Chart.lock`.
  `charts/*.tgz` is gitignored.
- Do not set empty `image.tag`/`image.digest` overrides on upstream charts.
  An explicit empty value replaces the upstream digest pin.
- New local charts follow the recent ones (`magistr`, `zigbee2mqtt`, `zeta`):
  - ship `values.schema.json`
  - name template files after the resource kind
    (`poddisruptionbudget.yaml`, not `pdb.yaml`)
  - mark Renovate tracking of `appVersion` with a
    `# renovate: datasource=... depName=...` comment in `Chart.yaml`
- Images pinned in `values-kind.yaml` need a `# renovate:` comment on the
  line before them.
- Never commit plaintext Secrets. SealedSecrets come from per-app
  `make secrets` (`op2secret | kubeseal`), which needs 1Password and cluster
  access. Ask the user to regenerate them.
- To register a new app: add a project, namespace and `newApplications`
  entry in `apps/values.yaml`, and add a row to the `README.md` table.

## Cluster / containerd

- This repo has no Ansible or Kubespray playbooks. Document the node
  settings an app needs in that chart's README.
- The production cluster runs containerd 2.3. `discard_unpacked_layers` must
  stay false or omitted, or Spegel cannot serve layers. Kind's base config
  sets it to true, so `hack/e2e/kind.yaml` patches it. Verify the effective
  value with `containerd config dump`, not by reading a config file. Details
  are in `spegel/README.md`.
