# mk's k8s cluster

GitOps repository for a home-lab Kubernetes cluster managed by Argo CD.

## Structure

### [`argo-cd/overlays/production`](argo-cd/overlays/production/)

Kustomize overlay that deploys Argo CD itself plus a single `Application`
(`apps`) that bootstraps everything else.

### [`apps/`](apps/)

Helm chart that renders `AppProject` and `Application` resources. This is the
[app of apps][cluster-bootstrapping] pattern — all local workload Argo CD
Applications are defined here.

### [`apps-of-apps/`](apps-of-apps/)

Helm chart for Applications sourced from external Git repositories.

[cluster-bootstrapping]: https://argo-cd.readthedocs.io/en/stable/operator-manual/cluster-bootstrapping/

## Applications

| App                         | Path                                            | Status |
|-----------------------------|-------------------------------------------------|--------|
| cert-manager                | [`cert-manager`](cert-manager/)                  | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=cert-manager)](https://argo-cd.infra.rwx.im/applications/cert-manager) |
| chadgpt                     | [`chadgpt`](chadgpt/)                           | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=chadgpt)](https://argo-cd.infra.rwx.im/applications/chadgpt) |
| chadgpt-magistr             | [`chadgpt`](chadgpt/)                           | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=chadgpt-magistr)](https://argo-cd.infra.rwx.im/applications/chadgpt-magistr) |
| cloudnative-pg              | [`cloudnative-pg`](cloudnative-pg/)              | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=cloudnative-pg)](https://argo-cd.infra.rwx.im/applications/cloudnative-pg) |
| grafana                     | [`grafana`](grafana/)                           | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=grafana)](https://argo-cd.infra.rwx.im/applications/grafana) |
| longhorn                    | [`longhorn`](longhorn/)                         | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=longhorn)](https://argo-cd.infra.rwx.im/applications/longhorn) |
| monitoring                  | [`monitoring`](monitoring/)                     | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=monitoring)](https://argo-cd.infra.rwx.im/applications/monitoring) |
| mosquitto                   | [`mosquitto`](mosquitto/)                       | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=mosquitto)](https://argo-cd.infra.rwx.im/applications/mosquitto) |
| mqtt2prometheus             | [`mqtt2prometheus`](mqtt2prometheus/)            | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=mqtt2prometheus)](https://argo-cd.infra.rwx.im/applications/mqtt2prometheus) |
| mqtt2prometheus-mqtt-topic  | [`mqtt2prometheus`](mqtt2prometheus/)            | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=mqtt2prometheus-mqtt-topic)](https://argo-cd.infra.rwx.im/applications/mqtt2prometheus-mqtt-topic) |
| mumble                      | [`mumble`](mumble/)                             | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=mumble)](https://argo-cd.infra.rwx.im/applications/mumble) |
| nats                        | [`nats`](nats/)                                 | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=nats)](https://argo-cd.infra.rwx.im/applications/nats) |
| open-webui                  | [`open-webui`](open-webui/)                      | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=open-webui)](https://argo-cd.infra.rwx.im/applications/open-webui) |
| postgres                    | [`postgres`](postgres/)                         | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=postgres)](https://argo-cd.infra.rwx.im/applications/postgres) |
| prometheus-operator         | [`prometheus-operator/base`](prometheus-operator/base/) | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=prometheus-operator)](https://argo-cd.infra.rwx.im/applications/prometheus-operator) |
| quickwit                    | [`quickwit`](quickwit/)                         | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=quickwit)](https://argo-cd.infra.rwx.im/applications/quickwit) |
| redis                       | [`redis`](redis/)                               | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=redis)](https://argo-cd.infra.rwx.im/applications/redis) |
| sealed-secrets              | [`sealed-secrets`](sealed-secrets/)              | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=sealed-secrets)](https://argo-cd.infra.rwx.im/applications/sealed-secrets) |
| traefik                     | [`traefik`](traefik/)                           | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=traefik)](https://argo-cd.infra.rwx.im/applications/traefik) |
| vector-agent                | [`vector`](vector/)                             | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=vector-agent)](https://argo-cd.infra.rwx.im/applications/vector-agent) |
| vector-aggregator           | [`vector`](vector/)                             | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=vector-aggregator)](https://argo-cd.infra.rwx.im/applications/vector-aggregator) |
| zeta                        | [`zeta`](zeta/)                                 | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=zeta)](https://argo-cd.infra.rwx.im/applications/zeta) |

### External apps

| App                         | Source                                                            | Status |
|-----------------------------|-------------------------------------------------------------------|--------|
| magistr-prod                | [rwx-labs/k8s-rwx-apps](https://github.com/rwx-labs/k8s-rwx-apps) | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=magistr-prod)](https://argo-cd.infra.rwx.im/applications/magistr-prod) |
| meta                        | [mkroman/k8s-meta-apps](https://github.com/mkroman/k8s-meta-apps) | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=meta)](https://argo-cd.infra.rwx.im/applications/meta) |
| meta-oss                    | [mkroman/k8s-meta-apps](https://github.com/mkroman/k8s-meta-apps) | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=meta-oss)](https://argo-cd.infra.rwx.im/applications/meta-oss) |
| meta-webhook                | [mkroman/k8s-meta-apps](https://github.com/mkroman/k8s-meta-apps) | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=meta-webhook)](https://argo-cd.infra.rwx.im/applications/meta-webhook) |

## Prerequisites

- Kubernetes (deployed via [Kubespray])
- [Sealed Secrets](https://github.com/bitnami-labs/sealed-secrets) controller in-cluster
- 1Password CLI (`op`) and `kubeseal` for encrypting secrets

[Kubespray]: https://kubespray.io/
