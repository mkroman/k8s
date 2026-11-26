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
prerequisites, then installs the chart into `<namespace>-rollout` with the same
kind values, upgrades it, restarts the workload, and verifies that the pod was
replaced, a marker below `data_path` survived, and the PVC identities and bind
state did not change. Unless `verify_pvc_retention` is false, the PVCs must
also survive `helm uninstall`.

`defaults.upgrade` in `apps.yaml` supplies the timeout, security profile and
PVC-retention defaults. An app-level `upgrade` block overrides them.
