#!/usr/bin/env bash
# Starts local Postgres + Redis and creates the dev/test databases.
# Idempotent; safe to run at the start of every session.
set -euo pipefail
service postgresql start >/dev/null 2>&1 || true
service redis-server start >/dev/null 2>&1 || true
for i in $(seq 1 30); do pg_isready -q && break; sleep 0.5; done
su postgres -c "psql -qc \"ALTER USER postgres PASSWORD 'postgres';\"" >/dev/null
# Test databases (and their per-migration-set templates) are created by
# bgh_core::testing on demand.
for db in bgh; do
  su postgres -c "psql -tAc \"SELECT 1 FROM pg_database WHERE datname='$db'\"" | grep -q 1 \
    || su postgres -c "createdb $db"
done
redis-cli ping >/dev/null
echo "postgres + redis ready (DATABASE_URL=postgres://postgres:postgres@localhost/bgh, REDIS_URL=redis://127.0.0.1/)"
