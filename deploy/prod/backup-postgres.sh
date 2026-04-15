#!/usr/bin/env bash
set -euo pipefail

POSTGRES_USER="${POSTGRES_USER:-tidx}"
POSTGRES_DB="${POSTGRES_DB:-tidx_igra}"
BACKUP_DIR="${BACKUP_DIR:-./backups}"

mkdir -p "$BACKUP_DIR"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
out="${BACKUP_DIR}/${POSTGRES_DB}_${stamp}.dump"

printf 'Creating PostgreSQL custom-format dump: %s\n' "$out"
docker compose exec -T postgres \
  pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc > "$out"

printf 'Verifying dump header\n'
docker compose exec -T postgres \
  pg_restore --list < "$out" >/dev/null

printf 'OK: backup written to %s\n' "$out"
