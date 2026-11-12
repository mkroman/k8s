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

## Trusted image

There is no official ser2net container image. This chart deliberately avoids
third-party images:

- the only image is the Docker Official Image
  `docker.io/library/debian:13.7-slim`;
- an init container downloads the pinned Debian package
  `ser2net=4.6.4-1` with `apt-get --download-only` and extracts it, together
  with its Debian dependencies, into a shared `emptyDir`;
- the main container runs that binary as UID 1000 with a read-only root
  filesystem.

The init container needs egress to the Debian archive on every Pod start.
`networkPolicy.allowPackageDownload` allows DNS plus TCP 80/443 for that.
Disable it only if the package is supplied by a local mirror or a future
prebuilt image. If the cluster has no archive access, this chart will not
start; build a pinned image from the same Debian package instead.

## Components

| Resource | Purpose |
| --- | --- |
| Deployment | One ser2net process, pinned to the coordinator node |
| ConfigMap | `ser2net.yaml`: raw TCP listener and serial device |
| Service | ClusterIP endpoint consumed by Zigbee2MQTT |
| NetworkPolicy | Ingress only from the client namespace; DNS/HTTP(S) egress for the package install |

## Prerequisites

- A node labelled
  `feature.node.kubernetes.io/zigbee-coordinator=true` with the coordinator
  attached. The `node-feature-discovery` chart sets this label for the Silicon
  Labs CC2652RB adapter.
- The serial device at `serial.devicePath` (default `/dev/ttyUSB0`). Prefer
  the stable `/dev/serial/by-id/...` path once it is known; keep the
  corresponding `/dev/serial/by-id` symlink on the node.
- The device's group GID. Debian and Ubuntu use 20 (`dialout`); verify with
  `stat -c %g /dev/ttyUSB0` and set `serial.groupId`.
- Zigbee2MQTT configured with
  `config.serial.port: tcp://ser2net.ser2net.svc.cluster.local:6638` (adjust
  the release name/namespace if different). Its NetworkPolicy must allow
  egress to this namespace and port.

## Configuration

```yaml
serial:
  devicePath: /dev/ttyUSB0
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
- The init container runs as root only to run `apt-get` and `dpkg-deb`. It has
  no device mount, no service-account token and no privileges beyond the
  default container capabilities.
- The Service is `ClusterIP`; the protocol is raw, unauthenticated serial data.
  Do not expose it with an Ingress, NodePort or LoadBalancer. If the bridge
  must cross an untrusted network, put an authenticated TLS tunnel in front
  and use a client that supports it; Zigbee2MQTT's `tcp://` transport is raw.
- The namespace needs the `privileged` Pod Security Standard because of the
  `hostPath` device and the privileged container. The Zigbee2MQTT namespace
  does not.

## Validation

```bash
helm lint ser2net
helm template ser2net ser2net --namespace ser2net > /tmp/ser2net.yaml
kubeconform -strict -summary /tmp/ser2net.yaml
yamllint --no-warnings --config-file hack/ci/yamllint.yaml \
  ser2net/Chart.yaml ser2net/values.yaml
./hack/ci/validate-helm.sh ser2net
```

The rendered chart has been exercised against the real Debian package and a
PTY pair: ser2net carried data in both directions between a TCP client and the
serial device. The chart has not been run against a physical coordinator.

## Known gaps

- No e2e entry: kind has no USB serial device. Zigbee2MQTT's e2e uses the
  in-pod coordinator emulator instead.
- The package install requires archive access at Pod start, as described above.
- ser2net logs a harmless `Unable to start mdns: Out of memory` line at
  startup. gensio initializes its optional mDNS backend before per-port
  settings are applied; there is no avahi in the container. The message is
  not an OOM and does not affect raw TCP.
- Raw TCP loses USB modem-control semantics (DTR/RTS). If the coordinator
  needs them for a reset or bootloader path, test it or use a device plugin
  instead.
- Single replica only. Two processes cannot share the coordinator.
