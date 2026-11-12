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
in this repository. It is a single-stage build on official
`debian:13.7-slim` with the pinned package `ser2net=4.6.4-1`.

This is deliberately a normal Debian image rather than distroless. The
distroless variants do not contain ser2net or gensio, so they required copying
the binary, the full multiarch library directory, and the gensio modules from
a builder. The Debian package manager keeps that dependency set correct and
the Dockerfile simple, at the cost of a larger image with a shell and package
manager. The image is about 165 MB uncompressed and about 41 MB compressed for
`linux/amd64`.

### Publishing and immutability

`.github/workflows/container-images.yaml` runs when `images/ser2net/` changes,
on manual dispatch, and on a weekly schedule for security updates.
`.github/workflows/container-images-release.yaml` additionally runs for
image-specific release tags such as `ser2net-0.2.0`. It builds `linux/amd64`
and `linux/arm64` images on pull requests and publishes them on `main` and
release tags. It pushes:

- `sha-<full commit sha>` — the immutable build tag;
- `4.6.4-1` on `main` — the human-readable build tag (the digest pin takes
  precedence in the chart);
- the version from a `ser2net-<version>` release tag.

Builds include SBOM and provenance attestations and are signed with cosign
keyless. The weekly scheduled build passes `APT_UPGRADE=true`, disables the
Docker build cache, and forces the base image to be pulled, so it executes
`apt-get update`/`upgrade` against current repository metadata and picks up
Debian security updates for the base packages and the installed ser2net
package, even when this repository has not changed. The
workflow's follow-up job resolves the digest of the new image and opens a PR
that sets `image.digest` in `values.yaml`. Once that PR is merged, the
Deployment uses `repository@sha256:...` and is immutable; `image.tag` is kept
as the human-readable build tag.

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
- The image is Debian slim, so it includes a shell and package manager. The
  container still runs as UID 1000 with a read-only root filesystem; treat the
  extra userland as attack surface. Base-image bumps come from Renovate; the
  weekly scheduled workflow rebuilds with current Debian security updates.
  Dispatch it manually after bumping `SER2NET_VERSION` if an immediate rebuild
  is needed.
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

- No readiness or liveness probes. Any TCP probe opens a real ser2net
  connection; with `max-connections: 1` and `kickolduser: true` it would kick
  the active Zigbee2MQTT connection or block it. Kubernetes restarts the
  container if ser2net exits, and Zigbee2MQTT retries the TCP connection while
  the bridge restarts.
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
