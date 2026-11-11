# magistr

A standalone Helm chart for [magistr.dk](https://magistr.dk/), the `#magistr`
IRC quote site: a Rust/Axum web application backed by PostgreSQL.

The upstream project publishes a container image and a Dockerfile, but no Helm
chart or Kubernetes manifests. This chart therefore translates the existing
`rwx-labs/k8s-rwx-apps` Kustomize deployment and the application's documented
configuration into a chart; it is not a wrapper around an upstream chart.

## Research summary

| Item | Value |
| --- | --- |
| Upstream repository | <https://github.com/rwx-labs/magistr.dk> |
| Existing deployment | <https://github.com/rwx-labs/k8s-rwx-apps/tree/main/magistr.dk> |
| Stable release | `v0.2.4` (latest tag; image built 2025-08-29) |
| Image | `ghcr.io/rwx-labs/magistr:0.2.4` |
| Image digest | `sha256:e833aa3e7be57405f459a3f1b85a399fb50e02ff66eb6a711cf365fe831f9500` |
| Architectures | `linux/amd64` only |
| Runtime | distroless `cc-debian13`, user `nonroot` (uid/gid 65532) |
| License | None declared in the upstream repository |

The application is a single stateless container. PostgreSQL is the only
dependency; SQLx migrations run automatically at startup. Static assets are
embedded in the binary, so the container needs no writable filesystem.

## What the chart deploys

- `Deployment` with rolling updates, probes, a restricted security context,
  and an optional soft pod anti-affinity in production.
- `Service` (ClusterIP, port 3000) and a dedicated `ServiceAccount` with token
  mounting disabled.
- `PodDisruptionBudget` (enabled by default).
- Optional `HorizontalPodAutoscaler` (disabled by default).
- Optional ingress-only `NetworkPolicy`.
- Optional Traefik `IngressRoute`s, an HTTP-to-HTTPS `Middleware`, and a
  cert-manager `Certificate`.
- A `helm test` Pod that requests `/` through the Service and therefore
  verifies database connectivity.
- For e2e only: a disposable PostgreSQL `Deployment`/`Service`/`Secret` and a
  PostSync HTTP probe.

## Prerequisites

- Kubernetes 1.27 or newer.
- A PostgreSQL database and a Secret containing its connection URL under
  `DATABASE_URL` (or another key of your choice).
- For `values-prod.yaml`: the Traefik CRDs (`traefik.io/v1alpha1`), a
  `letsencrypt-production` ClusterIssuer, and the existing `postgres` Secret
  in the `magistr-prod` namespace.

The production deployment previously read `DATABASE_URL` from a SealedSecret
named `postgres` in `magistr-prod`. That Secret is cluster-specific and is not
shipped by this chart; create or retain it before installing.

## Installation

Production (the same values Argo CD uses):

```bash
kubectl -n magistr-prod get secret postgres
helm upgrade --install magistr ./magistr \
  --namespace magistr-prod \
  --values magistr/values-prod.yaml
```

Default values (no ingress, no database Secret; useful for rendering and
customisation):

```bash
helm template magistr ./magistr --namespace magistr-prod
```

If `database.existingSecret` is empty, no `MAGISTR_DATABASE_URL` environment
variable is rendered. The application then falls back to its built-in
`localhost` URL and will not become ready; the install notes call this out.

After an install, run the connection test:

```bash
helm test magistr --namespace magistr-prod
```

The test Pod requests `/` through the Service, so it only passes when the
application can reach PostgreSQL.

### Cutting over from the existing Kustomize deployment

The old `magistr.dk/base` Deployment selects pods with only
`app.kubernetes.io/name: magistr`; this chart adds the standard
`app.kubernetes.io/instance` selector label. A Deployment's `spec.selector` is
immutable, so Argo CD cannot update the existing Deployment in place.

Before syncing the new `magistr` Application:

1. Retain the `postgres` Secret. It is not created by this chart; if the old
   Application is deleted with cascade, reapply its SealedSecret from
   `rwx-labs/k8s-rwx-apps` afterwards.
2. Remove the old `magistr-prod` Application (or stop its auto-sync) and delete
   its Deployment, IngressRoutes, Middleware, and Certificate. The old
   Certificate writes the same `magistr.dk-tls` Secret, and cert-manager will
   not let two Certificates own it.
3. Sync the new `magistr` Application and wait for the PostSync/e2e probe or
   run `helm test`.

Expect a short outage while the replacement pods start; the Service and TLS
Secret are recreated by the chart.

## Values

`values.yaml` documents every key. The most important groups are:

| Key | Default | Description |
| --- | --- | --- |
| `replicaCount` | `2` | Number of application replicas. |
| `image.repository` / `tag` / `digest` | `ghcr.io/rwx-labs/magistr` / chart `appVersion` / empty | Image reference. A digest replaces the tag when set. |
| `service.port` / `containerPort` | `3000` | Service port and container listen port. |
| `config.logLevel` | `magistr=info,tower_http=info` | `RUST_LOG` filter. |
| `config.http.compression` | `true` | Sets `MAGISTR_HTTP_COMPRESSION`. |
| `config.http.address` | empty | Optional `MAGISTR_HTTP_ADDRESS`; empty keeps the app default `[::]:3000`. |
| `config.tracing.enabled` | `false` | Sets `MAGISTR_TRACING_ENABLED`. Add `OTEL_EXPORTER_OTLP_ENDPOINT` via `extraEnv` before enabling. |
| `database.existingSecret` / `existingSecretKey` | empty / `DATABASE_URL` | Secret holding the PostgreSQL URL. |
| `podDisruptionBudget.enabled` / `minAvailable` | `true` / `1` | Disruption budget. Production uses `2` with four replicas. |
| `autoscaling.enabled` / `minReplicas` / `maxReplicas` | `false` / `2` / `10` | Optional HPA; `replicaCount` is ignored when enabled. |
| `tests.image` | `mirror.gcr.io/library/busybox:1.37.0` | Image used by `helm test`. |
| `networkPolicy.enabled` | `false` | Ingress-only policy; production enables it. |
| `ingress.*` | disabled | Traefik hosts, TLS Secret, and cert-manager Certificate. |
| `extraEnv`, `extraEnvFrom`, `extraVolumes`, `extraVolumeMounts` | empty | Escape hatches for additional configuration. |

`values-prod.yaml` mirrors the previous production overlay: four replicas,
the same CPU/memory envelope, trace logging, the `postgres` Secret, Traefik
IngressRoutes for `magistr.dk` and `www.magistr.dk`, and a cert-manager
Certificate stored in `magistr.dk-tls`.

`values-kind.yaml` is for `hack/e2e/run-app.sh` only. It disables ingress,
the PDB and the NetworkPolicy, and enables the disposable PostgreSQL fixture
and the HTTP probe.

## Application configuration

The application reads environment variables with the `MAGISTR_` prefix
(Figment, `Env::prefixed("MAGISTR_").split("_")`). The chart sets the ones
that map directly to the configuration struct:

| Variable | Effect |
| --- | --- |
| `MAGISTR_DATABASE_URL` | PostgreSQL connection URL (from the Secret). |
| `MAGISTR_HTTP_ADDRESS` | Listen address; optional. |
| `MAGISTR_HTTP_COMPRESSION` | Enable response compression. |
| `MAGISTR_TRACING_ENABLED` | Enable the OTLP/HTTP span exporter. |
| `RUST_LOG` | Log filter (not prefixed). |

The upstream README also lists `MAGISTR_DATABASE_MAX_CONNECTIONS` and
`MAGISTR_DATABASE_IDLE_TIMEOUT`. With the current `split("_")` provider they
are parsed as nested keys (`database.max.connections`) and do not populate the
`max_connections`/`idle_timeout` fields, so this chart does not expose them.
Use `extraEnv` to add any new variable once the application supports it.

## Database, migrations, and backups

- The schema is owned by the application's SQLx migrations under
  `migrations/`; they run automatically before the HTTP listener starts.
  SQLx takes a PostgreSQL advisory lock while migrating, so replicas starting
  together do not apply the same migration concurrently.
- The chart stores no application data. Quotes live in PostgreSQL; back them
  up with the database's native tooling. In this cluster the `postgres`
  CloudNativePG cluster already has scheduled Barman backups configured.
- The e2e PostgreSQL fixture is not backed up: it uses `emptyDir` and is
  deleted with the pod.

## Health, scaling, and observability

The application has no dedicated health, readiness, or metrics endpoint.

- Startup/liveness probes request `/robots.txt`, which is served from embedded
  static assets and does not touch the database.
- Readiness requests `/`, which queries PostgreSQL; a database outage removes
  the pod from the Service instead of restarting it.
- Logs are JSON on stdout/stderr. There is no Prometheus endpoint, so the
  chart does not create a ServiceMonitor.
- The service is stateless and scales horizontally. The application caches the
  quotes list for 5 minutes and individual quotes for 30 minutes per replica.
  A new quote only flushes the cache of the replica that handled the POST, so
  other replicas may serve a stale list until their cache expires. This is
  existing application behaviour, not a chart limitation.
- An optional HPA is available but disabled by default; the traffic profile is
  not documented, so no scaling target is asserted in production.

## Security notes

- Runs as uid/gid 65532 (`nonroot` in the distroless image), non-root, with a
  read-only root filesystem, `seccompProfile: RuntimeDefault`, all
  capabilities dropped, and no privilege escalation.
- `automountServiceAccountToken` is false on the pod and the ServiceAccount;
  the application never uses the Kubernetes API.
- The Service is ClusterIP by default. TLS terminates at Traefik. The chart
  does not expose the application publicly unless `ingress.enabled` is set.
- Production uses an existing Secret; no credentials are rendered from
  `values.yaml`. The e2e fixture intentionally uses passwordless `trust`
  authentication and must never be enabled outside a disposable cluster.
- The production NetworkPolicy restricts ingress to the `traefik` namespace and
  same-namespace clients. Egress is unrestricted because the database is
  external and its address is not known to the chart.

## E2E

The chart is registered in `apps/values.yaml` as `magistr` and in
`hack/e2e/apps.yaml`. On kind, the runner appends `values-kind.yaml` to
`values-prod.yaml`, starts the PostgreSQL fixture, and waits for the PostSync
probe to receive HTTP 200 from `/` through the Service.

```bash
KEEP_CLUSTER=1 ./hack/e2e/run-app.sh magistr
REUSE_CLUSTER=1 ./hack/e2e/run-app.sh magistr
```

The application image is `linux/amd64` only; the e2e cluster must run amd64
nodes.

## Validation

Run the repository checks:

```bash
./hack/ci/validate-helm.sh magistr
yamllint --no-warnings --config-file hack/ci/yamllint.yaml \
  magistr/Chart.yaml magistr/values*.yaml
```

The chart was rendered and validated with:

```bash
helm lint magistr --with-subcharts
helm template magistr magistr -n magistr-prod | kubeconform -strict -ignore-missing-schemas -summary
helm template magistr magistr -n magistr-prod -f magistr/values-prod.yaml | kubeconform -strict -ignore-missing-schemas -summary
helm template magistr magistr -n magistr-prod -f magistr/values-prod.yaml -f magistr/values-kind.yaml | kubeconform -strict -ignore-missing-schemas -summary
helm template magistr magistr -n magistr-prod --set autoscaling.enabled=true | kubeconform -strict -ignore-missing-schemas -summary
```

Results: all renders valid (CRD-backed Traefik/cert-manager resources skipped
by kubeconform), and `./hack/ci/validate-helm.sh magistr apps` passes.

A disposable kind cluster (`v1.37.0`) was also used for a real install with
`values-prod.yaml` plus `values-kind.yaml`:

- the release installed and the PostSync probe received HTTP 200;
- SQLx created `_sqlx_migrations` and `quotes` in PostgreSQL 18.6;
- `helm test` passed, `POST /ny.php` stored a quote, and the row was confirmed
  in PostgreSQL;
- `helm uninstall` removed every namespaced resource cleanly.

Production values were not installed against the real cluster (the `postgres`
Secret is cluster-specific); the production render was validated statically.

### Intentional differences from `helm create`

The chart follows the generated chart's helper and label conventions, with a
few deliberate departures:

- No generic `Ingress` or Gateway API `HTTPRoute`. The cluster serves this site
  through Traefik `IngressRoute`s, matching the existing deployment. Disable
  `ingress.enabled` if you use another controller.
- Service-account token mounting is hardcoded to `false` rather than exposed
  as `serviceAccount.automount`. The application does not need the Kubernetes
  API, and the secure setting should not be one value away from accidental
  exposure.
- `extraVolumes`/`extraVolumeMounts` are named as escape hatches rather than
  the generated chart's `volumes`/`volumeMounts`; the chart already mounts no
  application volumes, so the values represent additions, not replacements.
- The probes use `/robots.txt` for startup/liveness and `/` for readiness
  because the application publishes no dedicated health endpoint.

## Known assumptions and gaps

- No upstream Helm chart exists, so this chart is a direct translation of the
  Kustomize deployment. The application is not modified.
- The image is single-architecture (`linux/amd64`); a multi-arch node pool is
  not supported by the published tag.
- Ingress is Traefik-specific. If you use another controller, disable
  `ingress.enabled` and create your own Ingress.
- The chart does not manage PostgreSQL in production and does not create the
  existing `postgres` Secret.
- The HPA is optional and disabled by default: no traffic profile or
  application-level metrics justify asserting a scaling target.
- The upstream repository declares no license; review redistribution
  constraints before publishing derived images or chart packages.

## Sources

- Application repository and README:
  <https://github.com/rwx-labs/magistr.dk>
- Configuration struct and Figment provider:
  <https://github.com/rwx-labs/magistr.dk/blob/main/src/config.rs>
- HTTP routes and static assets:
  <https://github.com/rwx-labs/magistr.dk/blob/main/src/http.rs>
- Database migrations and caching:
  <https://github.com/rwx-labs/magistr.dk/blob/main/src/database.rs>
- distroless runtime and entrypoint:
  <https://github.com/rwx-labs/magistr.dk/blob/main/Dockerfile>
- Existing Kustomize deployment:
  <https://github.com/rwx-labs/k8s-rwx-apps/tree/main/magistr.dk>
- PostgreSQL 18 official image volume path:
  <https://hub.docker.com/_/postgres>
- Traefik IngressRoute and TLS documentation:
  <https://doc.traefik.io/traefik/routing/providers/kubernetes-crd/>
- cert-manager Certificate documentation:
  <https://cert-manager.io/docs/usage/certificate/>
