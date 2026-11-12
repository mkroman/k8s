# syntax=docker/dockerfile:1

# Both stages are digest-pinned. Renovate's docker manager updates these.
FROM docker.io/library/debian:13.7-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2 AS builder

ARG SER2NET_VERSION=4.6.4-1

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends "ser2net=${SER2NET_VERSION}"; \
    rm -rf /var/lib/apt/lists/*

# Keep this simple: copy the binary, the full multiarch library directory from
# the same Debian release, and all gensio modules. The extra size is preferred
# over a hand-maintained dependency closure that can miss a dlopen()ed library.
# Debian 13 uses usrmerge, so /lib is a symlink to /usr/lib.
RUN set -eux; \
    mkdir -p /rootfs/usr/sbin /rootfs/usr/lib /rootfs/usr/share; \
    cp -a /usr/sbin/ser2net /rootfs/usr/sbin/; \
    cp -a /usr/lib/*-linux-gnu /rootfs/usr/lib/; \
    cp -a /usr/libexec /rootfs/usr/; \
    if [ -d /usr/share/ser2net ]; then \
      cp -a /usr/share/ser2net /rootfs/usr/share/; \
    fi

FROM gcr.io/distroless/base-nossl-debian13:nonroot@sha256:e6b0f82595aa1edde72c8054de7a9dca30f0f8f54820c37a167c0ddfc21c5c67

ARG SER2NET_VERSION=4.6.4-1

LABEL org.opencontainers.image.title="ser2net" \
      org.opencontainers.image.description="ser2net serial-to-TCP bridge on a distroless base" \
      org.opencontainers.image.source="https://github.com/mkroman/k8s" \
      org.opencontainers.image.licenses="GPL-2.0-only" \
      org.opencontainers.image.version="${SER2NET_VERSION}" \
      org.opencontainers.image.base.name="gcr.io/distroless/base-nossl-debian13:nonroot"

COPY --from=builder /rootfs/ /

# The chart supplies supplementalGroups: [20] for the serial device. The
# runtime user is numeric because distroless has no entry for UID 1000.
USER 1000:1000

EXPOSE 6638
STOPSIGNAL SIGTERM

ENTRYPOINT ["/usr/sbin/ser2net", "-d", "-c", "/etc/ser2net/ser2net.yaml"]
