# Zigbee2MQTT

Deploys [Zigbee2MQTT](https://www.zigbee2mqtt.io/) 2.14.3 on Kubernetes using
the official `ghcr.io/koenkk/zigbee2mqtt` image. It bridges a USB Zigbee
coordinator to MQTT and Home Assistant. By default the coordinator is reached
through the [ser2net](../ser2net/) TCP bridge, which keeps this workload
unprivileged; a direct `hostPath` mode remains available as a fallback.

## Deployment model

The upstream project publishes a Helm chart
([`Koenkk/zigbee2mqtt-chart`](https://github.com/Koenkk/zigbee2mqtt-chart)),
but this chart deliberately follows the official container deployment instead:

- The upstream chart generates its own `configuration.yaml` and cannot preserve
  inline `devices` and `groups` blocks. Its `statefulset.command` default is an
  empty map, which also prevents a wrapper chart from adding a seeding command
  because Helm cannot coalesce a list over a map.
- The upstream chart defaults to `privileged: true` with a root container. This
  chart runs as UID 1000. Its default transport is the ser2net bridge, so the
  application container does not touch the host device at all. Direct hostPath
  mode is still supported, but only that mode needs privileged mode; a root
  container is never required. The
  [official security guide](https://www.zigbee2mqtt.io/guide/installation/14_securing.html)
  recommends a dedicated unprivileged user.

This chart keeps deployment and scalar settings in structured `config` values
and the large device/group inventories in `files/devices.yaml` and
`files/groups.yaml`. On first start an init container copies
`configuration.yaml`, `devices.yaml`, and `groups.yaml` into the data volume.
Chart-managed `config` keys are then applied as environment overrides, so the
PVC keeps runtime-owned keys while Git changes still take effect; see
[Configuration](#configuration). Separating the inventories avoids Helm
coalescing deleting explicit `null` values (for example a device-level
`homeassistant.name: null`) and keeps the files diff-friendly.

### Configuration options considered

| Option | Trade-off |
| --- | --- |
| One raw `config:` string | Exact and simple, but indent-sensitive, impossible to override per key, and no schema. |
| One structured map + `toYaml` | Ergonomic and `--set` friendly, but Helm deletes explicit `null` values during coalescing, which changes device behaviour. |
| Opaque existing ConfigMap/Secret (Sironite chart) | Good when config lives in an external secret manager, but the chart no longer owns defaults or validation. |
| Structured scalars + separate device/group files (this chart) | Keeps values ergonomic and validated, preserves `null`/comments in inventories, and matches the official split-file format. |

The chart renders `config` with `toYaml`, but reads `files/devices.yaml` and
`files/groups.yaml` verbatim with `.Files.Get`. Those files are valid
Zigbee2MQTT YAML and are not reshaped by Helm.

### Components

| Resource | Purpose |
| --- | --- |
| StatefulSet | One Zigbee2MQTT process; the single replica avoids concurrent serial-device and PVC access |
| PVC (`<release>-data`) | `/app/data`: configuration, devices, groups, `database.db`, `coordinator_backup.json`, state, logs |
| Service | ClusterIP frontend on port 8124 |
| Headless Service | Governing Service for the StatefulSet |
| ConfigMap | First-start `configuration.yaml`, `devices.yaml`, and `groups.yaml` |
| NetworkPolicy | Frontend ingress; DNS, MQTT, optional ser2net TCP, and OTA HTTPS egress |

## Prerequisites

- The [ser2net](../ser2net/) Application is healthy on the coordinator node,
  or the chart is switched to direct-device mode. The default
  `config.serial.port` is
  `tcp://ser2net.ser2net.svc.cluster.local:6638` and `config.serial.adapter`
  is `zstack` (a TCP path cannot be auto-discovered as a USB adapter).
  - ser2net must be scheduled on the node labelled
    `feature.node.kubernetes.io/zigbee-coordinator=true` and must have access
    to the serial device. See the [ser2net chart README](../ser2net/README.md).
  - `networkPolicy.ser2netNamespace` and `networkPolicy.ser2netPort` must
    match the bridge; they default to `ser2net` and `6638`.
  - Direct mode is the fallback: set `coordinator.devicePath` to the node
    device and `config.serial.port` to the same path. That mode needs
    `coordinator.privileged: true` (default) and a namespace that admits
    `hostPath` and privileged pods. Prefer the stable
    `/dev/serial/by-id/...` path; see the
    [adapter documentation](https://www.zigbee2mqtt.io/guide/configuration/adapter-settings.html).
- An MQTT broker reachable at `mqtt://10.0.3.3` (default). The NetworkPolicy
  allows the `mosquitto` namespace and `10.0.3.3/32`; adjust
  `networkPolicy.mqttNamespace`, `mqttCidr`, and `mqttPort` if needed.
- The Sealed Secrets controller (`kube-system`) for the MQTT password.

### Coordinator access

The default is a TCP serial bridge:

```yaml
config:
  serial:
    port: tcp://ser2net.ser2net.svc.cluster.local:6638
    adapter: zstack
coordinator:
  devicePath: ""   # no hostPath, no /run/udev, no USB nodeSelector
```

In this mode the Zigbee2MQTT container is unprivileged and can run under the
`restricted` Pod Security Standard. The [ser2net chart](../ser2net/) owns the
host device and is the only privileged workload. Its NetworkPolicy accepts
connections only from `networkPolicy.ser2netNamespace`; this chart's egress
policy allows that namespace on `networkPolicy.ser2netPort`.

Throughput and semantics are those of a raw TCP serial bridge. DTR/RTS modem
control lines are not carried over plain TCP; if the coordinator needs them
for a reset or bootloader path, test it or prefer direct mode.

#### Direct hostPath mode (fallback)

A `hostPath` volume only bind-mounts the device node into the container. It
does not add the device's major/minor numbers to the container's device
cgroup, which containerd/runc enforces when the device is opened. Setting
`coordinator.devicePath` therefore also sets `privileged: true` on the
Zigbee2MQTT container (`coordinator.privileged`, default `true`). Without it,
adapter discovery still matches `/dev/ttyUSB0`, but opening it fails with
`EPERM` (`Operation not permitted`). That errno identifies the device cgroup:
a Unix mode or group problem would be `EACCES` (`Permission denied`).
Capabilities, `supplementalGroups`, seccomp and Pod Security Admission labels
do not change the device cgroup.

Only the Zigbee2MQTT container becomes privileged. The seeding init container,
coordinator emulator and e2e probe stay unprivileged with all capabilities
dropped. The main container keeps the non-root UID/GID, read-only root
filesystem and `RuntimeDefault` seccomp. `allowPrivilegeEscalation` is `true`
because Kubernetes rejects it being `false` together with `privileged: true`,
and the capabilities list is omitted because a capability allowlist does not
gate device-cgroup access. Privileged mode grants that container access to
every device number, so it is a real trade-off.

Direct mode requires the namespace to admit `hostPath` and privileged pods;
the default `zigbee2mqtt` namespace in `apps/values.yaml` is `restricted`
because the default transport is ser2net. Relabel it only when using direct
mode. Set `coordinator.privileged: false` only when something else allocates
the device, such as a device plugin or CDI.

## Secret

`configuration.yaml` references the MQTT password as
`'!secret.yaml password'`. The chart mounts the Secret key at
`/app/data/secret.yaml`:

```yaml
password: <mqtt password>
```

Create and seal it:

```bash
cat > /tmp/z2m-secret.yaml <<'EOF'
password: <mqtt password>
EOF

kubectl create secret generic zigbee2mqtt-secrets \
  --namespace zigbee2mqtt \
  --from-file=secret.yaml=/tmp/z2m-secret.yaml \
  --dry-run=client -o yaml |
  kubeseal \
    --controller-namespace kube-system \
    --controller-name sealed-secrets-controller \
    --format yaml > zigbee2mqtt/templates/sealedsecret.yaml
```

Commit the resulting `SealedSecret` (any `templates/*.yaml` file is rendered by
Helm). The chart mounts it read-only; changing the password requires a Pod
restart because the file is mounted with `subPath`.

The Application is registered in `apps/values.yaml` and syncs automatically.
The chart does not create the Secret itself, so until the sealed secret is
committed (or a Secret with the same name exists out-of-band) the StatefulSet
Pod stays in `ContainerCreating` and the Application is not Healthy. Commit the
sealed secret before relying on the application.

Setting `secret.name: ""` disables both the secret mount and the committed
SealedSecret template. `values-kind.yaml` uses this because kind has no Sealed
Secrets controller or CRD.

If the existing Zigbee network key is not in the restored data directory,
add it to the secret and reference it from `config.advanced.network_key`:

```yaml
# secret.yaml
network_key: [0x01, 0x03, 0x05, ...]
```

```yaml
# config.advanced
network_key: '!secret.yaml network_key'
```

Changing or omitting the network key requires re-pairing every device.

## Configuration

`config` contains the scalar Zigbee2MQTT settings and is rendered with
`toYaml`. Device and group inventories are the chart files
`files/devices.yaml` and `files/groups.yaml`; the template reads them verbatim.
`devices.yaml` and `groups.yaml` are written to `/app/data` only when the
corresponding file does not already exist, so device pairing and frontend
group edits are not clobbered by a Git sync.

`config` is also rendered as `ZIGBEE2MQTT_CONFIG_*` environment variables on the
Zigbee2MQTT container. Zigbee2MQTT applies those on top of
`configuration.yaml`, without persisting them. This means:

- chart changes to `config` are part of the Pod template and roll the
  StatefulSet automatically;
- the PVC remains the source of truth for runtime-owned keys not present in
  the chart, such as `advanced.network_key`, `permit_join`, and frontend edits;
- secret references such as `!secret.yaml password` are not turned into env
  vars, so the mounted Secret keeps working;
- arrays and objects are JSON-encoded for the env override;
- removing a key from `config` stops overriding it and lets the PVC value
  reappear; clear it on the PVC if that is not what you want.

- To change a scalar setting, edit `config` in `values.yaml` (or use
  `--set config.mqtt.server=...`). Argo CD rolls the StatefulSet and the new
  env override takes effect. Do not delete `configuration.yaml` from the PVC
  unless you have the network key/data backup.
- To change devices or groups, edit `files/devices.yaml` or
  `files/groups.yaml`, then replace the matching file on the PVC and restart.
  Using the frontend is safer for groups while the app is running. These files
  are intentionally not values, so `--set` cannot change them; that is what
  keeps `null`, quoting, and comments intact.
- `service.port` must equal `config.frontend.port`. When
  `coordinator.devicePath` is set, it must equal `config.serial.port`; when it
  is empty (the default ser2net/emulator/remote mode), `config.serial.port`
  must start with `tcp://`. The chart fails to render if these rules diverge.
  `values.schema.json` also rejects wrong types such as a string frontend
  port.
- `coordinator.privileged` defaults to `true` and is used only in direct
  hostPath mode. The default ser2net transport keeps the container
  unprivileged. See [Coordinator access](#coordinator-access).

The frontend binds `0.0.0.0` because the Service is the access path; the
Service is `ClusterIP`, and access should be through an authenticated,
TLS-terminating ingress/gateway. See the
[frontend documentation](https://www.zigbee2mqtt.io/guide/configuration/frontend.html)
for `auth_token`, `base_url`, and TLS options.

## Persistence and migration

The PVC keeps `database.db` and, for Texas Instruments coordinators,
`coordinator_backup.json`. Copy the **entire** data directory when migrating;
the project FAQ and troubleshooting guide explicitly call this out. To keep
the existing network, restore the data before the first start, or provide the
network key as above. Without either, Zigbee2MQTT generates a new network and
all devices must be paired again.

The PVC carries `argocd.argoproj.io/sync-options: Prune=false,Delete=false` and
`helm.sh/resource-policy: keep` so pruning or deleting the Application does not
delete the Zigbee database.

Set `persistence.existingClaim` to use a pre-created claim instead. Setting
`persistence.enabled=false` uses `emptyDir` and loses the network state on
every restart; it is for testing only.

## Security notes

- Runs as UID/GID 1000, non-root, with `fsGroup` 1000, `readOnlyRootFilesystem`,
  `RuntimeDefault` seccomp, all capabilities dropped, no privilege escalation,
  and no Kubernetes API token.
- The default ser2net transport keeps the Zigbee2MQTT container unprivileged:
  no `hostPath`, no `/run/udev`, no `supplementalGroups`, and the namespace
  can use the `restricted` Pod Security Standard. The bridge is the only
  privileged workload.
- Direct hostPath mode is opt-in by setting `coordinator.devicePath`. A
  `hostPath` device is not added to the container device cgroup, so that mode
  needs `coordinator.privileged: true`; the group (`supplementalGroups: [20]`)
  is still required because privileged mode does not bypass Unix file
  permissions for a non-root process. Privileged mode is a broad grant
  (access to all device numbers), so prefer ser2net.
- The frontend is not exposed and has no `auth_token` in the default config.
  Anyone who can reach it has full control of the Zigbee network.

## Backup and restore

Use Longhorn snapshots/backups of the PVC, or copy the data directory while
Zigbee2MQTT is stopped. The project's backup guidance is: copy the whole data
folder, including `coordinator_backup.json` for TI coordinators. Zigbee2MQTT
also exposes an MQTT backup request
(`zigbee2mqtt/bridge/request/backup`) that returns a base64 zip of the data
folder; see the
[MQTT topics documentation](https://www.zigbee2mqtt.io/guide/usage/mqtt_topics_and_messages.html).

To restore: scale the StatefulSet to zero, restore the files into the PVC
(including `configuration.yaml`, `devices.yaml`, and `groups.yaml`), scale back
to one. The init container will not overwrite existing files. If the files come
from a root-run deployment, make them writable by UID/GID 1000 first
(`chown -R 1000:1000`); otherwise Zigbee2MQTT cannot update its own
configuration and database.

## Upgrade and rollback

1. Back up the PVC.
2. Bump `appVersion` and the image tag together (the image tag defaults to
   `appVersion`).
3. Render and validate (see below), then let Argo CD sync.
4. Watch the logs for configuration migrations and adapter startup.

Zigbee2MQTT migrates `configuration.yaml` on startup. Roll back the image only
after restoring a matching data directory backup; newer schema versions may not
be readable by older releases.

## Observability

Logs go to the container output (console) and, by Zigbee2MQTT's default, to
rotated log directories on the PVC; collected stdout is available to the
cluster's Vector agent. Set `config.advanced.log_output: [console]` if you do
not want file logs on the data volume. There is no native Prometheus endpoint;
the project publishes health on the MQTT topic `zigbee2mqtt/bridge/health`. The
existing `mqtt2prometheus` Application can export selected MQTT values if
needed.

## Validation

```bash
helm lint zigbee2mqtt
helm template zigbee2mqtt zigbee2mqtt --namespace zigbee2mqtt \
  > /tmp/zigbee2mqtt.yaml
kubeconform -strict -summary /tmp/zigbee2mqtt.yaml
yamllint --no-warnings --config-file hack/ci/yamllint.yaml \
  zigbee2mqtt/Chart.yaml zigbee2mqtt/values.yaml
./hack/ci/validate-helm.sh zigbee2mqtt
```

The environment overrides were exercised with the real 2.14.3 image against an
existing config containing `serial.port: /dev/ttyUSB0`. Zigbee2MQTT used the
rendered TCP endpoint and `adapter: zstack` instead of the old device path.
Secret references were checked to ensure they are not emitted as env vars.

The rendered chart has been exercised against the real 2.14.3 image as UID
1000 with a read-only root filesystem, a writable data directory, and the
secret file mounted. The default ser2net/TCP path was validated with the
coordinator emulator; the ser2net chart was separately exercised with a PTY
pair. Direct hostPath mode was tested on kind with a fake character device:
the unprivileged pod fails to open it with `EPERM`, while the privileged pod
with supplemental GID 20 opens it. The chart has **not** been run against a
physical USB coordinator, so the device node, its group and the node label
must be verified on the target node.

## e2e / kind

The kind-based e2e cluster (`hack/e2e/run-app.sh`) has no USB coordinator and
no `feature.node.kubernetes.io/zigbee-coordinator=true` node, so the chart can
run there only with a coordinator emulator. `values-kind.yaml` (next to
`Chart.yaml`; the runner appends it to the Application's valueFiles) wires this
up. It is e2e-only and must never be enabled in production.

- The emulator ([romanlum/zstack-adapter-emulator](https://github.com/romanlum/zstack-adapter-emulator))
  runs as a pod sidecar (`coordinator.emulator.enabled: true`) and speaks just
  enough of the TI Z-Stack (ZNP) protocol over TCP for zigbee-herdsman
  (`adapter: zstack`) to start the Zigbee stack. It provides **no real radio**:
  no RF, no pairing, no devices — it only proves that Zigbee2MQTT deploys,
  starts, and reports itself online.
- `coordinator.devicePath: ""` disables the device hostPath, the `/run/udev`
  hostPath, and the USB `nodeSelector` (there is no USB node in kind). With the
  emulator, the serial port is the pod-local loopback sidecar address
  `config.serial.port: tcp://127.0.0.1:6638`, so no Service is needed;
  `networkPolicy.ser2netNamespace` is empty for the same reason.
- The remaining overrides adapt to the kind cluster: default StorageClass
  (`persistence.storageClass: ""`), anonymous MQTT against the e2e mosquitto
  app (`config.mqtt.server: mqtt://mosquitto.mosquitto.svc.cluster.local:1883`,
  empty user/password, `secret.name: ""` so no Secret is mounted).
- The PostSync probe (`templates/e2e/probe.yaml`, gated by `e2e.enabled`) is a
  Helm `post-install,post-upgrade` hook Job that subscribes to
  `zigbee2mqtt/bridge/state` on the e2e broker with `mosquitto_sub` and fails
  the sync unless it receives the retained online state within
  `e2e.timeoutSeconds` (bare `online` or, since Zigbee2MQTT 2.x, JSON
  `{"state":"online"}`). It needs no RBAC because it never touches the
  Kubernetes API.

## Known gaps

- Single replica only. Zigbee2MQTT is not horizontally scalable: two processes
  cannot share the coordinator or the data volume.
- The HTTP probes use `/` on the frontend. If you enable `frontend.auth_token`,
  they will receive 401 and restart the Pod; use TCP probes or add an
  unauthenticated health path in a future chart revision.
- No native Prometheus endpoint; use `mqtt2prometheus` or the MQTT health topic.
- The chart does not create an Ingress. The Service is `ClusterIP` by default.
- The default real-device path depends on the ser2net Application. It is a
  raw, unauthenticated TCP bridge and must stay cluster-internal. If it is
  down, Zigbee2MQTT retries the connection but cannot reach the coordinator.
- The ser2net bridge is the only privileged workload. It opens exactly one
  device, but a privileged container still has access to every device number;
  keep it pinned to the coordinator node and do not add sidecars.
- Direct hostPath mode still needs a privileged Zigbee2MQTT container and a
  namespace that admits hostPath pods; the default namespace is restricted.
  A device plugin/CDI allocator remains the least-privilege alternative.
- The e2e suite runs the emulator over TCP and never exercises the physical
  ser2net/USB path, so a real coordinator needs a manual node test.

## Sources

- [Zigbee2MQTT documentation](https://www.zigbee2mqtt.io/)
- [Kubernetes installation](https://www.zigbee2mqtt.io/guide/installation/08_kubernetes.html)
- [Docker installation](https://www.zigbee2mqtt.io/guide/installation/02_docker.html)
- [Securing the installation](https://www.zigbee2mqtt.io/guide/installation/14_securing.html)
- [MQTT configuration](https://www.zigbee2mqtt.io/guide/configuration/mqtt.html)
- [Devices and groups](https://www.zigbee2mqtt.io/guide/configuration/devices-groups.html)
- [Adapter settings](https://www.zigbee2mqtt.io/guide/configuration/adapter-settings.html)
- [Troubleshooting / adapter access](https://www.zigbee2mqtt.io/guide/installation/20_zigbee2mqtt-fails-to-start_crashes-runtime.html)
- [Official Helm chart](https://github.com/Koenkk/zigbee2mqtt-chart)
- [Sironite Helm chart](https://github.com/Sironite/helm-zigbee2mqtt) (opaque
  ConfigMap/Secret + init-copy pattern)
- [dawg-io Kustomize manifests](https://github.com/dawg-io/zigbee2mqtt-kustomize)
  (separate `devices.yaml`/`groups.yaml`, first-run copying)
- [k8s-at-home chart](https://github.com/k8s-at-home/charts/tree/master/charts/stable/zigbee2mqtt)
  (structured `config` values copied on first run)
- [Official image](https://github.com/Koenkk/zigbee2mqtt/pkgs/container/zigbee2mqtt)
- [ser2net serial bridge](https://github.com/cminyard/ser2net)
- License: GPL-3.0
