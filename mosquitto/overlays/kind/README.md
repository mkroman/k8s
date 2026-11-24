# kind e2e overlay

This overlay adapts the mosquitto application to the Astrarium kind e2e
suite. Astrarium points the Application at `mosquitto/overlays/kind` when that
directory exists.

## Why it exists

The base manifests target a real cluster in three ways that cannot work in a
throwaway kind cluster:

1. **No LoadBalancer.** The base Service is `LoadBalancer` with a reserved
   `loadBalancerIP` and `externalTrafficPolicy: Local`. kind has no
   LoadBalancer implementation, so the Service would never get an address.
   The overlay switches it to `ClusterIP`, which is all the e2e consumers
   (the probe Job, and the downstream zigbee2mqtt app) need.
2. **The SealedSecret cannot be decrypted.** A fresh sealed-secrets
   controller generates a new key pair per cluster, so the sealed
   `mosquitto-passwd` data can never be unsealed and the app would stay
   unhealthy forever. The overlay deletes the SealedSecret (`$patch: delete`).
3. **Credentials are unnecessary in e2e.** The overlay generates its own
   `mosquitto.conf` (derived from the base one: only `allow_anonymous true`
   and the removal of `password_file` differ) and swaps the Deployment's
   projected volume (configMap + secret) for a plain ConfigMap volume with
   that config. Anonymous access is **e2e-only**; production keeps the
   password-protected listener.
4. **The local-path PVC is unnecessary here.** This test proves MQTT
   connectivity, not storage. The overlay switches `/mosquitto/data` to an
   `emptyDir` and deletes the base PVC, so a slow or permission-sensitive
   local-path volume cannot crash-loop the broker or leave a Pending PVC in
   the Application health.

## Layout note

The manifests live in `../base` so both the app root and this overlay can
include them: the app root `../../kustomization.yaml` is a thin wrapper
including `./base`, and this overlay includes `../../base`. Deploying the app
root renders base unchanged; the e2e runner points the Application at this
overlay instead.

## Deterministic ConfigMap naming

The base app generates its ConfigMap with `configMapGenerator`, which appends
a content hash to the name. The overlay adds a separate, deterministic
`mosquitto-e2e-config` ConfigMap with `disableNameSuffixHash: true`, and the
Deployment patch mounts it explicitly with `defaultMode: 0644`.

Do not use `behavior: replace` here. It is sensitive to how the cluster's
Kustomize/Argo CD version merges the base hash-suffixed resource and can leave
the pod mounting an empty ConfigMap; the broker then exits with
`Unable to open config file /mosquitto/config/mosquitto.conf`.

## NetworkPolicy

The base kustomization does not wire in
`base/manifests/networkpolicy.yaml`, so production deploys no policy. The
kind e2e cluster's CNI enforces NetworkPolicy, and the overlay ships its own
`networkpolicy.yaml` variant of the same manifest:

- ingress mirrors the base intent: MQTT (1883) from in-cluster addresses
  only — in kind, both the pod and service CIDRs sit inside `10.0.0.0/8`;
- egress allows in-cluster addresses for the broker pods selected by the
  policy. The probe Job is not selected: the overlay does not give its pod
  template `app.kubernetes.io/name: mosquitto`, so probe egress is not
  restricted by this policy.

If the base kustomization is ever changed to deploy its NetworkPolicy, drop
the copy from this overlay or the build will fail with a duplicate resource.

## Probe support details

- `job.yaml` runs an MQTT publish/subscribe round-trip as an Argo CD PostSync
  hook (`argocd.argoproj.io/hook`) with
  `hook-delete-policy: BeforeHookCreation,HookSucceeded`. It publishes a
  retained message to `e2e/probe` (publish-before-subscribe is race-free
  because of the retain flag), subscribes and requires the exact message
  back, and clears the retained message. The publish/subscribe pair retries
  for up to 90 seconds so a broker that is not ready yet cannot fail the
  PostSync hook prematurely. A timeout, empty payload, or wrong payload
  fails the Job. It has no Kubernetes API access and needs no RBAC.
- The Job intentionally does **not** set `spec.selector`: the API server
  rejects a hand-written selector on a Job, and the generated
  `batch.kubernetes.io/controller-uid`/`job-name` labels can never match the
  broker pod. Do not add one.
- Broker pods are tagged `app.kubernetes.io/component: broker` and the
  Service selector requires that label. If the app labels were ever added to
  the probe pod, it still could not become a Service endpoint.
- `deployment-patch.yaml` keeps `fsGroup: 1883` so the data mount and any
  future writable volume remain usable by the image's UID.
