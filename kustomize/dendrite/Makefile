OP2SECRET ?= op2secret
NAMESPACE = rwx
MANIFESTS = base/sealedsecret.postgres-backup.yaml base/sealedsecret.postgres-dendrite-auth.yaml base/sealedsecret.postgres-superuser-auth.yaml base/sealedsecret.dendrite-config.yaml

.PHONY: secrets
secrets: $(MANIFESTS)

clean:
	rm $(MANIFESTS)

base/sealedsecret.postgres-backup.yaml:
	$(OP2SECRET) -n $(NAMESPACE) rwx/postgres-backup \
		| kubeseal -o yaml > $@

base/sealedsecret.postgres-dendrite-auth.yaml:
	$(OP2SECRET) -n $(NAMESPACE) -t "kubernetes.io/basic-auth" rwx/postgres-dendrite-auth \
		| kubeseal -o yaml > $@

base/sealedsecret.postgres-superuser-auth.yaml:
	$(OP2SECRET) -n $(NAMESPACE) -t "kubernetes.io/basic-auth" rwx/postgres-superuser-auth \
		| kubeseal -o yaml > $@

base/sealedsecret.dendrite-config.yaml:
	$(OP2SECRET) -n $(NAMESPACE) rwx/dendrite-config \
		| kubeseal -o yaml > $@

base/sealedsecret.dendrite-monitoring-auth.yaml:
	$(OP2SECRET) -n $(NAMESPACE) -t "kubernetes.io/basic-auth" rwx/dendrite-monitoring-auth \
		| kubeseal -o yaml > $@
