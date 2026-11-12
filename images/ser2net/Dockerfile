# syntax=docker/dockerfile:1

# Both stages are digest-pinned. Renovate's docker manager updates these.
FROM docker.io/library/debian:13.7-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2 AS builder

ARG SER2NET_VERSION=4.6.4-1

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends "ser2net=${SER2NET_VERSION}"; \
    rm -rf /var/lib/apt/lists/*

# Assemble only the runtime files used by the raw TCP-to-serial path: the
# binary, the net/serialdev/mdns gensio modules, and the shared libraries
# those files load. The SSL and other protocol modules are intentionally
# omitted; Zigbee2MQTT uses a plain tcp:// transport.
# Debian 13 uses usrmerge, so /lib is a symlink to /usr/lib.
RUN set -eux; \
    mkdir -p /rootfs; \
    cp -a --parents /usr/sbin/ser2net /rootfs; \
    gensio_dir="$(dirname "$(find /usr/libexec -type f -name 'libgensio_serialdev.so' | head -n 1)")"; \
    for module in \
      libgensio_net.so \
      libgensio_serialdev.so \
      libgensio_mdns.so; do \
      cp -a --parents "${gensio_dir}/${module}" /rootfs; \
    done; \
    { \
      ldd /usr/sbin/ser2net || true; \
      ldd "${gensio_dir}/libgensio_net.so" || true; \
      ldd "${gensio_dir}/libgensio_serialdev.so" || true; \
      ldd "${gensio_dir}/libgensio_mdns.so" || true; \
    } | awk '/=> \// {print $3}' | sort -u > /tmp/ser2net-libs; \
    while IFS= read -r lib; do \
      [ -n "${lib}" ] || continue; \
      case "${lib}" in \
        /lib/*) dest="/usr/lib/${lib#/lib/}" ;; \
        *) dest="${lib}" ;; \
      esac; \
      mkdir -p "/rootfs$(dirname "${dest}")"; \
      cp -aL "${lib}" "/rootfs${dest}"; \
    done < /tmp/ser2net-libs

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
