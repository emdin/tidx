#!/usr/bin/env bash
# Copy the Kaspa provenance tables (kaspa_l2_submissions, kaspa_entries) from
# Postgres, the source of truth, into their ClickHouse copies, then check that
# ClickHouse holds exactly the Postgres rows, all in 0x hex.
#
# Safe to rerun: ReplacingMergeTree keeps one row per key, and the copy ends
# with OPTIMIZE ... FINAL. Run it after `tidx backfill-kaspa*`, which writes
# Postgres only.
#
# --check   only the check: rows older than 30 s must match (newer ones may
#           still be on their way from the engine). Exit 1 on a mismatch.
# --rebuild recreate both tables from db/clickhouse/*.sql. Needed once for the
#           2026-11 change (kaspa_l2_submissions keyed on (l2_tx_hash,
#           kaspa_txid), hex with 0x), and only after the engine that writes 0x
#           hex is running; a plain run refuses old tables. Fills new tables,
#           swaps them in with EXCHANGE TABLES, copies rows promoted during the
#           fill, and keeps the old tables as <table>_before_<UTC timestamp>
#           (roll back with EXCHANGE TABLES; drop when no longer needed). The
#           script never drops a table: an aborted run leaves
#           <table>_new_<timestamp>, which holds the old data if it stopped right
#           after the swap. Check before dropping it.
#
# Usage: deploy/prod/ch-resync-kaspa.sh [--check | --rebuild]
# Env:   PSQL  Postgres client that takes -c   (default: compose postgres, tidx_igra)
#        CH    clickhouse-client in the target database (default: compose clickhouse, tidx_38833)
#        Both are split on spaces, without quoting. Never put a password in
#        them (command lines are audited); use PGPASSWORD / CLICKHOUSE_PASSWORD.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
schema_dir="$here/../../db/clickhouse"
compose=(docker compose -f "$here/docker-compose.yml")
read -r -a psql <<<"${PSQL:-${compose[*]} exec -T postgres psql -U tidx -d tidx_igra}"
read -r -a ch <<<"${CH:-${compose[*]} exec -T clickhouse clickhouse-client -d tidx_38833}"

mode=copy
case "${1:-}" in
  --rebuild) mode=rebuild ;;
  --check) mode=check ;;
  "") ;;
  *) echo "usage: $0 [--check | --rebuild]" >&2; exit 2 ;;
esac

pg() { "${psql[@]}" -v ON_ERROR_STOP=1 -AtX -c "$1" </dev/null; }
chq() { "${ch[@]}" --query "$1" </dev/null; }
log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$*"; }

# Per table: the Postgres SELECT producing the ClickHouse columns in order, and
# a ClickHouse condition true for a row with any hex column lacking 0x.
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
declare -A bare_hex=(
  [kaspa_l2_submissions]="NOT startsWith(l2_tx_hash, '0x') OR NOT startsWith(kaspa_txid, '0x')"
  [kaspa_entries]="NOT startsWith(kaspa_txid, '0x') OR NOT startsWith(recipient, '0x')"
)

# copy <table> <ClickHouse target table> [<Postgres WHERE clause>]
copy() {
  local where=${3:+ WHERE $3}
  # ClickHouse rejects an INSERT without rows.
  [[ $(pg "SELECT count(*) FROM $1$where") == 0 ]] && return 0
  "${psql[@]}" -v ON_ERROR_STOP=1 -AtX -c "COPY (${select_sql[$1]}$where) TO STDOUT (FORMAT csv)" </dev/null \
    | "${ch[@]}" --query "INSERT INTO $2 (${columns[$1]}) FORMAT CSV"
}

# Why <table> can't take a plain copy, if it can't: an old key or bare hex.
needs_rebuild() {
  local want key bare
  want=$(sed -n 's/^ORDER BY (\(.*\))$/\1/p' "$schema_dir/$1.sql")
  key=$(chq "SELECT sorting_key FROM system.tables WHERE database = currentDatabase() AND name = '$1'")
  bare=$(chq "SELECT count() FROM $1 WHERE ${bare_hex[$1]}")
  if [[ $key != "$want" ]]; then echo "sorting key is ($key), not ($want)"
  elif (( bare > 0 )); then echo "$bare rows have hex without 0x"
  fi
}

# Rows older than the cutoff must be in ClickHouse exactly as in Postgres; the
# engine writes ClickHouse just after Postgres, so newer ones may be missing.
check() {
  local cutoff pg_rows ch_rows bare
  cutoff=$(pg "SELECT extract(epoch FROM now())::bigint - 30")
  pg_rows=$(pg "SELECT count(*) FROM $1 WHERE created_at < to_timestamp($cutoff)")
  ch_rows=$(chq "SELECT count() FROM $1 FINAL WHERE created_at < toDateTime64($cutoff, 3, 'UTC')")
  bare=$(chq "SELECT count() FROM $1 WHERE ${bare_hex[$1]}")
  log "$1: rows older than 30 s: postgres $pg_rows, clickhouse $ch_rows; rows with hex without 0x: $bare"
  if (( bare > 0 )); then
    log "$1: FAILED: hex without 0x (an engine older than the 0x change, or old data: use --rebuild)"; return 1
  fi
  if (( ch_rows != pg_rows )); then
    log "$1: FAILED: ClickHouse should hold exactly the Postgres rows"; return 1
  fi
}

ts=$(date -u +%Y%m%dT%H%M%SZ)
status=0
for t in kaspa_l2_submissions kaspa_entries; do
  case $mode in
    rebuild)
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
      chq "OPTIMIZE TABLE $t FINAL"
      ;;
    copy)
      reason=$(needs_rebuild "$t")
      if [[ -n $reason ]]; then
        log "$t: REFUSED: $reason; run with --rebuild"; exit 2
      fi
      log "$t: copying from Postgres"
      copy "$t" "$t"
      chq "OPTIMIZE TABLE $t FINAL"
      ;;
  esac
  check "$t" || status=1
done
(( status == 0 )) && log "done"
exit $status
