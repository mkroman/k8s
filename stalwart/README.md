# Stalwart

Deploys the [Stalwart](https://stalw.art/) mail and collaboration server
(SMTP, IMAP, POP3, JMAP, CalDAV, CardDAV, WebDAV) on Kubernetes.

The chart follows Stalwart's official Kubernetes reference but hardens it:

- StatefulSet with a stable pod hostname; RocksDB in kind and PostgreSQL in
  production
- restricted Pod Security, read-only root filesystem, and seccomp
  `RuntimeDefault`
- AppArmor is opt-in. Set `podSecurityContext.appArmorProfile.type` and
  `containerSecurityContext.appArmorProfile.type` to `RuntimeDefault` on nodes
  that enable AppArmor. Kind does not, and the restricted Pod Security Standard
  rejects `Unconfined`, so the default leaves AppArmor unset.
- no Kubernetes API token, no host namespaces, no privileged containers
- default-deny NetworkPolicy with explicit mail, management and egress rules
- the plain HTTP management listener is never part of the mail LoadBalancer
- cert-manager `Certificate` and scheduled `ReloadTlsCertificates` CronJob
- Prometheus `ServiceMonitor` and `PrometheusRule` with the repo's monitor label
- an e2e PostSync probe plus the generic in-cluster upgrade test

## Important: configuration lives in the database

Stalwart stores almost all configuration in its data store. The chart renders
only `/etc/stalwart/config.json` (the DataStore connection), from a ConfigMap
or a Secret. Domains, listeners, TLS Certificate objects, DKIM, DNS management,
accounts, tracers and the Prometheus exporter are configured through the
WebUI, `stalwart-cli`, or a declarative plan.

This means:

- Changing `values.yaml` does not create domains or mailboxes.
- A backup of the data store includes the configuration.
- The first install needs a recovery administrator because the chart always
  provides `config.json` and the server never enters bootstrap mode.

## Prerequisites

- A public IP address with a PTR record matching the server hostname.
- Port 25 open in both directions, plus inbound 443, 465 and 993.
- DNS records for the server hostname and the autoconfiguration names
  (`autoconfig`, `autodiscover`, `mta-sts`, `ua-auto-config`).
- Outbound HTTPS 443 (WebUI bundle, spam rules, ASN data, ACME, DNS providers)
  and DNS 53.
- The `letsencrypt-production-dns` ClusterIssuer from the cert-manager chart.
- A PostgreSQL database and role. Production uses
  `postgres-rw.rwx.svc.cluster.local` and database `stalwart_prod`; the role
  needs `CREATE` on the schema (Stalwart creates its own tables).
- A Secret `stalwart-postgres` in the `stalwart` namespace with a `config.json`
  key containing the complete DataStore configuration. The username and
  password are read from this Secret; no credentials are rendered from
  `values-prod.yaml`:
  ```json
  {
    "@type": "PostgreSql",
    "host": "postgres-rw.rwx.svc.cluster.local",
    "port": 5432,
    "database": "stalwart_prod",
    "authUsername": "<database role>",
    "authSecret": {"@type": "Value", "secret": "<database password>"}
  }
  ```
  The non-secret `config` block in `values-prod.yaml` is only used for chart
  validation and the NetworkPolicy database egress rule; keep it consistent
  with the Secret. Seal the Secret with the repository's
  `op2secret | kubeseal` workflow, or create it directly for a disposable
  cluster. Rotating it requires `kubectl rollout restart statefulset/stalwart`
  because an external Secret is not checksummed by Helm.
- A SealedSecret `stalwart-recovery-admin` before the first production sync:
  - `STALWART_RECOVERY_ADMIN`: `admin:<password>`
  - `STALWART_USER`: `admin`
  - `STALWART_PASSWORD`: `<password>`
- A SealedSecret `stalwart-metrics`:
  - `username`: `prometheus`
  - `password`: `<metrics password>`
- A Secret `stalwart-cli` with a permanent administrator's `STALWART_USER`
  and `STALWART_PASSWORD`, or `STALWART_TOKEN`, for the certificate reload
  CronJob.

The recovery administrator is a full-access master credential. Create a
permanent administrator, then set `recoveryAdmin.enabled: false` and upgrade.

## First install

```bash
kubectl -n stalwart port-forward svc/stalwart-admin 8080:8080
# open http://127.0.0.1:8080/admin
```

Sign in with the recovery administrator, create a permanent administrator,
configure the default hostname/domain and listeners, then store the permanent
administrator's credentials in `stalwart-cli` and remove the recovery
administrator from the values.

Port 8080 is plain HTTP and is only exposed by the ClusterIP admin Service. It
must never be added to the mail LoadBalancer.

Additional Service ports belong in `service.mail.extraPorts`; the chart adds
their target ports to the mail NetworkPolicy automatically. Use numeric
targets (or the named container ports from the StatefulSet) so the policy and
Service agree.

## TLS with cert-manager

`values-prod.yaml` creates a cert-manager Certificate for:

- `mail.rwx.im`
- `autoconfig.rwx.im`
- `autodiscover.rwx.im`
- `mta-sts.rwx.im`
- `ua-auto-config.rwx.im`

The Secret is mounted at `/etc/stalwart/tls`. Create a Stalwart `Certificate`
object once, pointing at the files:

```json
{
  "certificate": {"@type": "File", "filePath": "/etc/stalwart/tls/tls.crt"},
  "privateKey": {"@type": "File", "filePath": "/etc/stalwart/tls/tls.key"}
}
```

The optional `certificate.reload` CronJob calls
`Action/ReloadTlsCertificates` every six hours so cert-manager renewals take
effect without a pod restart. It reads `STALWART_USER` and `STALWART_PASSWORD`,
or `STALWART_TOKEN`, from `certificate.reload.credentialsSecret`. Production
uses the permanent administrator in `stalwart-cli`, not the recovery
administrator, because the recovery Secret is removed after setup.

`values-prod.yaml` references the repo's `letsencrypt-production-dns`
ClusterIssuer. It uses Cloudflare DNS-01, so it works for the mail
LoadBalancer IP where the HTTP-01 Traefik challenge cannot. The
`cloudflare-api-access` SealedSecret in the cert-manager chart must contain a
token with DNS edit access to the `rwx.im` zone. Alternatively, point
`certificate.issuerRef` at an existing wildcard certificate issuer and keep the
matching Secret name.

## Prometheus

Enable the exporter in Stalwart with a file-based password:

```json
{
  "prometheus": {
    "@type": "Enabled",
    "authUsername": "prometheus",
    "authSecret": {
      "@type": "File",
      "filePath": "/etc/stalwart/metrics/password"
    }
  }
}
```

The chart mounts `stalwart-metrics` at `/etc/stalwart/metrics`. The
`ServiceMonitor` scrapes the admin Service on `8080` at
`/metrics/prometheus` with basic auth. The `monitoring` namespace is allowed to
reach the management port by the NetworkPolicy.

Configure the exporter before enabling `metrics.exporter.enabled`. The
ServiceMonitor can be created earlier; without the exporter the scrape returns
404 and `up` is zero.

The `PrometheusRule` alerts on:

- a down target and an outbound-queue backlog (only when
  `metrics.exporter.enabled` is true);
- a pod that is not Ready for two minutes;
- container restarts;
- `ImagePullBackOff`/`ErrImagePull`;
- data PVC usage above `metrics.prometheusRule.pvcUsageThreshold`;
- a cert-manager certificate expiring within
  `metrics.prometheusRule.certificateExpiryDays`;
- a failed TLS reload CronJob.

The pod and PVC alerts need kube-state-metrics and kubelet metrics; the
certificate alerts need the cert-manager metrics endpoint. The PrometheusRule
is rendered when `metrics.prometheusRule.enabled` is true and does not require
the ServiceMonitor. `StalwartDown` and `StalwartQueueBacklog` are also gated on
`metrics.exporter.enabled` so an unconfigured exporter does not page.

## Storage and backup

Production uses the PostgreSQL data store; `values-prod.yaml` disables the
local data PVC. Stalwart creates its tables in `stalwart_prod` on first start.
Use the CloudNativePG backup and restore workflow for the `postgres` cluster.

The kind e2e suite keeps the embedded RocksDB store at `/var/lib/stalwart`.
The StatefulSet uses `persistentVolumeClaimRetentionPolicy: Retain` for both
deletion and scale-down, and `helm uninstall` leaves the PVC behind. RocksDB
has no online backup; use one of:

- a Longhorn volume snapshot of the whole data directory, or
- stop the pod and use the official offline export:

```bash
stalwart --config /etc/stalwart/config.json --export /var/backups/stalwart
```

Back up the data store first and any external blob store afterwards. Back up
`config.json` and the environment separately. Test restores regularly.

To restore a snapshot, create a PVC from it and set `persistence.existingClaim`
instead of the volumeClaimTemplate.

## Logging

The default Stalwart installation creates a `Log` tracer writing daily files to
`/var/log/stalwart`. The chart mounts a writable `emptyDir` there so the
read-only root filesystem works.

`emptyDir` is not retained. Logs written there disappear when the pod is
replaced, which is exactly when upgrade and crash diagnostics matter. Switch the
tracer to `Stdout` in the WebUI and collect the container logs through the
cluster logging stack. Keep the `Log` tracer only for short-lived debugging.

## Memory

RocksDB sizes its block cache and the message memory budget from the container
memory limit. The default limit is 2 GiB; production uses 4 GiB. The official
memory worksheet shows that a 2 GiB host can reach roughly 2477 MiB of maximum
cache and budget values, so raise the limit or lower `messagesLarge`,
`groupwareLarge`, and `cacheSize` for large accounts. See
<https://stalw.art/docs/performance/memory/>.

## Upgrades

Pin an exact image tag (the chart defaults to the pinned `appVersion`). Back up
before every minor or major upgrade. The StatefulSet replaces one pod at a
time and reuses the PVC. A major release may change the data format; read its
release notes first.

Stalwart is a single-replica stateful service, so a pod-template change is a
short but real outage. A same-values `helm upgrade` does not restart the pod
and causes no outage. The chart adds checksum annotations for the ConfigMap and
for a chart-managed recovery-admin Secret, so those changes do restart the pod.
An external `recoveryAdmin.existingSecret` cannot be checksummed at render
time; after rotating it, restart the StatefulSet explicitly:

```bash
kubectl rollout restart statefulset/stalwart
```

A failed image pull leaves the StatefulSet pod in `ImagePullBackOff`.
`helm rollback` (including `--atomic`/`--rollback-on-failure`) restores the
StatefulSet revision, but the StatefulSet controller does not replace a pod
that never became Ready. Delete the stuck pod after the rollback so it is
recreated from the restored revision:

```bash
helm rollback stalwart <revision> --wait
kubectl delete pod stalwart-0
kubectl rollout status statefulset/stalwart
```

Avoid `kubectl delete pod --force` for routine operations. If the old
container is still releasing the RocksDB LOCK, the replacement pod can fail
once with `Failed to open database: ... Resource temporarily unavailable` and
then recover on restart. Use a graceful delete and allow the configured
`terminationGracePeriodSeconds` and `preStop` drain to finish.

## Testing

The chart is exercised by the repo's Astrarium e2e suite:

```bash
astrarium run --app stalwart
astrarium upgrade --app stalwart
```

Before creating the cluster, the e2e workflow renders the chart and checks every
image, including the probe image, with `docker buildx imagetools inspect`
against the registry proxy. The e2e run then deploys cert-manager and
Prometheus Operator as prerequisites, issues a self-signed Certificate, applies
the ServiceMonitor and PrometheusRule, and runs the PostSync probe (admin readiness, SMTP banner, SMTPS/IMAPS TCP,
HTTPS TCP reachability). The upgrade command then runs the generic in-cluster
upgrade test because `apps.yaml` sets `upgrade.enabled: true`. It installs
`values-upgrade-from.yaml` (`v1.1.0`), writes a marker under
`/var/lib/stalwart`, upgrades to the current `v1.1.1` image, verifies that the
StatefulSet replaced the pod, checks marker survival, and checks PVC retention
after uninstall.

Static checks:

```bash
./hack/ci/validate-helm.sh stalwart
./hack/ci/validate-chart-cases.sh
./hack/ci/validate-e2e-contract.sh
```

`hack/ci/chart-cases/stalwart.yaml` covers the data-safety guards: RocksDB
without persistence, multiple replicas, a recovery administrator without a
Secret, a certificate without DNS names, and a ServiceMonitor without the admin
Service.

### Chaos test results

A disposable single-node kind cluster was exercised with a continuous
in-cluster SMTP and management probe. The app was installed without
cert-manager or Prometheus Operator, so these numbers isolate the StatefulSet
and storage path.

| Scenario | SMTP gap | Admin gap | Result |
| --- | ---: | ---: | --- |
| Same-values `helm upgrade` | 0 s | 0 s | No pod restart |
| ConfigMap checksum change | 8.7 s | 7.6 s | Pod replaced, marker intact |
| Chart-managed recovery-admin Secret change | 8.7 s | 7.6 s | New environment value, marker intact |
| Graceful pod delete | 9.7 s | 7.5 s | Pod replaced, marker intact |
| Forced pod delete | 26.5 s | 26.5 s | One RocksDB LOCK startup failure, then recovery |
| Forced pod delete during a rollout | 26.4 s | 27.5 s | Helm succeeded after an automatic restart |
| Bad image upgrade, then manual rollback | 410 s | 411 s | Rollback required deleting the stuck pod |
| Bad image upgrade with `--atomic` | 403 s | 402 s | Auto-rollback also required deleting the stuck pod |

The PVC UID and a data marker were unchanged in every scenario, and a JMAP
mailbox created before the final rollouts survived. The bad-image cases are
deliberately destructive: they measure recovery behaviour, not a normal
upgrade. The normal 8–10 s gap is the cost of a single replica; clients should
retry idempotent reads and connections.

## Known limitations

- One replica only. Clustering requires a shared data store, blob/search
  stores, Keep or Redis, and a coordinator; this chart deliberately refuses a
  second replica so two independent servers cannot silently diverge.
- The chart does not create domains, mailboxes, listeners, or the Stalwart
  `Certificate`/`Metrics` objects. Those are database configuration.
- Kind cannot validate public DNS, PTR, port 25, or the production ACME
  challenge.
- The Prometheus exporter remains disabled until it is enabled in Stalwart's
  database; the e2e test validates the monitoring CRs, not a scrape.

## License

The Stalwart Community edition is AGPL-3.0-only. Enterprise features require a
Stalwart Enterprise License. This chart follows the official Kubernetes
deployment model and does not redistribute the Stalwart binary.
