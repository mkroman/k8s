# Agent notes

GitOps app-of-apps for a home-lab Kubernetes cluster managed by Argo CD.
Deeper docs: `hack/ci/README.md` (CI scripts, tool manifest),
`astrium/README.md` (Astrarium e2e runner), `hack/e2e/README.md` (shared e2e
inputs), `spegel/README.md` (containerd).

## How it is wired

- `argo-cd/overlays/production` (Kustomize, vendored `install-v*.yaml`)
  installs Argo CD plus the root `apps` Application.
- `apps/` is a Helm chart, and `apps/values.yaml` is the catalogue:
  - `projects`: AppProjects
  - `namespaces`: every Namespace, with Pod Security labels. Applications do
    not use `CreateNamespace`, so a new namespace must be added here.
  - `applications`: local apps; external repositories live in `apps-of-apps/`
- A chart directory with no `applications` entry is not deployed (for
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

## Astrarium test orchestrator

- The generic e2e/test orchestrator is Astrarium. It lives in `astrium/`;
  the binary is `astrarium`, the suite manifest is `astrarium.toml` at the
  repository root, and RFDs live in `astrium/rfd/`.
- Rust 2024, Rust 1.100, workspace with `astrarium-core` and
  `astrarium-test-support`. Run `cargo fmt --check`,
  `cargo clippy --locked --all-targets --all-features -- -D warnings`, and
  `cargo test --locked --all-features` before committing.
- The Rust binary may invoke only `helm`, `kustomize`, and `kind`. Do git
  operations with `gix`, Kubernetes with `kube`, HTTP with `reqwest`, and dates
  and times with `jiff`. Cold-image removal and node config checks use the
  Docker API through `bollard`, never the `docker` CLI. CI workflow scripts may
  use `gh`, `docker compose`, `tar`, `sha256sum`, etc.
- Native `astrarium.toml` mode remains supported. This repository opts into
  the typed, read-only `[catalog]` adapter for its existing app-of-apps
  catalogues (`apps/values.yaml` and `hack/e2e/apps.yaml`). The adapter imports
  projects, namespaces, applications and Helm repositories before normal
  validation; it does not use the production `apps` chart for deployment.
  A manifest must use either native tables or `[catalog]`, never both. Create
  Namespaces, AppProjects, and Applications directly from Rust.
- Keep the production `apps` chart as the app-of-apps source of truth for
  deployment. Do not use it as an e2e bootstrap.
- Jaeger is defined in `astrium/compose.yaml`. Registry mirrors are the
  path-prefixed proxy entries in `astrarium.toml`; kind nodes authenticate via
  `ASTRARIUM_REGISTRY_AUTH_FILE`. Only endpoints that contain `{host}` need the
  host-address resolution.
- `astrarium run --trace-file <path>` writes OTLP JSON that Jaeger UI can load
  through Search -> Upload. The e2e workflow uploads it as an
  `astrarium-<app>-trace` artifact; keep that path in failure diagnostics too.
- Use the Astrarium RFD process for architectural changes: `astrium/rfd/`,
  states prediscussion/ideation/discussion/published/committed/abandoned,
  branch + PR discussion, `rfd/README.md` index, no direct pushes to main.
- Rust style: domain modules; use `foo.rs` + `foo/` for module roots and
  submodules, and never add `foo/mod.rs`; enforce with
  `clippy::mod_module_files` (the lint that bans `mod.rs`; do not confuse it
  with `self_named_module_files`, which does the opposite); no
  `utils`/`helpers`/`common`; newtypes for semantic strings; `thiserror` per
  module and `miette` at the CLI boundary; no `unwrap`/`expect` in production;
  `gix` in `spawn_blocking`; `tokio::JoinSet`/`CancellationToken` for
  concurrency.
- CI/release: project CI builds/tests/checks the workspace. Release tags are
  `astrarium-vX.Y.Z`; release binaries are uploaded to GitHub Releases and
  cached via a local composite action. No new third-party Actions; pin runner
  images and action SHAs.
- Do not add new shell e2e scripts; extend Astrarium instead. `hack/e2e`
  contains only shared inputs (catalogue, kind config and Argo CD overlay).
- Agent workflow: define shared interfaces first, then parallelize by module
  in separate worktrees. The manager reviews each change for API consistency,
  idiom, security, and tests before merging.

## Checks

Install the pinned CI tools with
`./hack/ci/install-tools.sh ~/.local/bin [tool ...]`. Helm, kubectl and
yamllint must be installed separately.

Astrarium workspace checks (from `astrium/`):

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo build --locked --release
./target/release/astrarium doctor
```

Repository checks:

```bash
./hack/ci/lint-yaml.sh                      # plain YAML; skips templates/, charts/, workflows
./hack/ci/validate-helm.sh spegel           # one chart: dep build, lint, render, kubeconform
./hack/ci/validate-helm.sh                  # every chart (CI)
./hack/ci/validate-chart-cases.sh           # data-driven render/negative chart cases
./hack/ci/validate-e2e-contract.sh          # shared Argo CD e2e probe contract
./hack/ci/validate-ci-images.sh stalwart    # registry image preflight (needs registry login)
./hack/ci/prepare-ci-images.sh              # pre-pull host images through the registry
./hack/ci/prepare-kind-registry-auth.sh     # write node auth config from REGISTRY_USER/PASSWORD
./hack/ci/validate-kustomize.sh             # every kustomization.yaml
./hack/ci/validate-app-of-apps.sh           # root app keeps SSA + server-side diff
./hack/ci/validate-dashboards.py            # grafana/dashboards/*.json; datasources must use $datasource
python3 hack/ci/test-vector-config.py       # vector/quickwit; needs helm repos from hack/e2e/apps.yaml, vector, PyYAML
shellcheck hack/ci/*.sh
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

Astrarium is the only e2e runner. `astrarium.toml` uses the typed `[catalog]`
adapter; `hack/e2e/apps.yaml` supplies the e2e metadata, `hack/e2e/kind.yaml`
the cluster layout, and `hack/e2e/argocd` the headless Argo CD overlay.

```bash
astrarium plan --all
astrarium run --app mosquitto
astrarium run --all --keep-cluster
astrarium run --app spegel --reuse-cluster --fast-fail
```

- Iterate locally with `--keep-cluster` on the first run, then
  `--reuse-cluster`. The cluster name comes from `[cluster].name`; Astrarium
  only deletes clusters named `astrarium` or prefixed with `astrarium-`.
- Container pulls use `registry.rwx.im` through path-prefixed containerd
  mirrors. `astrarium.toml` sets `override_path = true` and
  `ASTRARIUM_REGISTRY_AUTH_FILE` points at a plain Docker `config.json` that
  Astrarium mounts at `/var/lib/kubelet/config.json` on every node. Its `auths`
  entries must be keyed by the upstream image hosts (docker.io, ghcr.io, ...),
  because kubelet chooses credentials before containerd applies the mirror.
  `hack/ci/prepare-kind-registry-auth.sh` derives those entries from
  `astrarium.toml`. CI creates the file from the `REGISTRY_USER` and
  `REGISTRY_PASSWORD` secrets. Mirrors set `fallback = false`, so a failed
  mirror never pulls directly from the upstream or leaks the credentials to it.
  Host-side images are pre-pulled and retagged by
  `hack/ci/prepare-ci-images.sh`; do not prefix image names in charts or
  manifests.
- `--trace-file diagnostics/astrarium-trace.json` writes OTLP JSON that Jaeger
  UI can open through Search → Upload.
- Kind overrides are picked up automatically: `<chart>/values-kind.yaml` for
  Helm, `<app>/overlays/kind/` for Kustomize. CI only runs apps whose path,
  Application values, catalog entry or shared Astrarium input changed.
  Preview the selection with `astrarium plan --base origin/main`.
- Kind cannot decrypt production SealedSecrets. Disable them or replace them
  in the kind values/overlay.
- Helm cannot clear base values: `{}` merges and `null` is dropped. Design
  values so that empty means "feature off" (for example
  `zigbee2mqtt` `coordinator.devicePath`).
- Astrarium relies on Argo CD sync plus diagnostics. Static chart validation
  still happens in `hack/ci/validate-helm.sh`.
- E2E probes carry `app.kubernetes.io/component: e2e-probe` and
  `e2e.uplink.io/app: <release>` labels. `hack/ci/validate-e2e-contract.sh`
  enforces the shared probe contract. Shared probe image/timeout defaults live
  in `hack/e2e/values-common.yaml` and are injected as `global.e2e`.
- Apps can opt into the generic in-cluster upgrade test with an `upgrade`
  block in `hack/e2e/apps.yaml`. `astrarium upgrade` installs into
  `<namespace>-rollout-<run>` with Helm, then checks pod replacement, marker
  persistence and PVC retention. The e2e workflow runs it with
  `--reuse-cluster --no-deploy` after `run --keep-cluster`. Do not add a
  per-app shell script.

## Probe conventions

- Probes are Helm hooks with `post-install,post-upgrade` (Argo CD maps these
  to PostSync), delete policy `before-hook-creation,hook-succeeded`, and a
  bounded `activeDeadlineSeconds`. Gate them on `.Values.e2e.enabled`.
  `helm.sh/hook: test` never runs under Argo CD. Reference implementations:
  `grafana/templates/e2e/`, `spegel/templates/e2e/checks.yaml`.
- Probe RBAC must be normal resources, not hooks. Argo CD's hook cleanup for
  passive resources does not follow Helm's per-hook lifecycle.
- Never set `spec.selector` on probe Jobs; the server-side validation in CI
  catches hand-written selectors.
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
- To register a new app: add a project, namespace and `applications`
  entry in `apps/values.yaml`, and add a row to the `README.md` table.

## Cluster / containerd

- Image pulls go through `registry.rwx.im`. The mirrors in `astrarium.toml`
  store repositories as `registry.rwx.im/<upstream>/<repo>` and require
  `override_path = true`; containerd otherwise appends `/v2` to the mirror
  path. Node authentication is the plain Docker config mounted at
  `/var/lib/kubelet/config.json`, not an imagePullSecret.
- This repo has no Ansible or Kubespray playbooks. Document the node
  settings an app needs in that chart's README.
- The production cluster runs containerd 2.3. `discard_unpacked_layers` must
  stay false or omitted, or Spegel cannot serve layers. Kind's base config
  sets it to true, so `hack/e2e/kind.yaml` patches it. Verify the effective
  value with `containerd config dump`, not by reading a config file. Details
  are in `spegel/README.md`.
