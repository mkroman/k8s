# Quickwit

Quickwit stores logs and traces in B2-backed indexes. The Helm values in
`values.yaml` manage the Kubernetes log index through the chart's bootstrap
jobs (`bootstrap.enabled` + `seed.indexes`). Quickwit's OTLP log/trace indexes
are auto-created and are not declared here.

The upstream bootstrap job is disabled because it derives its Job name from the
index ID without sanitizing it; the Kubernetes logs index contains underscores,
which are invalid in Kubernetes object names. `templates/configmap-bootstrap.yaml`
and `templates/job-create-indices.yaml` render the equivalent ConfigMap and Job
with an RFC 1123-safe Job name while keeping the index ID unchanged.

## Indexes

| Index | Producer | Retention |
| --- | --- | --- |
| `vector-kubernetes_logs-v0_2` | Vector agent/aggregator JSON ingest | 30 days |

The `vector-kubernetes_logs-v0_2` mapping is managed here and created by the
bootstrap job. The old `v0_1` index is left untouched for its existing
retention period.

Quickwit auto-creates its OTLP log and trace indexes on indexer startup. They
are intentionally **not** declared in `values.yaml` and are not managed by this
repository; use Quickwit's defaults for them.

## OTLP endpoints

The Quickwit indexer service exposes:

- OTLP/HTTP: `http://quickwit-indexer.quickwit-prod.svc.cluster.local:7280/api/v1/otlp/v1/traces`
- OTLP/HTTP logs: `http://quickwit-indexer.quickwit-prod.svc.cluster.local:7280/api/v1/otlp/v1/logs`
- OTLP/gRPC: `quickwit-indexer.quickwit-prod.svc.cluster.local:7281`

Quickwit 0.8.2 accepts **binary OTLP protobuf** over HTTP. Do not send JSON or
newline-delimited JSON to these paths.

## OTLP indexes

Quickwit 0.8.2 auto-creates the OTLP log and trace indexes when the OTLP
endpoint starts. They are not declared in `values.yaml`; this repository does
not own their mappings or retention. Vector simply posts to the OTLP endpoints
and Quickwit routes the data to its own indexes.

## Grafana

The Quickwit datasource `0.6.3` supports log and trace search. The
`grafana/templates/configmap.datasources.yaml` manifest provisions both
datasources from GitOps:

- `quickwit-prod` (logs, `vector-kubernetes_logs-v0_2`)
- `quickwit-traces` (Quickwit's auto-created OTLP trace index)

The two datasources are cross-linked through the plugin's
`tracesDatasourceUid` / `logsDatasourceUid` fields. Add a separate datasource
for Quickwit's auto-created OTLP log index if you want to browse OTel logs
directly; the Kubernetes logs index has no normalized severity field yet.

The Jaeger-compatible API is also available on the searcher for Quickwit's
auto-created trace index:

```text
http://quickwit-searcher.quickwit-prod.svc.cluster.local:7280/api/v1/<quickwit-auto-trace-index>/jaeger
```
