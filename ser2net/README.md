# ser2net

Exposes the Zigbee coordinator's USB serial port as a cluster-internal TCP
endpoint so Zigbee2MQTT can reach it without `hostPath` or a privileged
application container.

The chart runs [ser2net](https://github.com/cminyard/ser2net) 4.6.4 on the node
that owns the adapter and exposes it at
`tcp://<release>.<namespace>.svc.cluster.local:6638`.

## Why this exists

A Kubernetes `hostPath` volume only bind-mounts a device node into the
container. It does not add the device's major/minor numbers to the container's
device-cgroup allowlist, so an unprivileged process gets `EPERM` when it opens
`/dev/ttyUSB0`. The alternatives are a device plugin/CDI allocator, a
privileged application container, or a small serial proxy. This chart is the
proxy:

- only ser2net is privileged, and it opens exactly one serial device;
- Zigbee2MQTT stays unprivileged and can use the `restricted` Pod Security
  Standard;
- the bridge is reachable only from the configured client namespace over
  ClusterIP.

## Image

The image is built from [`images/ser2net/Dockerfile`](../images/ser2net/Dockerfile)
in this repository. It is a multi-stage build:

- builder: official `debian:13.7-slim`, with the pinned package
  `ser2net=4.6.4-1`;
- runtime: `gcr.io/distroless/base-nossl-debian13:nonroot`, with only the
  ser2net binary, the `net`, `serialdev`, and `mdns` gensio modules, and their
  shared-library closure copied in.

The distroless runtime has no shell, package manager, or SSL modules. The
`base-nossl` variant is used instead of `base-debian13` because the bridge only
speaks raw TCP; SSL support would add OpenSSL for no benefit. `static-debian13`
is not usable here because ser2net and gensio are dynamically linked.

### Publishing and immutability

`.github/workflows/container-images.yaml` builds `linux/amd64` and
`linux/arm64` images on pull requests and publishes them on `main` and version
tags. It pushes:

- `sha-<full commit sha>` — the immutable build tag;
- `4.6.4-1` on `main` — the human-readable build tag (the digest pin takes
  precedence in the chart);
- semver tags for `v*` releases.

Builds include SBOM and provenance attestations and are signed with cosign
keyless. The workflow's follow-up job resolves the digest of the new image and
opens a PR that sets `image.digest` in `values.yaml`. Once that PR is merged,
the Deployment uses `repository@sha256:...` and is immutable; `image.tag` is
kept as the human-readable build tag.

Verify a published image:

```bash
cosign verify \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp 'https://github.com/mkroman/k8s/.github/workflows/container-images.yaml@.*' \
  ghcr.io/mkroman/ser2net@sha256:<digest>
```

If the GHCR package is private, set `imagePullSecrets` in the chart.

## Components

| Resource | Purpose |
| --- | --- |
| Deployment | One ser2net process, pinned to the coordinator node |
| ConfigMap | `ser2net.yaml`: raw TCP listener and serial device |
| Service | ClusterIP endpoint consumed by Zigbee2MQTT |
| NetworkPolicy | Ingress only from the client namespace; no egress |

## Prerequisites

- A node labelled
  `feature.node.kubernetes.io/zigbee-coordinator=true` with the coordinator
  attached. The `node-feature-discovery` chart sets this label for the Silicon
  Labs CC2652RB adapter.
- The serial device at `serial.devicePath` (default `/dev/ttyUSB0`). Prefer
  the stable `/dev/serial/by-id/...` path once it is known; keep the
  corresponding `/dev/serial/by-id` symlink on the node (leave
  `serial.hostPathType` empty for a symlink).
- The device's group GID. Debian and Ubuntu use 20 (`dialout`); verify with
  `stat -c %g /dev/ttyUSB0` and set `serial.groupId`.
- Zigbee2MQTT configured with
  `config.serial.port: tcp://ser2net.ser2net.svc.cluster.local:6638` (adjust
  the release name/namespace if different). Its NetworkPolicy must allow
  egress to this namespace and port.

## Configuration

```yaml
image:
  repository: ghcr.io/mkroman/ser2net
  tag: "4.6.4-1"
  digest: ""          # set by the image workflow's digest PR

serial:
  devicePath: /dev/ttyUSB0
  hostPathType: CharDevice
  settings: "115200n81"
  groupId: 20
  local: true

service:
  port: 6638
```

`settings` is passed directly to ser2net's `serialdev` connector: baud,
parity, data bits, stop bits. `local: true` tells ser2net to ignore modem
control lines, which USB serial adapters generally do not provide.

The generated ConfigMap is:

```yaml
%YAML 1.1
---
connection: &zigbee
  accepter: tcp,0.0.0.0,6638
  connector: serialdev,/dev/ttyUSB0,115200n81,local
  options:
    kickolduser: true
    max-connections: 1
```

Only one client may use the port at a time. A new connection replaces the old
one (`kickolduser`), matching Zigbee2MQTT's exclusive use of the coordinator.

## Security notes

- The ser2net container must be `privileged: true`; that is the only way a
  `hostPath` character device reaches the container device cgroup. It still
  runs as UID 1000, keeps `readOnlyRootFilesystem: true`, and receives the
  device group through `supplementalGroups`. Privileged mode grants access to
  every device number, so keep this workload on the dedicated coordinator
  node and do not add anything else to the container.
- The distroless runtime has no shell or package manager. Only the ser2net
  binary, three gensio modules, and their library closure are present.
- The Service is `ClusterIP`; the protocol is raw, unauthenticated serial data.
  Do not expose it with an Ingress, NodePort or LoadBalancer. If the bridge
  must cross an untrusted network, put an authenticated TLS tunnel in front
  and use a client that supports it; Zigbee2MQTT's `tcp://` transport is raw.
- The namespace needs the `privileged` Pod Security Standard because of the
  `hostPath` device and the privileged container. The Zigbee2MQTT namespace
  does not.

## Validation

```bash
docker build -t ser2net:test images/ser2net
helm lint ser2net
helm template ser2net ser2net --namespace ser2net > /tmp/ser2net.yaml
kubeconform -strict -summary /tmp/ser2net.yaml
yamllint --no-warnings --config-file hack/ci/yamllint.yaml \
  ser2net/Chart.yaml ser2net/values.yaml
./hack/ci/validate-helm.sh ser2net
```

The image has been exercised with a real PTY pair: ser2net carried data in
both directions between a TCP client and the serial device. The chart has not
been run against a physical coordinator.

## Known gaps

- No e2e entry: kind has no USB serial device. Zigbee2MQTT's e2e uses the
  in-pod coordinator emulator instead.
- ser2net logs a harmless `Unable to start mdns: Out of memory` line at
  startup. gensio initializes its optional mDNS backend before per-port
  settings are applied; there is no avahi daemon in the container. The message
  is not an OOM and does not affect raw TCP.
- Raw TCP loses USB modem-control semantics (DTR/RTS). If the coordinator
  needs them for a reset or bootloader path, test it or use a device plugin
  instead.
- Single replica only. Two processes cannot share the coordinator.
