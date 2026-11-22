#!/usr/bin/env bash
# Copy the Kaspa provenance tables (kaspa_l2_submissions, kaspa_entries) from
# Postgres, the source of truth, into their ClickHouse copies.
#
# Safe to rerun: ReplacingMergeTree keeps one row per key, and the run ends with
# OPTIMIZE ... FINAL so the copies are merged before it reports counts.
#
# Run it after `tidx backfill-kaspa*`, which writes Postgres only.
#
# --rebuild recreates both tables from db/clickhouse/*.sql. Needed once for the
# 2026-11 change (kaspa_l2_submissions keyed on (l2_tx_hash, kaspa_txid), hex
# with 0x), and only after the engine that writes 0x hex is running. It fills
# new tables, swaps them in with EXCHANGE TABLES, copies rows promoted during the
# fill, and keeps the old tables as <table>_before_<UTC timestamp> for rollback
# (swap back with EXCHANGE TABLES; drop when no longer needed). The script never
# drops a table: an aborted run leaves <table>_new_<timestamp>, which holds the
# old data if the run stopped right after the swap. Check before dropping it.
#
# Usage: deploy/prod/ch-resync-kaspa.sh [--rebuild]
# Env:   PSQL  Postgres client reading SQL from -c   (default: compose postgres, tidx_igra)
#        CH    clickhouse-client in the target database (default: compose clickhouse, tidx_38833)
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
schema_dir="$here/../../db/clickhouse"
compose=(docker compose -f "$here/docker-compose.yml")
read -r -a psql <<<"${PSQL:-${compose[*]} exec -T postgres psql -U tidx -d tidx_igra}"
read -r -a ch <<<"${CH:-${compose[*]} exec -T clickhouse clickhouse-client -d tidx_38833}"

rebuild=false
case "${1:-}" in
  --rebuild) rebuild=true ;;
  "") ;;
  *) echo "usage: $0 [--rebuild]" >&2; exit 2 ;;
esac

pg() { "${psql[@]}" -v ON_ERROR_STOP=1 -AtX -c "$1"; }
chq() { "${ch[@]}" --query "$1"; }
log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$*"; }

# Each table: Postgres SELECT producing the ClickHouse columns, in order.
declare -A select_sql=(
  [kaspa_l2_submissions]="SELECT '0x' || encode(l2_tx_hash, 'hex'), '0x' || encode(kaspa_txid, 'hex'),
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS.MS') FROM kaspa_l2_submissions"
  [kaspa_entries]="SELECT '0x' || encode(kaspa_txid, 'hex'), '0x' || encode(recipient, 'hex'), amount_sompi,
       to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS.MS') FROM kaspa_entries"
)
declare -A columns=(
  [kaspa_l2_submissions]="l2_tx_hash, kaspa_txid, created_at"
  [kaspa_entries]="kaspa_txid, recipient, amount_sompi, created_at"
)
declare -A hex_column=([kaspa_l2_submissions]=l2_tx_hash [kaspa_entries]=kaspa_txid)

# copy <table> <ClickHouse target table> [<Postgres WHERE clause>]
copy() {
  local where=${3:+ WHERE $3}
  # ClickHouse rejects an INSERT without rows.
  [[ $(pg "SELECT count(*) FROM (${select_sql[$1]}$where) s") == 0 ]] && return 0
  "${psql[@]}" -v ON_ERROR_STOP=1 -AtX -c "COPY (${select_sql[$1]}$where) TO STDOUT (FORMAT csv)" \
    | "${ch[@]}" --query "INSERT INTO $2 (${columns[$1]}) FORMAT CSV"
}

ts=$(date -u +%Y%m%dT%H%M%SZ)
for t in kaspa_l2_submissions kaspa_entries; do
  if $rebuild; then
    new="${t}_new_$ts"
    log "$t: building $new"
    chq "$(sed "s/CREATE TABLE IF NOT EXISTS $t\b/CREATE TABLE $new/" "$schema_dir/$t.sql")"
    since=$(pg "SELECT extract(epoch FROM now())::bigint")
    copy "$t" "$new"
    chq "EXCHANGE TABLES $t AND $new"
    chq "RENAME TABLE $new TO ${t}_before_$ts"
    log "$t: swapped in (old data: ${t}_before_$ts); copying rows promoted during the fill"
    # 10 min covers a promotion transaction that started before the fill.
    copy "$t" "$t" "created_at >= to_timestamp($since - 600)"
  else
    log "$t: copying from Postgres"
    copy "$t" "$t"
  fi
  chq "OPTIMIZE TABLE $t FINAL"

  pg_before=$(pg "SELECT count(*) FROM $t")
  ch_rows=$(chq "SELECT count() FROM $t FINAL")
  pg_after=$(pg "SELECT count(*) FROM $t")
  bare=$(chq "SELECT count() FROM $t WHERE NOT startsWith(${hex_column[$t]}, '0x')")
  log "$t: postgres $pg_before..$pg_after, clickhouse $ch_rows, without 0x: $bare"
  if (( bare > 0 )); then
    log "$t: $bare rows without 0x: an engine older than the 0x change wrote them; rerun once it is gone"; exit 1
  fi
  if (( ch_rows < pg_before || ch_rows > pg_after )); then
    log "$t: MISMATCH (ClickHouse should hold every Postgres row and no others)"; exit 1
  fi
done
log "done"
