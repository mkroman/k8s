# Monitoring

This directory deploys the cluster monitoring stack with Argo CD and Kustomize:

- Prometheus (Prometheus Operator CR) with a Thanos sidecar in production
- Alertmanager
- Thanos query, store and compact workloads
- Traefik ingress and basic-auth for Prometheus/Thanos
- Kubernetes control-plane scrape targets

Grafana (`grafana/`) and the node/cluster exporters
(`monitoring/cluster-exporters/`) are separate Argo CD Applications.

## Layout

```text
monitoring/
├── kustomization.yaml                 # production composition root
├── overlays/kind/                     # e2e app overlay: Prometheus without production features
├── prometheus/
│   ├── kustomization.yaml             # defaults to overlays/production
│   ├── base/                          # server, RBAC and self-monitor
│   ├── scrape/                        # cluster/control-plane targets
│   └── overlays/
│       ├── production/                # Alertmanager, Thanos and PVC settings
│       └── kind/                      # same base+scrape, no production features
├── thanos/
├── alertmanager/
└── ingress/
```

The base Prometheus CR is intentionally minimal: one replica, no Alertmanager,
Thanos or PVC. `prometheus/overlays/production` adds those features. This keeps
the kind overlay from having to delete production fields, which made the e2e
manifest brittle.

`monitoring/kustomization.yaml` sets `namespace: monitoring`. The
`ClusterRoleBinding` service-account subject is explicitly in `monitoring`
because Kustomize does not rewrite binding subjects.

## Scrape targets

| Target | Resource | Job label | Endpoint | Auth |
| --- | --- | --- | --- | --- |
| kubelet | `scrape/servicemonitor.kubelet.yaml` | `kubelet` | `https://<node>:10250/metrics`, `/metrics/cadvisor`, `/metrics/probes` | projected SA token |
| CoreDNS | `scrape/servicemonitor.coredns.yaml` | `kube-dns` | `http://<pod>:9153/metrics` | none |
| kube-apiserver | `scrape/servicemonitor.kube-apiserver.yaml` | `apiserver` | `https://kubernetes.default.svc:443/metrics` | projected SA token + cluster CA |
| kube-scheduler | `scrape/scrapeconfig.kube-scheduler.yaml` | `kube-scheduler` | `https://<control-plane-node>:10259/metrics` | token Secret |
| kube-controller-manager | `scrape/scrapeconfig.kube-controller-manager.yaml` | `kube-controller-manager` | `https://<control-plane-node>:10257/metrics` | token Secret |

The scheduler and controller-manager are static pods with no Service. They are
discovered with `kubernetes_sd_configs: role: Node` and filtered on the
`node-role.kubernetes.io/control-plane` node label. The monitoring Argo CD
project only writes to the `monitoring` and `grafana` namespaces, so creating
discovery Services in `kube-system` (the upstream kube-prometheus pattern) is
not an option here.

### Requirements

- Prometheus Operator v0.94.1 or newer, started with
  `--kubelet-service=kube-system/kubelet` and `--kubelet-endpoints=true`. The
  vendored manifest in `prometheus-operator/` sets both. The operator creates
  the headless `kubelet` Service that the kubelet ServiceMonitor selects.
- Control-plane nodes labelled `node-role.kubernetes.io/control-plane` (the
  Kubespray/kubeadm default).
- kube-scheduler and kube-controller-manager bound to an address reachable from
  the Prometheus pods. Kubespray defaults to `"::"`; `hack/e2e/kind.yaml` patches
  kind's kubeadm config to `0.0.0.0`.
- Network access from Prometheus to TCP 10250, 10257, 10259, 6443 and 9153.
- The Prometheus ServiceAccount ClusterRole must retain `nodes`,
  `nodes/metrics`, `services`, `endpoints`, `pods`, `endpointslices` and the
  `nonResourceURLs: ["/metrics"]` rule. `base/manifests/clusterrole.yaml` is the
  source of truth.

## Authentication

`ServiceMonitor` endpoints use `bearerTokenFile`, which reads Prometheus's
short-lived, automatically rotated projected service-account token. The field
is deprecated in the operator API, but it avoids a long-lived credential and is
still what kube-prometheus ships. Revisit it when the operator exposes an
equivalent for `ScrapeConfig`.

`ScrapeConfig` only supports `authorization.credentials`, a Secret reference;
it has no `bearerTokenFile` or `credentialsFile` field in v0.94.1. The
`scrape/secret.scrape-token.yaml` Secret is therefore a long-lived token for the
same `prometheus` ServiceAccount. It is restricted to the ClusterRole above.
Rotate it by deleting the Secret; the token controller recreates the token and
the operator reloads the scrape configuration.

Do not set `automountServiceAccountToken: false` on the Prometheus
ServiceAccount: the kubelet and apiserver `bearerTokenFile` endpoints need the
projected token.

TLS verification is disabled for kubelet, kube-scheduler and
kube-controller-manager because their serving certificates are self-signed or
not guaranteed to include the node address. The API server target uses the
in-cluster CA and `serverName: kubernetes`.

## Adding a scrape target

1. Prefer a `ServiceMonitor` when a Service already exposes the metrics port.
   Otherwise use a `ScrapeConfig` with the smallest discovery mechanism that
   works (`staticConfigs`, `kubernetesSDConfigs`, ...).
2. Add the resource to `prometheus/scrape/` and list it in
   `prometheus/scrape/kustomization.yaml`. The label transformer adds
   `metrics.uplink.io/monitor: prometheus` and
   `app.kubernetes.io/part-of: monitoring`; do not repeat them in the resource.
3. Add the expected `job` label to `prometheus_targets.jobs` for the
   `monitoring` app in `hack/e2e/apps.yaml` and to the table above. If the job
   can have several targets, the e2e check requires all of them to be `up`.
4. If the target is not available in kind, put it in a component/overlay rather
   than the always-on `scrape/` directory. Keep the kind overlay free of
   production-only storage, Alertmanager and Thanos dependencies.
5. Render and validate:

   ```bash
   kubectl kustomize monitoring
   kubectl kustomize monitoring/overlays/kind
   ./hack/e2e/run-app.sh monitoring
   ```

## Kind e2e

`hack/e2e/run-app.sh monitoring` deploys `monitoring/overlays/kind`, which
contains only the production-free Prometheus composition. After the Application
is Synced and Healthy, the harness port-forwards to the `prometheus` Service and
queries `/api/v1/targets?state=active`. The `prometheus_targets.jobs` entry in
`hack/e2e/apps.yaml` lists the expected jobs; every target for those jobs must
report `up`. This check lives in the generic e2e harness, not in a
monitoring-specific Job.

Kind's control-plane runs kubeadm. `hack/e2e/kind.yaml` patches the
scheduler/controller-manager bind addresses; kind's kubeadm patch helper merges
the legacy map form into kubeadm v1beta4's `extraArgs` list and preserves
kind's `enable-hostpath-provisioner` default.

## Known limitations

- `ScrapeConfig` is `v1alpha1` in prometheus-operator v0.94.1. The e2e check
  covers the scheduler/controller-manager path so an upgrade is visible.
- The long-lived scrape token is a deliberate exception to the repo's
  SealedSecret convention; it is generated by the Kubernetes token controller,
  contains no commit-time secret, and is scoped to the Prometheus ServiceAccount.
- The node relabeling handles IPv4 and IPv6 `InternalIP` values separately.
  A node with more than one address still exposes only the first address of
  each type through Prometheus's Node SD.
- Clusters that still use the legacy `node-role.kubernetes.io/master` label are
  not matched by the control-plane filter.
- The kind overlay does not deploy Thanos, Alertmanager, ingress, or PVCs, so
  those components are not exercised by the monitoring e2e test.
- Metrics cardinality is not trimmed here. If the control-plane or kubelet
  targets become expensive, add `metricRelabelings` rather than dropping
  targets.
