# Shared e2e inputs

Astrarium is the e2e runner. The remaining files in this directory are inputs
to it, not a separate harness:

- `apps.yaml` is the e2e metadata catalogue read by the typed
  `[catalog]` adapter. It selects the applications exercised by the suite and
  adds `requires`, probe/resource checks, Prometheus targets, timeouts,
  cold-image settings, upgrade tests, and Helm repositories.
- `kind.yaml` is the kind cluster layout, including the kubeadm and containerd
  settings used by the registry mirrors and the Spegel probe.
- `values-common.yaml` holds shared Helm values, including the `global.e2e`
  probe image and timeout. The catalog adapter merges it into every app.
- `argocd/` is the headless Argo CD Kustomize overlay. Astrarium renders and
  applies it before creating child Applications.

## Registry

Astrarium writes containerd `hosts.toml` files for `docker.io`, `ghcr.io`,
`quay.io`, `registry.k8s.io`, `gcr.io`, `mirror.gcr.io`, and `public.ecr.aws`.
Each mirror endpoint is a path-prefixed registry root such as
`https://registry.rwx.im/v2/docker.io`, with `override_path = true`, so chart
and manifest image names stay unchanged.

Registry credentials come from a plain Docker `config.json` named by
`ASTRARIUM_REGISTRY_AUTH_FILE`; Astrarium mounts it at
`/var/lib/kubelet/config.json` on every node. Create it with
`hack/ci/prepare-kind-registry-auth.sh`, which keys `auths` by every upstream
image host because kubelet chooses credentials before containerd applies the
mirror. Mirrors use `fallback = false` so a failed mirror never falls back to
the upstream. Host-side images are pulled through the same registry and
retagged before kind starts. In CI the `REGISTRY_USER` and
`REGISTRY_PASSWORD` repository secrets provide both.

Run a suite with:

```bash
astrarium plan --all
astrarium run --app mosquitto
astrarium run --all
astrarium upgrade --app stalwart
```

The per-app `values-kind.yaml` and `overlays/kind/` convention is still used
by the catalog adapter. For an app with no kind-specific files, the production
values and source are used.

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
enforces those rules.

## Upgrade tests

An app can opt into the generic upgrade test with an `upgrade` block in
`apps.yaml`. `astrarium upgrade --app <name>` first deploys the app and its
prerequisites, then installs the chart into `<namespace>-rollout-<run>` with the same
kind values, upgrades it, restarts the workload, and verifies that the pod was
replaced, a marker below `data_path` survived, and the PVC identities and bind
state did not change. Unless `verify_pvc_retention` is false, the PVCs must
also survive `helm uninstall`.

`defaults.upgrade` in `apps.yaml` supplies the timeout, security profile and
PVC-retention defaults. An app-level `upgrade` block overrides them. In CI,
`astrarium run --app <name> --keep-cluster` deploys the app and
`astrarium upgrade --app <name> --reuse-cluster --no-deploy` runs only the
in-cluster part on that cluster. Apps without an upgrade test are no-ops.
