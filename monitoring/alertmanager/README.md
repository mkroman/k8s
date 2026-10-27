# Alertmanager

The `Alertmanager` CR currently has no `configSecret`, so the Prometheus
Operator generates the default configuration with a `null` receiver. Rules are
evaluated, but alerts are not delivered anywhere.

## Required inputs

To route alerts, provide one of:

- An email destination (one or more addresses) and permission to use the
  existing Grafana SMTP relay, plus the SMTP host, port, sender address, username
  and password.
- A Slack/Discord/ntfy/generic webhook URL (and channel for Slack).
- A dedicated SMTP account if the Grafana relay must not be reused.

The `grafana-env` SealedSecret in the `grafana` namespace cannot be reused here:
SealedSecrets are bound to their namespace/name, and mounting the whole secret
would expose Grafana's OAuth client secret, database password and S3 keys.

## Recommended layout

Use the default `configSecret` name (`alertmanager-cluster`) so the CR needs no
change. The Secret must contain:

- `alertmanager.yaml` — the routing tree
- `smtp-password` — the SMTP password, referenced with `auth_password_file`

The operator mounts the config Secret at `/etc/alertmanager/config/`, so the
password file is available at
`/etc/alertmanager/config/smtp-password`.

```yaml
global:
  resolve_timeout: 5m
  smtp_hello: infra.rwx.im
  smtp_smarthost: <smtp-host>:587
  smtp_from: <alerts@example.com>
  smtp_auth_username: <smtp-username>
  smtp_auth_password_file: /etc/alertmanager/config/smtp-password
  smtp_require_tls: true

route:
  receiver: email
  group_by: [alertname, cluster, environment]
  group_wait: 30s
  group_interval: 5m
  repeat_interval: 4h
  routes:
    - matchers: ['severity="critical"']
      receiver: email
      group_wait: 10s
      repeat_interval: 1h
    - matchers: ['severity=~"info|none"']
      receiver: "null"

receivers:
  - name: "null"
  - name: email
    email_configs:
      - to: <alerts@example.com>
        send_resolved: true

inhibit_rules:
  - source_matchers: ['severity="critical"']
    target_matchers: ['severity=~"warning|info"']
    equal: [alertname, cluster, instance]
```

For Slack, replace the receiver with:

```yaml
receivers:
  - name: slack
    slack_configs:
      - api_url_file: /etc/alertmanager/config/slack-webhook-url
        channel: '#alerts'
        send_resolved: true
```

Add the webhook URL as another key in the same Secret.

## Sealing

Create the plaintext files locally (do not commit them), then seal:

```bash
kubectl create secret generic alertmanager-cluster -n monitoring \
  --from-file=alertmanager.yaml=./alertmanager.yaml \
  --from-file=smtp-password=./smtp-password \
  --dry-run=client -o yaml | kubeseal -o yaml \
  > monitoring/alertmanager/manifests/sealedsecret.alertmanager.yaml
```

Add the SealedSecret to `monitoring/alertmanager/kustomization.yaml`.

## Verification

```bash
kubectl -n monitoring get sealedsecret alertmanager-cluster
kubectl -n monitoring get secret alertmanager-cluster
kubectl -n monitoring exec alertmanager-cluster-0 -c alertmanager -- \
  amtool check-config /etc/alertmanager/config_out/alertmanager.env.yaml

kubectl -n monitoring port-forward svc/alertmanager 9093:9093
curl -XPOST -H 'Content-Type: application/json' \
  -d '[{"labels":{"alertname":"TestAlert","severity":"warning"}}]' \
  http://localhost:9093/api/v2/alerts
```

Check `alertmanager_config_last_reload_successful == 1` and
`alertmanager_notifications_total{integration="email"}`. A malformed
configuration keeps the previous config active, so a typo shows up as a failed
reload metric rather than a crash.

## Notes

- The default `externalUrl` is `http://127.0.0.1:8001/api/v1/proxy/...`, so
  notification links point at localhost. Expose Alertmanager behind Traefik (and
  the existing basic-auth middleware) or accept broken links.
- Alertmanager has no persistent storage; silences are lost on restart.
- Add a `Watchdog`/dead-man's-switch rule so a broken notification path is
  visible instead of silent.
