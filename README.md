# mk's k8s cluster

This is the GitOps repository for the kubernetes cluster running in my home lab.

## Prerequisites

* Kubernetes
* Router capable of BGP load-balancing (I use [EdgeRouter 4])
* CNI (I use [flannel])
* Argo CD

[edgerouter 4]: https://www.ui.com/edgemax/edgerouter-4/
[flannel]: https://github.com/flannel-io/flannel

## Structure

### [`apps/`](apps/)

Argo CD [app of apps][cluster-bootstrapping]
that contains the declarative Application manifests.

[cluster-bootstrapping]: https://argo-cd.readthedocs.io/en/stable/operator-manual/cluster-bootstrapping/

### [`helm/`](helm/)

This is the folder for Helm charts and are usually just meta-charts for entire
applications.

### [`kustomize/`](kustomize/)

This is the folder where declarative [Kustomization] manifests reside for
applications.

[kustomization]: https://kubernetes.io/docs/tasks/manage-kubernetes-objects/kustomization/

## Applications

### [`helm/bitnami-sealed-secrets`](helm/bitnami-sealed-secrets/)

This is the application that deploys bitnami's [sealed secrets] which allows me
to commit encrypted secrets directly to my git repo and have them be unsealed
(decrypted) directly in the cluster.

[sealed secrets]: https://github.com/bitnami-labs/sealed-secrets

### [`helm/cert-manager`](helm/cert-manager/)

This application deploys [cert-manager] for automatic provisioning of
certificates with any ACME providers.

[cert-manager]: https://cert-manager.io/docs/

### [`helm/longhorn`](helm/longhorn/)

This application deploys [longhorn] for dynamic provisioning of distributed
block storage, and it also includes a bunch of neat features like replication
and backup.

[longhorn]: https://longhorn.io/

### [`helm/traefik`](helm/traefik/)

This is an application that deploys [Traefik v2][traefik] as an ingress
controller across my whole cluster.

### [`kustomize/argo-events`](kustomize/argo-events/)

This application deploys [Argo Events][argo-events] which is a very extensible
and modular way of doing workflow automations with various event sources.

[argo-events]: https://argoproj.github.io/argo-events/

### [`kustomize/dendrite`](kustomize/dendrite/)

This application deploys my [Matrix] instance at [rwx.im](https://rwx.im).

I'm using the second-generation homeserver `Dendrite` instead of the more
supported [`synapse`](https://github.com/matrix-org/synapse) because it's
smaller and much faster.

[matrix]: https://matrix.org

### [`kustomize/meta`](kustomize/meta/)

This application deploys my IRC bot called `meta` which is based on my IRC
framework, [Blur].

[blur]: https://github.com/mkroman/blur

### [`kustomize/meta-webhook`](kustomize/meta-webhook/)

This application deploys [meta-webhook] which is a simple HTTP endpoint where
someone with a valid bearer token can issue RPC messages directly to `meta`.

The underlying RPC is done using [meta-rpc_client].

[meta-webhook]: https://github.com/mkroman/meta-webhook
[meta-rpc_client]: https://github.com/mkroman/meta-rpc_client

### [`kustomize/metallb`](kustomize/metallb/)

This application deploys MetalLB and configures it to use BGP routing.

This has the advantage that it's much easier to set up an ingress controller and
retain the client source IP which was a huge problem when I was still using
[`k3s`][k3s] [<sup>1</sup>][k3s-issue-1]

It's configured to use the IP range `10.0.1.128/25` which gives me 126 IP
addresses for `LoadBalancer` type services.

[k3s]: https://k3s.io
[k3s-issue-1]: https://github.com/k3s-io/k3s/discussions/2997

### [`kustomize/postgres-operator`](kustomize/postgres-operator/)

This application deploys [postgres-operator] as well as the [postgres-operator
ui].

This takes Postgres into the era of cloud native databases. It makes it trivial
to deploy clusters, databases, add replication, load-balancing, pooling, etc.

[postgres-operator]: https://github.com/zalando/postgres-operator
[postgres-operator ui]: https://github.com/zalando/postgres-operator/blob/master/docs/operator-ui.md

### [`kustomize/rwx`](kustomize/rwx/)

This application deploys resources such as [cert-manager] certificates for
[rwx.im](https://rwx.im).

### [`kustomize/whoami`](kustomize/whoami/)

This application deploys a basic web application that is useful for debugging
ingress issues.

For an example, you can access it [here](https://whoami.infra.rwx.im/)
