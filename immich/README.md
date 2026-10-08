# immich

Self-hosted [Immich](https://immich.app/) (photo and video backup solution)
on Kubernetes, following the official documentation at
https://immich.app/docs/install/kubernetes.

## Overview

The chart deploys two Immich components as `apps/v1` Deployments:

| Component | Image | Port | Purpose |
| --- | --- | --- | --- |
| `immich-server` | `ghcr.io/immich-app/immich-server` | 2283 | API + web UI + background workers; serves the media library |
| `immich-machine-learning` | `ghcr.io/immich-app/immich-machine-learning` | 3003 | ML inference (smart search, facial recognition) - optional |

Neither PostgreSQL nor Redis/Valkey is bundled. This mirrors the [official
Helm chart](https://github.com/immich-app/immich-charts), which also expects
both to be provided externally.

## Prerequisites

- Kubernetes >= 1.25, Helm >= 3.
- **PostgreSQL** with the VectorChord extension available (`pgvector` is
  detected as a fallback). The recommended setup is
  [CloudNativePG](https://cloudnative-pg.io/) with the
  [`tensorchord/cloudnative-vectorchord`](https://github.com/tensorchord/cloudnative-vectorchord)
  image, per the [official chart guidance](https://github.com/immich-app/immich-charts/blob/main/README.md).
  Database files belong on local SSD-class storage - never on a network share:
  https://immich.app/docs/install/requirements
- **Redis/Valkey** reachable from the cluster.
- A PersistentVolume for the media library (created by the chart by default,
  or provide `server.persistence.library.existingClaim` as the official chart
  recommends).

## Installing the chart

```console
# 1. Create the credential Secrets (chart never renders inline DB passwords).
kubectl create namespace immich
kubectl -n immich create secret generic my-immich-database \
  --from-literal=username=immich \
  --from-literal=password='<db-password>'
kubectl -n immich create secret generic my-immich-redis \
  --from-literal=redis-username=default \
  --from-literal=redis-password='<redis-password>'

# 2. Install with the external services configured.
helm install my-immich ./immich -n immich \
  --set database.host=my-postgres.postgres.svc.cluster.local \
  --set database.existingSecret=my-immich-database \
  --set redis.host=rwx-master.rwx-redis-prod.svc.cluster.local \
  --set redis.port=6739 \
  --set redis.existingSecret=my-immich-redis
```

Complete `helm install` notes print the remaining first steps (admin sign-up,
backup scheduling). See also https://immich.app/docs/install/post-install.

## Configuration

Key values (see `values.yaml` for the full, documented set):

| Value | Default | Description |
| --- | --- | --- |
| `database.host` | `""` | **Required.** PostgreSQL host; render fails without it. |
| `database.port` | `5432` | PostgreSQL port. |
| `database.existingSecret` | `<fullname>-database` | Secret with `username`/`password` keys (CloudNativePG-compatible). |
| `database.urlKey` | `""` | Alternative: read the full `DB_URL` from this secret key (e.g. CNPG's `uri`). |
| `database.sslMode` | `""` | `DB_SSL_MODE`, e.g. `require`. |
| `database.vectorExtension` | `""` | `vectorchord` or `pgvector`; empty auto-detects. |
| `redis.host` | `rwx-master.rwx-redis-prod.svc.cluster.local` | Redis host. |
| `redis.port` | `6739` | Redis port. |
| `redis.dbIndex` | `0` | `REDIS_DBINDEX`. |
| `redis.existingSecret` | `<fullname>-redis` | Secret holding username **and** password (`redis-username`/`redis-password`). |
| `server.persistence.library` | 50Gi RWO | Media library at `/data`; `existingClaim` for a pre-created PVC. |
| `machineLearning.enabled` | `true` | `false` + `machineLearning.url` for external ML; `false` + empty URL disables ML. |
| `machineLearning.image.tag` | appVersion | Append `-cuda`/`-rocm`/`-openvino`/`-armnn`/`-rknn` for [hardware acceleration](https://immich.app/docs/features/ml-hardware-acceleration). |
| `ingress.enabled` | `false` | Ingress for the web UI; HTTPS recommended. |
| `networkPolicy.enabled` | `false` | Per-component policies: server egress is DNS + HTTPS + DB/Redis ports; ML ingress is limited to this release's server pods and egress to DNS + HTTPS (model downloads). |
| `podSecurityContext` | UID/GID 1000 | Matches the official rootless compose (`user: 1000:1000`). |

### Environment variables

`env` applies to both containers, `server.env` / `machineLearning.env` to one
each. All documented variables from
https://immich.app/docs/install/environment-variables can be set there, e.g.
`IMMICH_LOG_LEVEL`, `IMMICH_TRUSTED_PROXIES` (when behind a reverse proxy) or
`IMMICH_ALLOW_SETUP=false` once the admin account exists.

### Machine learning

The ML container downloads models into its `/cache` volume on demand; keeping
that volume persistent avoids re-downloads. For GPU/accelerator inference use
the matching image tag suffix and schedule onto capable nodes:

```yaml
machineLearning:
  image:
    tag: v3.1.0-cuda
  nodeSelector:
    nvidia.com/gpu.present: "true"
```

## Security

- Pods run as non-root UID/GID 1000 with `fsGroup` for volume ownership
  (mirrors upstream `docker-compose.rootless.yml`), `RuntimeDefault` seccomp,
  dropped capabilities and no privilege escalation.
- ServiceAccount tokens are not mounted (`automount: false`); Immich needs no
  Kubernetes API access.
- Credentials (DB username/password, Redis username/password) are only ever
  read from Secrets via `secretKeyRef`; there is no inline database password
  value, and inline Redis credentials are a testing-only opt-in guarded
  against being combined with `existingSecret`.
- Opt-in per-component `NetworkPolicy` (see table above) and
  `PodDisruptionBudget` are included; the `helm test` pod meets the same
  Pod Security Standards (restricted) profile as the application pods.

## Operations

- **Backups**: the media library PVC and the PostgreSQL database must be
  backed up separately - https://immich.app/docs/administration/backup-and-restore
- **Upgrades**: bump `appVersion`/image tags; read the release notes first -
  https://immich.app/docs/install/upgrading. Database migrations run on
  server start (the startup probe allows ~2.5 minutes).
- **Verify** a release: `helm test <release>` pings `/api/server/ping`.

## Chart verification (offline)

```console
helm lint --strict immich/
helm template r ./immich --set database.host=postgres
helm package immich/ --destination /tmp
```
