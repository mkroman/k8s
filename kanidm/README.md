# kanidm

A Helm chart for [Kanidm](https://kanidm.com/) — a simple, secure and fast identity
management platform supporting OAuth 2.1, OpenID Connect, LDAP and SCIM — designed for
GitOps deployment with Argo CD.

The chart deploys the official `kanidm/server` image as a single-replica Deployment with
the Kanidm database on a persistent volume. Kanidm terminates TLS itself for both HTTPS
and LDAPS, so the chart manages the TLS certificate lifecycle with cert-manager and the
configuration is rendered into a `server.toml` ConfigMap (rolled out automatically via a
checksum annotation).

## Requirements

| Dependency | Since | Usage |
|------------|-------|-------|
| Kubernetes | 1.25+ | |
| Helm | 3.x / 4.x | `values.schema.json` validation requires Helm 3+ |
| cert-manager | 1.x | Issues the server TLS certificate (`certificate.enabled`, default). Alternatively bring your own secret via `kanidm.tls.existingSecret`. |

Kanidm must be reachable on its public domain with end-to-end TLS. Since it terminates
TLS inside the pod, traffic may not be TLS-terminated at an ingress or load balancer:

- **Ingress**: pass TLS through to the pod. For ingress-nginx, enable ssl-passthrough
  cluster-wide (`--enable-ssl-passthrough`) and keep the default ingress annotations.
- **Traefik**: set `ingressRouteTCP.enabled: true` — the chart creates the `IngressRouteTCP`
  (TCP routing with `tls.passthrough`) for HTTPS, and a second one for LDAPS via
  `ingressRouteTCP.ldaps` when `kanidm.ldap.enabled` is true. Requires the
  [Traefik Kubernetes CRDs](https://doc.traefik.io/traefik/reference/dynamic-configuration/kubernetes-crd/)
  in the cluster.
- **LoadBalancer**: set `service.type: LoadBalancer` and route the domain to it.

## Installing

```bash
helm install kanidm ./kanidm \
  --namespace kanidm --create-namespace \
  --set kanidm.domain=idm.example.com \
  --set certificate.issuerRef.name=letsencrypt-production
```

The `kanidm.domain` value is the single most important setting: it is used in
security-critical contexts (WebAuthn, OAuth2) and changing it later breaks every
registered credential (it requires `kanidmd domain rename`). The TLS certificate is
issued for it, and the ingress host defaults to it.

## Deploying with Argo CD

The chart is a plain Helm chart, so any Argo CD source type works. If the chart and its
environment values live in the same repository, a single-source Application is enough --
value files are resolved relative to the Application's `path` (mirroring the
`values-prod.yaml` convention of the other charts in this repository):

```yaml
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: kanidm
  namespace: argocd
spec:
  project: default
  destination:
    server: https://kubernetes.default.svc
    namespace: kanidm
  source:
    repoURL: https://git.example.com/k8s.git
    targetRevision: main
    path: kanidm
    helm:
      valueFiles:
        - values-production.yaml
  syncPolicy:
    automated:
      prune: true
      selfHeal: true
    syncOptions:
      - CreateNamespace=true
```

To keep the environment values in a separate repository (or branch), use a multi-source
Application: the source tagged `ref: values` becomes available to the chart source
through the `$values` prefix, and the remainder of the path is relative to the root of
that repository. Leave `path` unset on the ref source so it is used solely as a source of
value files:

```yaml
spec:
  sources:
    # The chart itself.
    - repoURL: https://git.example.com/k8s.git
      targetRevision: main
      path: kanidm
      helm:
        valueFiles:
          # $values resolves to the source tagged `ref: values` below; the remainder
          # of the path is relative to that repository's root.
          - $values/kanidm/production.yaml
    # Referenced by $values above. No `path`: this source only provides value files.
    - repoURL: https://git.example.com/deployments.git
      targetRevision: main
      ref: values
```

Notes for Argo CD:

- The pod template carries a `checksum/config` annotation over the rendered
  `server.toml`, so Argo CD syncs automatically roll the pod when the configuration
  changes — no manual restarts needed.
- `values.schema.json` is validated by Helm during `helm template`, so misconfigured
  values fail the sync with a clear error instead of deploying.
- The Deployment uses `strategy: Recreate` (the database is a single file on a
  `ReadWriteOnce` volume). Do not scale it beyond one replica.

## Day-2 operations

Initialise the break-glass admin accounts (also printed in the install NOTES):

```bash
kubectl -n kanidm exec deploy/kanidm -- \
  /sbin/kanidmd scripting recover-account admin
kubectl -n kanidm exec deploy/kanidm -- \
  /sbin/kanidmd scripting recover-account idm_admin
```

Verify the server:

```bash
kubectl -n kanidm exec deploy/kanidm -- \
  /sbin/kanidmd scripting healthcheck
```

Upgrades: review the [release notes](https://github.com/kanidm/kanidm/releases), then run
the pre-upgrade check before bumping `image.tag` / `appVersion`:

```bash
kubectl -n kanidm exec deploy/kanidm -- \
  /sbin/kanidmd domain upgrade-check
```

Backups: the built-in `[online_backup]` task writes database backups to
`kanidm.onlineBackup.path` on the data volume (daily at 22:00 UTC by default, 7 versions
kept). Replicate them off-cluster, e.g. with a CronJob syncing the backups directory to
object storage.

After a TLS certificate renewal, restart the pod so kanidm reloads its key material:

```bash
kubectl -n kanidm rollout restart deploy/kanidm
```

## Security defaults

The chart follows the Kubernetes [restricted Pod Security
Standard](https://kubernetes.io/docs/concepts/security/pod-security-standards/) and the
upstream [security hardening](https://kanidm.github.io/kanidm/stable/security_hardening.html)
guidance:

- Runs as non-root (uid/gid 1000) with `RuntimeDefault` seccomp, all capabilities
  dropped, no privilege escalation and a read-only root filesystem.
- The service account is not created with a token (`automountServiceAccountToken: false`)
  — Kanidm never talks to the Kubernetes API.
- A NetworkPolicy allows ingress only on the service ports and denies all egress except
  cluster DNS (Kanidm is self-contained; add exceptions via
  `networkPolicy.additionalEgress`).
- ConfigMap and TLS secret volumes are mounted read-only with `0440` permissions (no
  world-readable bits, matching the upstream permission guidance).
- No secrets are templated through values — the only credentials (the break-glass admin
  passwords) are generated by Kanidm itself. Inject any additional secrets through
  `env`/`envFrom` from your own Secret sources.
- Resource requests/limits and TLS probes (`/status` over HTTPS) are enabled by default.

## Values

See [values.yaml](values.yaml) for the full list with inline documentation. Every key is
described there and validated by [values.schema.json](values.schema.json) (JSON Schema
draft-07, maintained alongside `values.yaml`): Helm rejects unknown or malformed values
at template/install time, and the `yaml-language-server` reference at the top of
`values.yaml` gives completion and validation directly in a capable editor.
