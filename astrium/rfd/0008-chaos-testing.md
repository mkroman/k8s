---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: ideation
discussion: "https://github.com/mkroman/k8s/pull/430"
labels: [software, testing]
---

# RFD 0008: Chaos testing for stateful upgrades

## Summary

Add a repeatable chaos suite for the stateful upgrade path, starting with the
`stalwart` chart. The suite should measure service downtime during upgrades,
verify data and PVC survival, and exercise failure recovery rather than only
the happy path.

This is a plan. Nothing in this RFD is implemented yet.

## Motivation

Manual chaos testing of the `stalwart` chart found:

- A normal config or chart-managed Secret upgrade replaces the single pod and
  causes roughly an 8–10 second SMTP gap.
- A forced pod delete causes a transient RocksDB `LOCK` failure and a longer
  outage, but recovers without data loss.
- A failed image upgrade leaves the pod in `ImagePullBackOff`. `helm rollback`
  and `--atomic` restore the StatefulSet revision but do not replace the stuck
  pod; a manual pod delete is required.
- Rotating an external recovery-admin Secret does not restart the pod, so the
  running process keeps the old value.

The existing `astrarium upgrade` test covers install, a single upgrade, pod
replacement, a data marker, and PVC retention. It does not cover timing,
failure recovery, or Secret rotation.

## Scenarios

| Scenario | What it proves |
| --- | --- |
| Same-values `helm upgrade` | The no-op path does not restart the pod. |
| ConfigMap checksum change | A config change rolls the pod and applies the new file. |
| Chart-managed Secret change | The checksum annotation rolls the pod and updates the environment. |
| External Secret rotation | The chart cannot checksum the Secret; the test must verify that a restart is required and that the new value is applied afterward. |
| Graceful pod delete | Termination and startup preserve data and recover within the expected budget. |
| Forced pod delete | RocksDB crash recovery succeeds, with a documented transient LOCK failure. |
| Pod delete during a rollout | The StatefulSet controller converges and does not lose data. |
| Failed image upgrade + rollback | Rollback restores the revision; the test records whether the stuck pod must be deleted. |

## Harness design

### Manifest

Prefer metadata under the existing app entry instead of a separate script:

```toml
[apps.upgrade]
enabled = true
container = "stalwart"
data_path = "/var/lib/stalwart"

[apps.upgrade.chaos]
enabled = true
smtp_port = 25
admin_port = 8080
normal_outage_budget_seconds = 15
forced_outage_budget_seconds = 45
```

The native manifest and the catalog adapter must expose the same fields.
`astrarium upgrade --chaos` and an opt-in `astrarium chaos` command can share
one implementation; the suite must not add a per-app shell script.

### Probe

Run one in-cluster client pod with a small static image. It repeatedly:

- opens a TCP connection to the mail Service on the SMTP port;
- checks the admin `/healthz/ready` endpoint at a lower rate to avoid the
  server's rate limit;
- writes a monotonic timestamp and both results to stdout.

A host-side parser computes outage intervals from the pod logs. The probe must
not be the application pod, because the application pod disappears during the
scenario.

### Data assertions

- Write a marker below `upgrade.data_path` before each scenario and read it
  afterward.
- Record the PVC name, UID, and bind phase.
- Optionally create a JMAP mailbox before the final rollouts and query it
  afterward. The current manual test used this as an application-level write.
- After a forced kill, assert that the container restarted and eventually
  became Ready; do not treat the first LOCK error as data corruption.

### Failure injection

- `kubectl`-free failure injection through the Kubernetes API: delete pods,
  patch the release values, and run `helm`/Astrarium upgrade operations.
- The failed-image scenario needs a deliberately missing tag. The test must
  restore the previous revision and verify the documented recovery path.
- Concurrency: start an upgrade and delete the pod after a short delay. Assert
  convergence, not a specific intermediate state.

## CI

- Do not run chaos on every pull request. It is slower and intentionally
  disruptive.
- Run it on a schedule, on a manual `workflow_dispatch`, and on a label such
  as `e2e/chaos`.
- Use a dedicated matrix entry or a separate workflow so the normal e2e matrix
  stays bounded.
- Upload the upgrade trace and the probe log with the usual diagnostics.
- A chaos failure must fail the job; a documented recovery that requires an
  extra pod delete is a pass only if the test performs that recovery and proves
  the result.

## Acceptance criteria

- The probe produces a machine-readable outage interval for each scenario.
- Normal config and Secret upgrades stay within the configured budget and
  preserve the marker and PVC.
- Same-values upgrade produces no outage.
- Forced deletion recovers with data intact; the transient LOCK failure is
  recorded, not hidden.
- Failed-image rollback either recovers automatically or the test executes the
  documented pod-delete recovery and verifies it.
- External Secret rotation is either automated or explicitly requires and
  verifies a restart.
- The suite is documented in `stalwart/README.md` and can be run locally
  against a disposable kind cluster.

## Non-goals

- Multi-replica or shared-store HA. The chart currently refuses more than one
  replica.
- Node failure, storage failure, or network partition testing beyond pod
  deletion.
- Production traffic generation or load testing.

## Open questions

- Should the probe be a chart-owned Job, a generic Astrarium fixture, or a
  separate test image?
- Is a JMAP mailbox a strong enough application-level assertion, or should the
  test use Stalwart's export/import path?
- Should the failed-image scenario be part of the default chaos set, or a
  destructive opt-in?
- Should the normal outage budget be a manifest value or a chart annotation?
