OP2SECRET ?= op2secret
NAMESPACE = grafana

.PHONY: secrets
secrets: templates/sealedsecret.grafana-auth.yaml

.PHONY: templates/sealedsecret.grafana-auth.yaml
templates/sealedsecret.grafana-auth.yaml:
	$(OP2SECRET) "${NAMESPACE}/$<" \
		| kubeseal -o yaml > $@
