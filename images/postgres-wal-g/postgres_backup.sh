#!/usr/bin/env sh

# Copyright (C) 2022, Mikkel Kroman <mk@maero.dk>
# All rights reserved.

export PGUSER=postgres
export PGPASSWORD="${POSTGRES_PASSWORD}"

wal-g backup-push "${PGDATA}"
