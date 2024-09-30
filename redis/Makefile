OP2SECRET ?= op2secret
NAMESPACE = rwx-redis-prod

.PHONY: secrets
secrets: templates/sealedsecret.redis-auth.yaml

.PHONY: templates/sealedsecret.redis-auth.yaml
templates/sealedsecret.redis-auth.yaml:
	$(OP2SECRET) "${NAMESPACE}/redis-auth" \
		| kubeseal -o yaml > $@
