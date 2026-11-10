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
for **this cluster: Kubernetes 1.36, containerd ≥ 2.2**.

Spegel v0.8.x (the pinned chart) officially requires **Kubernetes ≥ 1.35**
(the registry Service uses `trafficDistribution`) and **containerd ≥ 2.2**
([Releases][spegel-releases], [v0.8.0 announcement][spegel-v080]) — both
satisfied here. Older deployments (e.g. Kubespray v2.25 with containerd
1.7.x) cannot run chart ≥ 0.8 and must pin chart `0.7.x` instead.

Spegel's startup self-checks enforce two containerd requirements:

1. `config_path` must be set (registry host configuration via `certs.d`)
2. `discard_unpacked_layers` must **not** be enabled

[spegel-releases]: https://spegel.dev/docs/releases/
[spegel-v080]: https://spegel.dev/blog/spegel-v0.8.0/

### 1.1 Layer retention — `discard_unpacked_layers` ✅ requires override (v2.25)

Why the default breaks Spegel: Kubespray sets
`containerd_discard_unpacked_layers: true` to save disk. With it enabled,
containerd's garbage collector deletes layer blobs from the content store
(`/var/lib/containerd/io.containerd.content.v1.content`) as soon as they are
unpacked into the overlayfs snapshotter. Spegel serves peers by streaming
blobs **directly out of that content store** — once discarded there is nothing
to serve, every lookup silently falls back to the upstream registry (and
Spegel actually refuses to start when it detects the option enabled, since it
cannot write the fix itself: disabling it requires a containerd restart).

| Kubespray version | Default containerd | `discard_unpacked_layers` rendered? | Spegel impact |
|---|---|---|---|
| v2.25.x | 1.7.16 | **Yes, `= true`** | ❌ breaks Spegel — override required |
| v2.26.x / master | ≥ 2.1 (2.3.7) | No — option removed in containerd 2.1, default `false` | ✅ works, override kept as belt-and-braces |

Fix: [`kubespray/group_vars/all/containerd.yml`](kubespray/group_vars/all/containerd.yml):

```yaml
containerd_discard_unpacked_layers: false
```

### 1.2 Registry config path (`certs.d`) ✅ matches out of the box

Kubespray's `config.toml.j2` renders:

```toml
[plugins."io.containerd.cri.v1.images".registry]
  config_path = "{{ containerd_cfg_dir }}/certs.d"    # → /etc/containerd/certs.d
```

This is exactly Spegel's default `spegel.containerdRegistryConfigPath`, and
the DaemonSet host-mounts that directory into its config init container.
**No value changes needed** — the defaults are spelled out explicitly in
[`values.yaml`](values.yaml) so a future Kubespray change fails loudly.

**Interaction with `containerd_registries_mirrors`:** Kubespray renders this
variable into `/etc/containerd/certs.d/<prefix>/hosts.toml` (verified:
`roles/container-engine/containerd/tasks/main.yml`). Two coexistence modes:

| Mode | Config | Behaviour |
|---|---|---|
| **Recommended** | `containerd_registries_mirrors: []` in group_vars | Spegel solely owns `certs.d`. Stock Kubespray entry is only a docker.io passthrough (`registry-1.docker.io`) that Spegel already falls back to — nothing is lost. |
| **Keep Kubespray mirrors** | leave list, `spegel.prependExisting: true` (set in values.yaml) | Spegel reads existing `hosts.toml` and **prepends** its local mirror, preserving Kubespray entries as fallbacks: *local peer → Kubespray mirror → upstream*. Caveat: an Ansible re-run re-renders the files without the Spegel line until that node's Spegel pod restarts. |

⚠️ Do **not** leave `mirroredRegistries` empty on Kubespray *while*
`containerd_registries_mirrors` is set: an empty list makes Spegel write only
a `_default/hosts.toml` catch-all, and containerd gives per-registry
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

**Verdict:** Kubespray is green — after the `discard_unpacked_layers`
override and the two value tweaks above, Spegel works unmodified.

---

## 2. Node preparation (Ansible)

1. Copy [`kubespray/group_vars/all/containerd.yml`](kubespray/group_vars/all/containerd.yml)
   into the Kubespray checkout as `inventory/<cluster>/group_vars/all/containerd.yml`.
2. Copy [`kubespray/playbooks/prepare-containerd-spegel.yml`](kubespray/playbooks/prepare-containerd-spegel.yml)
   into `<kubespray>/playbooks/`.

```bash
cd <kubespray>

# Rolling application — one node at a time, containerd restarts only where
# the config actually changed:
ansible-playbook -i inventory/<cluster>/hosts.yaml -b \
  playbooks/prepare-containerd-spegel.yml

# Or restrict to specific nodes first (recommended for the canary):
ansible-playbook -i inventory/<cluster>/hosts.yaml -b \
  --limit "worker-1" playbooks/prepare-containerd-spegel.yml
```

> Alternative: run the stock `playbooks/cluster.yml` /
> `playbooks/upgrade-cluster.yml` after committing the group_vars — same
> result, heavier execution.
>
> **Disk note:** disabling `discard_unpacked_layers` retains layer tarballs on
> disk. That is what Spegel serves from, but ensure kubelet image GC stays
> enabled (Kubespray default) — Spegel has no GC of its own and relies on
> kubelet eviction thresholds to reclaim space ([upstream FAQ][faq-disk]).

[faq-disk]: https://spegel.dev/docs/faq/#why-is-my-node-running-out-of-disk-space

---

## 3. Deployment

### Helm CLI (one-off / testing)

```bash
helm upgrade --install spegel oci://ghcr.io/spegel-org/helm-charts/spegel \
  --version 0.8.1 \
  --create-namespace --namespace spegel \
  -f spegel/values.yaml
```

### GitOps (this repo)

The chart is wired into the app-of-apps (`apps/values.yaml`): project
`spegel`, namespace `spegel`, auto-syncing Application `spegel` →
`path: spegel`. Argo CD resolves the OCI dependency from `Chart.lock`
(`charts/*.tgz` is gitignored). Commit and let Argo CD sync.

---

## 4. Verification runbook

### 4.1 Node-level containerd settings

```bash
# On any node (via the bastion/SSH):
grep -E 'config_path|discard_unpacked_layers' /etc/containerd/config.toml
# containerd < 2.1 must print:  discard_unpacked_layers = false
# containerd >= 2.1 may omit the line entirely (default false)
# Expect: config_path = "/etc/containerd/certs.d"

# Effective runtime config as seen through CRI:
crictl info | jq '.config' | grep -iE 'discard|registryConfigPath'

# Definitive check of the daemon's parsed config:
containerd config dump | grep -E 'discard_unpacked_layers|config_path'

# Content store must actually hold blobs (grows as images are pulled):
du -sh /var/lib/containerd/io.containerd.content.v1.content
ctr -n k8s.io -a /run/containerd/containerd.sock content ls | head
```

Across the whole fleet (from the Ansible control node):

```bash
ansible k8s_cluster -i inventory/<cluster>/hosts.yaml -b -m shell -a \
  "grep -E 'discard_unpacked_layers|config_path' /etc/containerd/config.toml"
```

### 4.2 DaemonSet health & peer discovery

```bash
kubectl -n spegel get daemonset,pods -o wide
# Expect: DESIRED == node count, all pods Running/Ready, one per node incl.
# control-plane members.

# Spegel refuses to start on misconfigured nodes — a clean rollout is itself
# a compatibility check, but confirm:
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

# Node-side corroboration: the mirror entry written by the init container
kubectl -n spegel exec "$MIRROR_POD" -c configuration -- \
  cat /etc/containerd/certs.d/docker.io/hosts.toml 2>/dev/null || \
  ssh <MIRROR_NODE> cat /etc/containerd/certs.d/docker.io/hosts.toml
#   [host.'http://<nodeIP>:30020']  ← Spegel first, Kubespray/upper entries after

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
| Image pinning | upstream chart pins `ghcr.io/spegel-org/spegel@sha256:bdc1d14e…` | Helm best practice: no floating tags; inherited by leaving `image.tag`/`image.digest` empty |
| Chart pinning | exact dependency version + `Chart.lock` OCI digest | Argo CD verifies the built dependency against the lock digest; deliberate deviation from Helm's `~` range advice for supply-chain reproducibility |
| Chart signature | cosign signatures published per release | Verify before adopting a new pin: `cosign verify ghcr.io/spegel-org/helm-charts/spegel:<version> --certificate-identity-regexp 'https://github\.com/spegel-org/spegel/\.github/workflows/.*' --certificate-oidc-issuer https://token.actions.githubusercontent.com` |
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
| [`values.schema.json`](values.schema.json) | Draft-07 schema validating all values (incl. upstream pass-through surface) in CI |
| [`kubespray/group_vars/all/containerd.yml`](kubespray/group_vars/all/containerd.yml) | Ansible overrides: layer retention + certs.d ownership |
| [`kubespray/playbooks/prepare-containerd-spegel.yml`](kubespray/playbooks/prepare-containerd-spegel.yml) | Rolling one-shot node prep playbook |
