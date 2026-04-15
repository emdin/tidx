#!/bin/sh
set -eu

INTERVAL_SECONDS="${BLOCKSCOUT_IMPORT_INTERVAL_SECONDS:-900}"
LOCAL_URL="${BLOCKSCOUT_LOCAL_URL:-http://tidx:8080}"
SOURCE_URL="${BLOCKSCOUT_SOURCE_URL:-https://explorer.igralabs.com}"
CHAIN_ID="${BLOCKSCOUT_CHAIN_ID:-38833}"

echo "Starting Blockscout verification importer"
echo "source=${SOURCE_URL} local=${LOCAL_URL} chain_id=${CHAIN_ID} interval_seconds=${INTERVAL_SECONDS}"

while true; do
  echo "Running Blockscout import at $(date -u +%Y-%m-%dT%H:%M:%SZ)"

  if tidx import-blockscout \
    --local-url "${LOCAL_URL}" \
    --source-url "${SOURCE_URL}" \
    --chain-id "${CHAIN_ID}"; then
    echo "Blockscout import finished successfully"
  else
    echo "Blockscout import failed; retrying after sleep" >&2
  fi

  sleep "${INTERVAL_SECONDS}"
done
