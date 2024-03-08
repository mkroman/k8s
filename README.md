# mk's k8s cluster

This is the GitOps repository for the kubernetes cluster running in my home lab.

## Prerequisites

* Kubernetes
* Router capable of BGP load-balancing (I use an [EdgeRouter 4])
* CNI (I use [cilium])
* Ansible
* Argo CD

[edgerouter 4]: https://www.ui.com/edgemax/edgerouter-4/
[cilium]: https://cilium.io/

The cluster is managed with Ansible and [Kubespray].

[kubespray]: https://kubespray.io/

### Status

| App      | Path                                          | Status |
|----------|-----------------------------------------------|--------|
| cert-manager | [`cert-manager`](cert-manager/)   | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=cert-manager)](https://argo-cd.infra.rwx.im/applications/cert-manager) |
| dendrite | [`kustomize/dendrite`](kustomize/dendrite/)   | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=dendrite)](https://argo-cd.infra.rwx.im/applications/dendrite) |
| longhorn | [`longhorn`](longhorn/)   | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=longhorn)](https://argo-cd.infra.rwx.im/applications/longhorn) |
| sealed-secrets | [`sealed-secrets`](sealed-secrets/)   | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=sealed-secrets)](https://argo-cd.infra.rwx.im/applications/sealed-secrets) |
| traefik  | [`traefik`](traefik/)   | [![App Status](https://argo-cd.infra.rwx.im/api/badge?name=traefik)](https://argo-cd.infra.rwx.im/applications/traefik) |

## Structure

### [`apps/`](apps/)

Argo CD [app of apps][cluster-bootstrapping]
that contains the declarative Application manifests.

[cluster-bootstrapping]: https://argo-cd.readthedocs.io/en/stable/operator-manual/cluster-bootstrapping/

## Applications

### [`sealed-secrets`](sealed-secrets/)

This is the application that deploys bitnami's [sealed secrets] which allows me
to commit encrypted secrets directly to my git repo and have them be unsealed
(decrypted) directly in the cluster.

[sealed secrets]: https://github.com/bitnami-labs/sealed-secrets

### [`cert-manager`](cert-manager/)

This application deploys [cert-manager] for automatic provisioning of
certificates with any ACME providers.

[cert-manager]: https://cert-manager.io/docs/

### [`longhorn`](longhorn/)

This application deploys [longhorn] for dynamic provisioning of distributed
block storage, and it also includes a bunch of neat features like replication
and backup.

[longhorn]: https://longhorn.io/

### [`traefik`](traefik/)

This is an application that deploys [Traefik v2][traefik] as an ingress
controller.

### [`kustomize/dendrite`](kustomize/dendrite/)

This application deploys my [Matrix] instance at [rwx.im](https://rwx.im).

I'm using the second-generation homeserver `Dendrite` instead of the more
supported [`synapse`](https://github.com/matrix-org/synapse) because it's
smaller and much faster.

[matrix]: https://matrix.org

### [`kustomize/rwx`](kustomize/rwx/)

This application deploys resources such as [cert-manager] certificates for
[rwx.im](https://rwx.im).
