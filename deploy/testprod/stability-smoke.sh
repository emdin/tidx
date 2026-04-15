#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${BASE_URL:-https://stage-roman.igralabs.com}"
CHAIN_ID="${CHAIN_ID:-38833}"
EXPECTED_HEAD_DELAY_BLOCKS="${EXPECTED_HEAD_DELAY_BLOCKS:-10}"
MAX_EFFECTIVE_LAG_BLOCKS="${MAX_EFFECTIVE_LAG_BLOCKS:-10}"
MAX_GAP_BLOCKS="${MAX_GAP_BLOCKS:-25}"
POSTGRES_USER="${POSTGRES_USER:-tidx}"
POSTGRES_DB="${POSTGRES_DB:-tidx_igra}"
CLICKHOUSE_DB="${CLICKHOUSE_DB:-tidx_38833}"
KNOWN_WITHDRAWAL_BLOCK="${KNOWN_WITHDRAWAL_BLOCK:-4046564}"

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

need_cmd curl
need_cmd jq
need_cmd docker

printf 'Checking API health at %s\n' "$BASE_URL"
health="$(curl -fsS "$BASE_URL/health")"
test "$health" = "OK" || fail "health endpoint returned: $health"

status_json="$(curl -fsS "$BASE_URL/status")"
chain_json="$(printf '%s' "$status_json" | jq -c --argjson chain_id "$CHAIN_ID" '.chains[] | select(.chain_id == $chain_id)' | head -n 1)"
test -n "$chain_json" || fail "chain_id $CHAIN_ID missing from /status"

head_num="$(printf '%s' "$chain_json" | jq -r '.head_num')"
tip_num="$(printf '%s' "$chain_json" | jq -r '.tip_num')"
lag="$(printf '%s' "$chain_json" | jq -r '.lag')"
gap_blocks="$(printf '%s' "$chain_json" | jq -r '.gap_blocks')"
effective_lag="$(( lag > EXPECTED_HEAD_DELAY_BLOCKS ? lag - EXPECTED_HEAD_DELAY_BLOCKS : 0 ))"

printf 'Status: head=%s tip=%s lag=%s expected_delay=%s effective_lag=%s gaps=%s\n' \
  "$head_num" "$tip_num" "$lag" "$EXPECTED_HEAD_DELAY_BLOCKS" "$effective_lag" "$gap_blocks"

test "$effective_lag" -le "$MAX_EFFECTIVE_LAG_BLOCKS" || \
  fail "effective lag $effective_lag exceeds max $MAX_EFFECTIVE_LAG_BLOCKS"

test "$gap_blocks" -le "$MAX_GAP_BLOCKS" || \
  fail "gap blocks $gap_blocks exceeds max $MAX_GAP_BLOCKS"

printf 'Checking local Prometheus and Grafana readiness\n'
curl -fsS "http://127.0.0.1:9091/-/ready" >/dev/null || fail "Prometheus is not ready"
curl -fsS "http://127.0.0.1:3000/api/health" >/dev/null || fail "Grafana is not ready"

printf 'Checking Docker services\n'
docker compose ps --format json >/dev/null || fail "docker compose ps failed"

printf 'Checking PostgreSQL withdrawal row\n'
pg_withdrawals="$(
  docker compose exec -T postgres \
    psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc \
    "SELECT count(*) FROM l2_withdrawals WHERE block_num = ${KNOWN_WITHDRAWAL_BLOCK};"
)"
test "$pg_withdrawals" -ge 1 || fail "missing known withdrawal block $KNOWN_WITHDRAWAL_BLOCK in PostgreSQL"

printf 'Checking ClickHouse withdrawal mirror\n'
ch_table_exists="$(
  docker compose exec -T clickhouse \
    clickhouse-client --query "EXISTS TABLE ${CLICKHOUSE_DB}.l2_withdrawals"
)"
test "$ch_table_exists" = "1" || fail "ClickHouse l2_withdrawals table is missing"

ch_withdrawals="$(
  docker compose exec -T clickhouse \
    clickhouse-client --query "SELECT count() FROM ${CLICKHOUSE_DB}.l2_withdrawals WHERE block_num = ${KNOWN_WITHDRAWAL_BLOCK}"
)"
test "$ch_withdrawals" -ge 1 || fail "missing known withdrawal block $KNOWN_WITHDRAWAL_BLOCK in ClickHouse"

printf 'Checking public query API for withdrawal row\n'
api_rows="$(
  curl -fsS "${BASE_URL}/query?chainId=${CHAIN_ID}&sql=SELECT%20count(*)%20AS%20rows%20FROM%20l2_withdrawals%20WHERE%20block_num%20%3D%20${KNOWN_WITHDRAWAL_BLOCK}" \
    | jq -r '.rows[0][0]'
)"
test "$api_rows" -ge 1 || fail "public query API did not return known withdrawal row"

printf 'OK: stability smoke passed\n'
