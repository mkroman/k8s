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

The legacy shell runner (`run-app.sh`) and its change-selection script have
been removed. Use `astrarium plan` and `astrarium run` instead; see
[`astrium/README.md`](../../astrium/README.md) and the RFDs in
[`astrium/rfd/`](../../astrium/rfd/).
