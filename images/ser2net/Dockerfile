# syntax=docker/dockerfile:1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e

# This is intentionally a normal Debian image rather than distroless: it keeps
# the Dockerfile simple and lets the package manager own ser2net and its
# dependencies. The base is pinned by digest; Renovate's docker manager
# updates it.
FROM docker.io/library/debian:13.7-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

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
