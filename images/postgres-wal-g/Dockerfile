FROM alpine:3.15 AS builder

ARG WALG_VERSION=v1.1
ARG WALG_SHA256SUM=c922c46c770fa5edd38bd60bfacdb92d813272d5d6bd2fc83ef57084b9c6c68c

WORKDIR /build

RUN apk add curl

# Download and install wal-g
RUN curl -q -L "https://github.com/wal-g/wal-g/releases/download/${WALG_VERSION}/wal-g-pg-ubuntu-20.04-amd64.tar.gz" -o wal-g-pg.tar.gz \
  && echo "${WALG_SHA256SUM}  wal-g-pg.tar.gz" | sha256sum -c \
  && tar xvf wal-g-pg.tar.gz \
  && pwd

FROM postgres:14.1

COPY --from=builder /build/wal-g-pg-ubuntu-20.04-amd64 /usr/local/bin/wal-g

RUN apt update \
  && apt install -y ca-certificates

ADD postgres_backup.sh /opt/postgres_backup.sh
ADD postgres_archive.sh /opt/postgres_archive.sh

RUN chmod a+x /opt/postgres_archive.sh /opt/postgres_backup.sh
