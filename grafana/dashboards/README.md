# Grafana dashboards

These dashboards are provisioned in production by the Grafana chart's dashboard
sidecar. The sidecar watches ConfigMaps labelled `grafana.com/dashboard: "true"`
(see `sidecar.dashboards` in `../values.yaml`). The files in this directory are
packaged into that ConfigMap by `../templates/configmap-dashboards.yaml`.

The kind values disable the dashboard sidecar, so these dashboards are
production-only for now. In production the sidecar provider is pinned to
Grafana org 2 (`grafana.sidecar.dashboards.provider.orgid`), matching
`users.auto_assign_org_id`.

## Datasource convention

Dashboard panels use the dashboard variable `datasource`:

```json
{ "type": "prometheus", "uid": "$datasource" }
```

The variable is a Prometheus datasource variable with `current` set to
`default`, so it resolves to the default Prometheus datasource in each
environment. The variable name matches the active ArgoCD, Quickwit, and Thanos
dashboards; the `default` value matches the Thanos dashboards. No datasource
UID is embedded in the repository.

## Updating a dashboard

1. Edit the JSON file in this directory.
2. Run `./hack/ci/validate-dashboards.py` from the repository root.
3. Open a pull request and let Argo/Helm sync the change.
4. Verify the dashboard UID and panels in Grafana after sync.

CI runs the validator when files in this directory or the validator change.
Do not re-import these dashboards through the Prometheus plugin's "Import
dashboards" action; that would re-create the plugin-managed copies and may
restore the old `Prometheus` datasource reference.

## Notes

- `grafana-metrics.json` is the "Grafana metrics" dashboard (`isFoa0z7k`).
- `prometheus-2.0-stats.json` is the "Prometheus 2.0 Stats" dashboard
  (`UDdpyzz7z`).
- `id` and `version` are intentionally omitted from the JSON files so the
  dashboard is identified by its stable UID.
