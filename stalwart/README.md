# Stalwart

Deploys the [Stalwart](https://stalw.art/) mail and collaboration server
(SMTP, IMAP, POP3, JMAP, CalDAV, CardDAV, WebDAV) on Kubernetes.

The chart follows Stalwart's official Kubernetes reference but hardens it:

- StatefulSet with a stable pod hostname and a per-pod RocksDB volume
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
only `/etc/stalwart/config.json` (the DataStore connection). Domains,
listeners, TLS Certificate objects, DKIM, DNS management, accounts, tracers and
the Prometheus exporter are configured through the WebUI, `stalwart-cli`, or a
declarative plan.

This means:

- Changing `values.yaml` does not create domains or mailboxes.
- A backup of the data store includes the configuration.
- The first install needs a recovery administrator because the chart always
  renders `config.json` and the server never enters bootstrap mode.

## Prerequisites

- A public IP address with a PTR record matching the server hostname.
- Port 25 open in both directions, plus inbound 443, 465 and 993.
- DNS records for the server hostname and the autoconfiguration names
  (`autoconfig`, `autodiscover`, `mta-sts`, `ua-auto-config`).
- Outbound HTTPS 443 (WebUI bundle, spam rules, ASN data, ACME, DNS providers)
  and DNS 53.
- cert-manager and the `letsencrypt-prod` ClusterIssuer.
- The `ssd-gp1x3` StorageClass (kind overrides this to `standard`).
- A SealedSecret `stalwart-recovery-admin` before the first production sync:
  - `STALWART_RECOVERY_ADMIN`: `admin:<password>`
  - `STALWART_USER`: `admin`
  - `STALWART_PASSWORD`: `<password>`
- A SealedSecret `stalwart-metrics`:
  - `username`: `prometheus`
  - `password`: `<metrics password>`

The recovery administrator is a full-access master credential. Create a
permanent administrator, then set `recoveryAdmin.enabled: false` and upgrade.

## First install

```bash
kubectl -n stalwart port-forward svc/stalwart-admin 8080:8080
# open http://127.0.0.1:8080/admin
```

Sign in with the recovery administrator, create a permanent administrator,
configure the default hostname/domain and listeners, then remove the recovery
administrator from the values.

Port 8080 is plain HTTP and is only exposed by the ClusterIP admin Service. It
must never be added to the mail LoadBalancer.

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
effect without a pod restart. It needs a Secret with `STALWART_USER` and
`STALWART_PASSWORD`, or `STALWART_TOKEN`.

`values-prod.yaml` references a ClusterIssuer named `letsencrypt-prod`. The
repo's cert-manager chart currently ships `letsencrypt-production` (HTTP-01
through Traefik); either create `letsencrypt-prod` with a DNS-01/Cloudflare
solver, or change `certificate.issuerRef.name` to the issuer you actually use.
If the issuer uses HTTP-01 through Traefik, a dedicated mail LoadBalancer IP
will not receive the challenge; use a DNS-01 issuer or an existing wildcard
certificate.

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
`/metrics/prometheus` with basic auth. The `PrometheusRule` alerts on a down
target and an outbound-queue backlog. The `monitoring` namespace is allowed to
reach the management port by the NetworkPolicy.

## Storage and backup

The data directory is `/var/lib/stalwart` on a `ssd-gp1x3` PVC. The StatefulSet
uses `persistentVolumeClaimRetentionPolicy: Retain` for both deletion and
scale-down, and `helm uninstall` leaves the PVC behind.

RocksDB has no online backup. Use one of:

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
read-only root filesystem works. For cluster log collection, switch the tracer
to `Stdout` in the WebUI; `kubectl logs` then shows the events.

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

## Testing

The chart is exercised by the repo's Astrarium e2e suite:

```bash
astrarium run --app stalwart
astrarium upgrade --app stalwart
```

The e2e run deploys cert-manager and Prometheus Operator as prerequisites,
issues a self-signed Certificate, applies the ServiceMonitor and PrometheusRule,
and runs the PostSync probe (admin readiness, SMTP banner, SMTPS/IMAPS TCP,
HTTPS TCP reachability). The upgrade command then runs the generic in-cluster
upgrade test because `apps.yaml` sets `upgrade.enabled: true`; it writes a
marker under `/var/lib/stalwart`, upgrades the chart, verifies pod replacement
and marker survival, and checks PVC retention after uninstall.

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
