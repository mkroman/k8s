SHELL := /bin/bash
MANIFESTS_DIR := ./templates
OP2SECRET ?= op2secret
KUBESEAL ?= kubeseal
SECRETS := mqtt2prometheus/mqtt2prometheus-auth

.PHONY: all
all: secrets

.PHONY: clean
clean:
	@for secret in $(SECRETS); do \
		rm -v "$(MANIFESTS_DIR)/sealedsecret.$${secret#*/}.yaml" \
	; done

.PHONY: secrets
secrets:
	@echo "Reading secrets using op2secret (\`$(OP2SECRET)') and encrypting using kubeseal (\`$(KUBESEAL)')"
	@for secret in $(SECRETS); do \
		$(OP2SECRET) $${secret} | kubeseal -o yaml > "$(MANIFESTS_DIR)/sealedsecret.$${secret#*/}.yaml" \
	; done
