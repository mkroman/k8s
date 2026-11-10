# Spegel — cluster-local P2P OCI registry mirror

[Spegel](https://spegel.dev/) is a stateless, cluster-local registry mirror.
Every node runs a DaemonSet instance that advertises the layers of every image
already pulled on that node over a libp2p DHT. When containerd pulls an image,
its `certs.d` mirror configuration routes the request to the local Spegel
instance first, which either streams the blobs from a peer's containerd content
store or transparently falls back to the upstream registry. Result: no more
Docker Hub / GHCR / Quay rate limiting, faster pod startup, and pulls that keep
working during upstream outages.

This directory wraps the upstream OCI chart (`oci://ghcr.io/spegel-org/helm-charts/spegel`,
pinned to `0.8.1`) with production values tuned for this Kubespray-provisioned
cluster.

> **A note on ports:** older Spegel releases (< 0.1.0) used host ports
> 29821/29822. Since chart v0.1.0 the registry is exposed through a NodePort
> Service (default `30020`) and the router (libp2p, TCP+UDP/QUIC), registry and
> metrics listeners default to `5001`, `5000` and `9090` inside the pod. All
> ports are parameterised in [`values.yaml`](values.yaml).

---

## 1. Kubespray ↔ Spegel compatibility audit

Audited against upstream docs
([spegel.dev](https://spegel.dev/docs/getting-started/#compatibility),
[containerd hosts.md](https://github.com/containerd/containerd/blob/main/docs/hosts.md))
for **this cluster: Kubernetes 1.36, containerd 2.3**.

Spegel v0.8.x (the pinned chart) officially requires **Kubernetes ≥ 1.35**
(the registry Service uses `trafficDistribution`) and **containerd ≥ 2.2**
([Releases][spegel-releases], [v0.8.0 announcement][spegel-v080]) — both
satisfied here. Older deployments (e.g. Kubespray v2.25 with containerd
1.7.x) cannot run chart ≥ 0.8 and must pin chart `0.7.x` instead.

Spegel's startup validation (v0.8.1) requires containerd ≥ 2.2. On containerd
2.4+ it also reads the CRI plugin's exported `RegistryConfigPath` and
`DiscardUnpackedLayers` and refuses to start when `discard_unpacked_layers` is
enabled. containerd 2.3 does not export those values, so there they are
operator checks rather than startup guards:

1. `config_path` must point at the directory Spegel writes mirror
   configuration to (`certs.d`), otherwise containerd never reads it.
2. `discard_unpacked_layers` must be false, otherwise containerd's GC removes
   layer blobs from the content store and Spegel cannot serve them.

[spegel-releases]: https://spegel.dev/docs/releases/
[spegel-v080]: https://spegel.dev/blog/spegel-v0.8.0/

### 1.1 Layer retention — keep `discard_unpacked_layers` false

Why it breaks Spegel: Kubespray's `containerd_discard_unpacked_layers` default
is `true` to save disk, but its template only renders the setting for
containerd < 2.1. With it enabled, containerd's garbage collector deletes layer
blobs from the content store
(`/var/lib/containerd/io.containerd.content.v1.content`) as soon as they are
unpacked into the overlayfs snapshotter. Spegel serves peers by streaming
blobs **directly out of that content store** — once discarded there is nothing
to serve, so every lookup silently falls back to the upstream registry.

containerd 2.3.4 still parses `discard_unpacked_layers`; the field was not
removed in 2.1. It is legacy/unsupported with the transfer service (containerd
2.1+ falls back to local pull mode when it is true), and only containerd 2.4+
exposes it to Spegel's startup check. Spegel 0.8.1 therefore **starts** on
containerd 2.3 even when the option is true, but it cannot serve layer blobs.
Treat `true` as a misconfiguration on every version.

A config-file grep alone does not prove the effective value: containerd may
import other files, a pod may not see the host's config file at all, and a
running daemon keeps its parsed configuration until restarted. Verify the
effective config (`containerd config dump`) on every node, as shown in §4.1.

| Kubespray version | Default containerd | `discard_unpacked_layers` rendered? | Spegel impact |
|---|---|---|---|
| v2.25.x | 1.7.16 | **Yes, `= true`** | ❌ breaks Spegel — override required |
| v2.26.x / master | ≥ 2.1 (2.3.x) | No — template renders it only for containerd < 2.1; containerd default `false` | ✅ no action needed |
| kind (e2e only) | bundled containerd | **Yes, `= true` in kind's base config** | `hack/e2e/kind.yaml` patches it to `false` for the P2P probe |

Fix: ensure `discard_unpacked_layers` is false (or omitted) in the effective
configuration on every node. On Kubespray < 2.26 set
`containerd_discard_unpacked_layers: false`; Kubespray master omits it for
containerd ≥ 2.1, whose default is `false`. This cluster runs containerd 2.3:
verify `config_path` and the effective value rather than assuming a config-file
line is enough.

> **Disk note:** disabling `discard_unpacked_layers` retains layer tarballs on
> disk. That is what Spegel serves from, but ensure kubelet image GC stays
> enabled — Spegel has no GC of its own and relies on kubelet eviction
> thresholds to reclaim space ([upstream FAQ][faq-disk]).

[faq-disk]: https://spegel.dev/docs/faq/#why-is-my-node-running-out-of-disk-space

### 1.2 Registry config path (`certs.d`) ✅ matches out of the box

Kubespray renders:

```toml
[plugins."io.containerd.cri.v1.images".registry]
  config_path = "{{ containerd_cfg_dir }}/certs.d"    # → /etc/containerd/certs.d
```

This is exactly Spegel's default `spegel.containerdRegistryConfigPath`, and
the DaemonSet host-mounts that directory into its config init container.
**No value changes needed** — the defaults are spelled out explicitly in
[`values.yaml`](values.yaml) so a future Kubespray change fails loudly.

**Interaction with Kubespray-managed mirrors:** Kubespray renders
`containerd_registries_mirrors` into
`/etc/containerd/certs.d/<prefix>/hosts.toml`. Keep that list empty when
Spegel owns `certs.d`, or leave `spegel.prependExisting: true` so existing
entries are preserved as fallbacks. If an external renderer rewrites
`hosts.toml` without the Spegel entry, that node bypasses Spegel until its
Spegel pod restarts.

⚠️ Do **not** leave `mirroredRegistries` empty while per-registry
`hosts.toml` files exist: an empty list makes Spegel write only a
`_default/hosts.toml` catch-all, and containerd gives per-registry
directories precedence over `_default` — so docker.io pulls would bypass
Spegel entirely. The values file therefore lists the registries explicitly.

### 1.3 Containerd socket ✅ matches out of the box

Kubespray: `containerd_state_dir: /run/containerd` → socket
`/run/containerd/containerd.sock` = Spegel's default `spegel.containerdSock`.
The DaemonSet host-mounts it with `type: Socket`.

### 1.4 Content store path ✅ matches out of the box

Kubespray: `containerd_storage_dir: /var/lib/containerd` →
`/var/lib/containerd/io.containerd.content.v1.content` = Spegel's default
`spegel.containerdContentPath`, mounted read-only into the DaemonSet pod.

### 1.5 Other cluster specifics ✅

- `dns_domain` default `cluster.local` matches `clusterDomain`.
- Calico provides the NetworkPolicy API used by the optional
  `networkPolicy.enabled: true` hardening.
- Bootstrap is DNS-based (`--bootstrap-kind=dns`) via the headless
  `spegel-bootstrap` Service with `publishNotReadyAddresses: true` — no host
  firewall changes required; DHT traffic flows over pod IPs through the CNI.

**Verdict:** Kubespray is green — after verifying `config_path`/certs.d and
the two value tweaks above, Spegel works unmodified on containerd 2.3.

---

## 2. Deployment

### Helm CLI (one-off / testing)

```bash
helm dependency build spegel
helm upgrade --install spegel ./spegel \
  --create-namespace --namespace spegel \
  -f spegel/values.yaml
```

### GitOps (this repo)

The chart is wired into the app-of-apps (`apps/values.yaml`): project
`spegel`, namespace `spegel`, auto-syncing Application `spegel` →
`path: spegel`. Argo CD resolves the OCI dependency from `Chart.lock`
(`charts/*.tgz` is gitignored). Commit and let Argo CD sync.

---

## 3. Values

Keys not listed here fall through to the upstream chart defaults — see its
helm-docs generated [values reference][upstream-values] for the full surface.
All keys are validated by `values.schema.json`. `validate-helm.sh` lints and
renders the chart with the production `values.yaml`; `values-kind.yaml` is
applied by the kind e2e run (`hack/e2e/run-app.sh`) and is covered there, not
by `validate-helm.sh`.

[upstream-values]: https://github.com/spegel-org/spegel/blob/main/charts/spegel/README.md

| Key | Default here | Purpose |
|---|---|---|
| `spegel.image.tag` / `digest` | unset | Inherit the chart's digest-pinned v0.8.1 image; upgrade via `Chart.yaml`. A set digest must be a full `sha256:` digest; an empty digest override is rejected |
| `spegel.tolerations` | control-plane, master, `CriticalAddonsOnly`, catch-alls | DaemonSet coverage of every node |
| `spegel.priorityClassName` | `system-node-critical` | Never evicted in favour of workloads |
| `spegel.updateStrategy` | `RollingUpdate`, `maxUnavailable: 1` | One node at a time keeps a mirror candidate per image |
| `spegel.resources` | 100m / 256Mi → 512Mi | Bounded limit: usage is mostly VFS page cache; `GOMEMLIMIT` derives from it |
| `spegel.service.registry.nodePort` | `30020` | NodePort containerd mirror configs target; must be inside `kube_apiserver_node_port_range` |
| `spegel.serviceMonitor.*` | enabled, 60s/30s, `metrics.uplink.io/monitor: prometheus` | Cluster Prometheus discovery |
| `spegel.livenessProbe.enabled` | `true` | `/livez`; startupProbe already allows 3m for DHT bootstrap |
| `spegel.networkPolicy.enabled` | `true` | DHT gossip limited to Spegel peers |
| `spegel.basicAuthSecretName` | empty | Set on multi-tenant clusters; the NodePort is otherwise unauthenticated |
| `spegel.spegel.mirroredRegistries` | docker.io, ghcr.io, quay.io, registry.k8s.io, gcr.io, mirror.gcr.io | Must be non-empty on Kubespray (see §1.2); entries require an `http(s)://` scheme |
| `spegel.spegel.prependExisting` | `true` | Keep Kubespray-rendered certs.d entries as fallbacks |
| `spegel.spegel.resolveTags` + `registryFilters` | `true` + `:latest$` | Digest-pinning with mutable tags excluded |
| `spegel.spegel.containerd*` | Kubespray defaults (see file header) | Socket, namespace, certs.d path, content store |
| `spegel.spegel.persistence` | `/var/lib/spegel` | Libp2p host key: stable DHT identities across restarts |
| `spegel.spegel.debugWebEnabled` | `true` | `/debug/web` stats page used by the runbook |
| `e2e.*` | disabled | Kind-only PostSync P2P pull test (`values-kind.yaml`) |

---

## 4. Verification runbook

### 4.1 Node-level containerd settings

```bash
# On any node (via the bastion/SSH). A single config file is not conclusive:
# containerd may import other files and keeps the parsed config until restarted.
grep -E 'config_path|discard_unpacked_layers' /etc/containerd/config.toml
# containerd 2.3 may still print discard_unpacked_layers; if present it must be false.
# containerd >= 2.1 may also omit the line entirely (default false).
# Expect: config_path = "/etc/containerd/certs.d"

# Effective runtime config as seen through CRI:
crictl info | jq '.config' | grep -iE 'discard|registryConfigPath'

# Definitive check of the daemon's parsed config (imports applied, after restart):
containerd config dump | grep -E 'discard_unpacked_layers|config_path'

# Content store must actually hold blobs (grows as images are pulled):
du -sh /var/lib/containerd/io.containerd.content.v1.content
ctr -n k8s.io -a /run/containerd/containerd.sock content ls | head
```

Repeat the same checks on every node.

### 4.2 DaemonSet health & peer discovery

```bash
kubectl -n spegel get daemonset,pods -o wide
# Expect: DESIRED == node count, all pods Running/Ready, one per node incl.
# control-plane members.

# A clean rollout proves the DaemonSet starts; on containerd 2.4+ Spegel also
# refuses to start when the CRI reports discard_unpacked_layers enabled. On
# 2.3 that is not a startup check, so verify the effective config in §4.1.
kubectl -n spegel logs -l app.kubernetes.io/name=spegel --tail=50 | grep -iE 'error|fail'
```

Peer discovery over the DHT:

```bash
# The headless bootstrap service lists every Spegel pod IP — the DHT seed list:
kubectl -n spegel get endpoints spegel-bootstrap -o wide

# Each instance advertises every layer it holds; a non-zero gauge proves the
# node is participating in the DHT:
POD=$(kubectl -n spegel get pod -l app.kubernetes.io/name=spegel -o jsonpath='{.items[0].metadata.name}')
kubectl -n spegel port-forward "$POD" 9090:9090 &
curl -s localhost:9090/metrics | grep -E 'spegel_advertised_keys|spegel_resolve_duration'
```

### 4.3 Prove cross-node P2P layer sharing

Pick an image tag **never pulled before** (e.g. a fresh `ubuntu:25.04`), then
run one pod per node — the second pull must be served by the first node's
Spegel ([upstream verification guide][verify]):

[verify]: https://spegel.dev/docs/getting-started/#verify-deployment

```bash
UPSTREAM_POD=$(kubectl -n spegel get pod -l app.kubernetes.io/name=spegel \
  -o custom-columns=:metadata.name --no-headers | shuf -n 1)
UPSTREAM_NODE=$(kubectl -n spegel get pod "$UPSTREAM_POD" \
  -o jsonpath='{.spec.nodeName}')
kubectl -n default run p2p-upstream --image=ubuntu:25.04 --restart=Never \
  --overrides="{\"spec\":{\"nodeName\":\"${UPSTREAM_NODE}\",\"containers\":[{\"name\":\"ubuntu\",\"image\":\"ubuntu:25.04\",\"imagePullPolicy\":\"Always\",\"command\":[\"true\"]}]}}"

MIRROR_POD=$(kubectl -n spegel get pod -l app.kubernetes.io/name=spegel \
  -o custom-columns=:metadata.name --no-headers | grep -v "^${UPSTREAM_POD}$" | shuf -n 1)
MIRROR_NODE=$(kubectl -n spegel get pod "$MIRROR_POD" \
  -o jsonpath='{.spec.nodeName}')
kubectl -n default run p2p-mirror --image=ubuntu:25.04 --restart=Never \
  --overrides="{\"spec\":{\"nodeName\":\"${MIRROR_NODE}\",\"containers\":[{\"name\":\"ubuntu\",\"image\":\"ubuntu:25.04\",\"imagePullPolicy\":\"Always\",\"command\":[\"true\"]}]}}"
```

Evidence on the serving (mirror) instance:

```bash
# Before/after counters on the SECOND node's Spegel pod:
kubectl -n spegel port-forward "$MIRROR_POD" 9090:9090 &
curl -s localhost:9090/metrics | grep -E \
  'spegel_mirror_requests_total|spegel_mirror_last_success_timestamp_seconds'
#   spegel_mirror_requests_total{...cache="hit"}  → increases on the p2p-mirror pull
#   spegel_mirror_last_success_timestamp_seconds  → jumps to now

# Or open the debug page while the pull runs:
#   kubectl -n spegel port-forward "$MIRROR_POD" 9090:9090
#   → http://localhost:9090/debug/web  ("Last Mirror Success" must show a
#     duration — "Pending" means the pull bypassed Spegel)

kubectl -n spegel logs "$MIRROR_POD" --tail=50

# Node-side corroboration: the mirror entry written by the init container.
# The `configuration` init container has completed, so `kubectl exec -c
# configuration` cannot work; read the file from the node instead.
ssh <MIRROR_NODE> sudo cat /etc/containerd/certs.d/docker.io/hosts.toml
# Or use a debug pod on the node (host root is mounted at /host):
#   kubectl debug node/<MIRROR_NODE> --profile=sysadmin -- \
#     cat /host/etc/containerd/certs.d/docker.io/hosts.toml
#   [host.'http://<nodeIP>:30020']  ← Spegel first, existing/upper entries after

# Optional airtight proof: sniff for upstream egress on node B during a
# mirrored pull — silence means everything came from a peer:
ssh <MIRROR_NODE> sudo tcpdump -ni any -c 20 'host registry-1.docker.io'

kubectl -n default delete pod p2p-upstream p2p-mirror
```

Continuous coverage via Prometheus:

```
rate(spegel_mirror_requests_total{cache="hit"}[5m])   # P2P hits across the fleet
spegel_advertised_keys                                # keys advertised per node
```

---

## 5. Security posture

| Control | Setting | Rationale |
|---|---|---|
| Pod Security Admission | `spegel` namespace labelled `privileged` (enforce/audit/warn, in `apps/values.yaml`) | Spegel needs hostPath access to the containerd socket and content store; an explicit label beats relying on the unlabeled namespace default |
| DHT isolation | `networkPolicy.enabled: true` | Limits libp2p gossip to Spegel peers, mitigating rogue peers advertising incorrect digests (v0.8 opt-in) |
| Image pinning | upstream chart pins `ghcr.io/spegel-org/spegel@sha256:bdc1d14e…` | Helm best practice: no floating tags; inherited by leaving `image.tag`/`image.digest` unset |
| Chart pinning | exact dependency version in `Chart.yaml`/`Chart.lock` | The lock's `digest` is a hash of the dependency set (`sha256:7dbb02c2…`), not the OCI manifest digest of the chart artifact (`sha256:0f6685e1…` for 0.8.1). Argo CD does not compare the artifact against that lock digest, and the version tag is mutable; the exact version pin is reproducible intent, not content integrity. |
| Chart signature | cosign signatures published per release | Use cosign to verify the artifact itself before adopting a new pin: `cosign verify ghcr.io/spegel-org/helm-charts/spegel:<version> --certificate-identity-regexp 'https://github\.com/spegel-org/spegel/\.github/workflows/.*' --certificate-oidc-issuer https://token.actions.githubusercontent.com` |
| Registry exposure | `basicAuthSecretName: ""` | The NodePort is reachable by anything that can reach a node IP; set a secret on multi-tenant clusters (docs: Multi-tenant Credentials) |
| Mutable tags | `registryFilters: [":latest$"]` | Prevents `latest` from being pinned to the first digest seen; such tags stay client-resolved, layers still served P2P |
| Container runtime | `readOnlyRootFilesystem: true` (upstream default); container runs as root | Root is required for the containerd socket and content store; capabilities are intentionally left untouched |
| Node constraints | `system-node-critical`, `maxUnavailable: 1`, kubelet image GC on (Kubespray default) | Spegel has no GC of its own; kubelet eviction thresholds protect node disks |

---

## Files

| Path | Purpose |
|---|---|
| [`Chart.yaml`](Chart.yaml) | Wrapper chart pinning `oci://ghcr.io/spegel-org/helm-charts/spegel` 0.8.1 |
| [`values.yaml`](values.yaml) | Production values (scheduling, ports, ServiceMonitor, containerd paths) |
| [`values.schema.json`](values.schema.json) | Draft-07 schema for chart values (production render in CI; kind overrides exercised by e2e) |
