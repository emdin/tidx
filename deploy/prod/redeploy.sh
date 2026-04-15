#!/usr/bin/env bash
set -euo pipefail

require_clean="${REQUIRE_CLEAN_GIT:-1}"

if [ "$require_clean" = "1" ] && command -v git >/dev/null 2>&1; then
  if ! git diff --quiet || ! git diff --cached --quiet; then
    printf 'Refusing deploy: git worktree has local changes. Set REQUIRE_CLEAN_GIT=0 to override.\n' >&2
    exit 1
  fi
fi

printf 'Backing up PostgreSQL before redeploy\n'
./backup-postgres.sh

printf 'Pulling release images\n'
docker compose pull

printf 'Restarting application services\n'
docker compose up -d tidx blockscout-importer grafana prometheus caddy

printf 'Running smoke checks\n'
./stability-smoke.sh

printf 'OK: redeploy complete\n'
