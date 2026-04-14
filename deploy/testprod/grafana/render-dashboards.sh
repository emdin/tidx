#!/bin/sh
set -eu

src_dir="${GRAFANA_DASHBOARDS_SRC_DIR:-/etc/grafana/dashboards-src}"
dst_dir="${GRAFANA_DASHBOARDS_DST_DIR:-/var/lib/grafana/dashboards-rendered}"

if [ -z "${EXPLORER_BASE_URL:-}" ]; then
  echo "EXPLORER_BASE_URL is required" >&2
  exit 1
fi

mkdir -p "$dst_dir"
rm -f "$dst_dir"/*.json

for src in "$src_dir"/*.json; do
  dst="$dst_dir/$(basename "$src")"
  sed "s|__EXPLORER_BASE_URL__|${EXPLORER_BASE_URL}|g" "$src" > "$dst"
done
