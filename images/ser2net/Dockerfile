# syntax=docker/dockerfile:1@sha256:572cebbc2b15e6f84c077742952674889cb63cd5e9b80abdffcbe587b6f19183

# This is intentionally a normal Debian image rather than distroless: it keeps
# the Dockerfile simple and lets the package manager own ser2net and its
# dependencies. The base is pinned by digest; Renovate's docker manager
# updates it.
FROM docker.io/library/debian:13.8-slim@sha256:8aa5a7bf5a1955bddbe7d74fcdd0b91ab30e74362a8b73af672b0481b35b44a0

ARG SER2NET_VERSION=4.6.4-1
# Scheduled CI runs set this to true so the rebuild picks up Debian security
# updates for the base packages and ser2net itself. Normal builds stay pinned.
ARG APT_UPGRADE=false

ENV DEBIAN_FRONTEND=noninteractive

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends "ser2net=${SER2NET_VERSION}"; \
    if [ "${APT_UPGRADE}" = "true" ]; then \
      apt-get upgrade -y; \
    fi; \
    rm -rf /var/lib/apt/lists/*

LABEL org.opencontainers.image.title="ser2net" \
      org.opencontainers.image.description="ser2net serial-to-TCP bridge on Debian slim" \
      org.opencontainers.image.source="https://github.com/mkroman/k8s" \
      org.opencontainers.image.licenses="GPL-2.0-only" \
      org.opencontainers.image.version="${SER2NET_VERSION}"

# The chart supplies supplementalGroups: [20] for the serial device. The
# runtime user is numeric because the image does not create a UID 1000 account.
USER 1000:1000

EXPOSE 6638
STOPSIGNAL SIGTERM

ENTRYPOINT ["/usr/sbin/ser2net", "-d", "-c", "/etc/ser2net/ser2net.yaml"]
