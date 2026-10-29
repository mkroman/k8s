# Vector

Vector runs as two roles:

- **Agent** (`values-agent.yaml`): a DaemonSet on every node, including
  control-plane nodes. It tails Kubernetes container logs, accepts node-local
  OTLP logs/traces, exposes host and Vector metrics locally, and forwards logs
  to the aggregator.
- **Aggregator** (`values-aggregator.yaml`): a single durable StatefulSet. It
  accepts logs from agents and OTLP logs/traces from applications, then writes
  them to Quickwit.

## Data flow

```text
Kubernetes container logs
  agent kubernetes_logs -> aggregator:6000 -> Quickwit JSON ingest
                                             vector-kubernetes_logs-v0_2

OTLP logs / traces (same node)
  application -> vector-agent:4317/4318 -> aggregator:4318 OTLP
                                        -> Quickwit OTLP/HTTP
                                        -> Quickwit auto-created OTLP indexes

OTLP logs / traces (direct)
  application -> vector-aggregator:4317/4318 -> Quickwit OTLP/HTTP
                                             -> Quickwit auto-created OTLP indexes
```

Host and Vector metrics are **not** sent to the aggregator. Each agent exposes
them on `:9090` and Prometheus scrapes the agent Pods directly. This avoids
shipping hundreds of events per second across the cluster only to discard them.

## OTLP endpoints

Applications can send OTLP to either endpoint:

**Same-node agent (recommended for node locality)**

The agent Service uses `internalTrafficPolicy: Local`, so the connection
terminates on the agent running on the application's node.

- gRPC: `vector-agent.vector.svc.cluster.local:4317`
- HTTP: `http://vector-agent.vector.svc.cluster.local:4318`

**Aggregator (direct, cluster-wide)**

- gRPC: `vector-aggregator.vector.svc.cluster.local:4317`
- HTTP: `http://vector-aggregator.vector.svc.cluster.local:4318`

Example environment variables for the node-local path:

```text
OTEL_EXPORTER_OTLP_ENDPOINT=http://vector-agent.vector.svc.cluster.local:4318
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
# or
OTEL_EXPORTER_OTLP_ENDPOINT=http://vector-agent.vector.svc.cluster.local:4317
OTEL_EXPORTER_OTLP_PROTOCOL=grpc
```

Set resource attributes at the application or collector so traces carry useful
identity:

```text
OTEL_RESOURCE_ATTRIBUTES=service.name=my-service,service.version=1.2.3,deployment.environment.name=prod,k8s.cluster.name=rwx
```

## Durable aggregator

The aggregator is an `Aggregator` StatefulSet with a 20 GiB PVC and disk
buffers for all three Quickwit sinks. In-flight logs and traces survive an
aggregator restart. If the aggregator is down long enough to fill the disk
buffers, the sinks block and backpressure the agent/OTLP sources rather than
silently dropping data.

## Network and container hardening

Both releases create ingress-only NetworkPolicies:

- Agent: OTLP clients may reach `:4317`/`:4318`; only Prometheus in the
  `monitoring` namespace may reach `:9090`.
- Aggregator: only agent pods may reach `:6000`; OTLP clients may reach
  `:4317`/`:4318`; Prometheus may reach `:9090`. The Vector API on `:8686` is
  not exposed to pod traffic.

Containers run with a read-only root filesystem, no privilege escalation, all
Linux capabilities dropped, and the `RuntimeDefault` seccomp profile. The
aggregator runs as UID/GID 1000 with an `fsGroup` on its PVC. The agent remains
root because it reads host log paths and writes the hostPath checkpoint
directory, but it still drops all capabilities.

## Metrics

The upstream Vector chart's PodMonitor is enabled for both releases:

- job `vector-agent` (via `jobLabel: app.kubernetes.io/instance`)
- job `vector-aggregator`

Prometheus scrapes `:9090/metrics` on both roles. Useful alerts:

- `vector_component_errors_total > 0`
- unexpected `vector_component_discarded_events_total`
- `vector_buffer_size_events` close to the buffer limit
- `up{job=~"vector-.*"} == 0`

> The upstream PodMonitor uses `jobLabel: app.kubernetes.io/instance`, so the
> job names are `vector-agent` and `vector-aggregator`. This replaces the old
> `vector/vector-agent` / `vector/vector-aggregator` job names; update any
> external alerts that matched the old names.

## E2E

`values-kind.yaml` only overrides the kind StorageClass and enables a PostSync
OTLP smoke probe. The probe sends one log and one trace to the agent Service,
then asserts non-zero OTLP metrics on both the agent and aggregator. The
`vector-agent` e2e app depends on `vector-aggregator` so the full hop is
exercised.

## Notes

- The agent accepts OTLP on `:4317`/`:4318` and forwards logs/traces to the
  aggregator as OTLP. The aggregator remains the direct cluster-wide OTLP
  endpoint as well.
- Kubernetes pod/namespace annotations are suppressed at the source to keep
  log documents small. Pod labels are still available as `kubernetes.pod_labels`
  for future normalization.
- The `vector-kubernetes_logs-v0_2` schema is intentionally managed in
  `quickwit/values.yaml`. The old `v0_1` index is left untouched for its
  existing retention period; a canonical log schema migration is a separate
  change.
- Quickwit auto-creates its OTLP log/trace indexes. They are not declared in
  the Quickwit Helm values and are not managed by this repository.
