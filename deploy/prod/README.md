# Igra Production Deployment Bundle

This directory deploys the production Igra explorer/indexer stack on one host.

It includes:

- `caddy` for public HTTPS termination
- `tidx` / `iidx` for L2 indexing, API, and explorer UI
- `blockscout-importer` for verified contract source/ABI imports
- `postgres` for primary indexed chain data
- `clickhouse` for analytics queries and Grafana dashboards
- `prometheus` for metrics
- `grafana` for dashboards
- `kaspa-wrpc-proxy` for Docker-to-host access to local Kaspa Borsh wRPC

Production defaults are intentionally conservative:

- PostgreSQL and ClickHouse are private Docker services.
- Prometheus and Grafana are bound to localhost; Grafana is also routed by Caddy at `/grafana`.
- Explorer admin writes are allowed only from `trusted_cidrs` in `config.toml`.
- Kaspa provenance is shown as pending first, then promoted after the configured finality delay.
- Database volumes are persistent and must never be deleted during redeploy.

## Host Requirements

Recommended first production host:

- Ubuntu 24.04 LTS
- Docker Engine and Docker Compose plugin
- 16 vCPU minimum, 32 vCPU preferred
- 64 GB RAM minimum
- 1 TB NVMe minimum, larger if ClickHouse retention grows
- Static public IPv4
- Fully synced local Kaspa node with Borsh wRPC enabled on `127.0.0.1:17110`

Open only:

- `80/tcp`
- `443/tcp`
- restricted `22/tcp`

Do not expose:

- `5432` PostgreSQL
- `8123` / `9000` ClickHouse
- `8080` tidx
- `9090` / `9091` Prometheus
- `3000` Grafana
- `17110` Kaspa wRPC

## DNS

Create:

```text
A explorer.igralabs.com -> <prod server public IP>
TTL 300
```

If using Cloudflare, start as `DNS only` until Caddy successfully obtains TLS certificates.

## Release Image

Production should use immutable image tags.

The production deployment branch is currently:

```text
igra
```

Devops may build from the latest commit on `origin/igra`, but the resolved commit must be captured and used in the image tag. Do not deploy a floating branch name as the runtime image tag.

Example:

```text
TIDX_IMAGE=ghcr.io/igralabs/iidx:igra-ac8e120
```

Avoid mutable tags like `latest` for prod rollback safety.

`TIDX_IMAGE` is the Docker image used by both production application services:

- `tidx`, which runs the indexer, API, and explorer UI
- `blockscout-importer`, which reuses the same binary image to import verified contracts

The placeholder in `.env.example` is intentionally not deployable:

```env
TIDX_IMAGE=ghcr.io/igralabs/iidx:replace-with-release-tag
```

Before production deploy, replace it with a real release tag built from a known git commit:

```env
TIDX_IMAGE=ghcr.io/igralabs/iidx:igra-aa3fdcc
```

Recommended CI release pattern:

```bash
git fetch origin
git checkout igra
git pull --ff-only origin igra

git_sha="$(git rev-parse --short=7 HEAD)"
image="ghcr.io/igralabs/iidx:igra-${git_sha}"

docker build -t "$image" .
docker push "$image"
```

Manual equivalent from a fresh checkout:

```bash
git clone https://github.com/reshmem/tidx.git
cd tidx
git checkout igra
git pull --ff-only origin igra

git_sha="$(git rev-parse --short=7 HEAD)"
image="ghcr.io/igralabs/iidx:igra-${git_sha}"

docker build -t "$image" .
docker push "$image"
printf 'Use this in deploy/prod/.env: TIDX_IMAGE=%s\n' "$image"
```

If the prod host pulls from private GHCR, authenticate once on the host:

```bash
echo "$GHCR_TOKEN" | docker login ghcr.io -u <github-user-or-bot> --password-stdin
```

Then set `.env` on the prod host:

```env
TIDX_IMAGE=ghcr.io/igralabs/iidx:igra-aa3fdcc
```

Deploy exactly that image:

```bash
docker compose pull
docker compose up -d tidx blockscout-importer
./stability-smoke.sh
```

Rollback is the same mechanism with the previous known-good tag:

```env
TIDX_IMAGE=ghcr.io/igralabs/iidx:igra-ac8e120
```

```bash
docker compose pull
docker compose up -d tidx blockscout-importer
./stability-smoke.sh
```

## First Deploy

Clone and configure:

```bash
git clone https://github.com/reshmem/tidx.git
cd tidx
git checkout igra
cd deploy/prod
cp .env.example .env
```

Edit `.env`:

```bash
nano .env
```

Required values:

```env
COMPOSE_PROJECT_NAME=iidx-prod
TIDX_IMAGE=ghcr.io/igralabs/iidx:<release-tag>

EXPLORER_DOMAIN=explorer.igralabs.com
EXPLORER_BASE_URL=https://explorer.igralabs.com/explore
LETSENCRYPT_EMAIL=ops@igralabs.com

POSTGRES_USER=tidx
POSTGRES_PASSWORD=<strong-postgres-password>
POSTGRES_DB=tidx_igra

GRAFANA_ADMIN_PASSWORD=<strong-grafana-password>

KASPA_WRPC_BORSH_PORT=17110
KASPA_PROXY_PORT=17111
KASPA_PROXY_BIND_HOST=172.17.0.1
```

Review `config.toml`:

```toml
rpc_url = "https://rpc.igralabs.com:8545"
chain_id = 38833
head_delay_blocks = 30
max_head_delay_blocks = 100
trust_rpc = false

[chains.kaspa]
enabled = true
rpc_url = "ws://host.docker.internal:17111"
txid_prefix = "97b1"
initial_tip_distance = 100
promotion_delay_secs = 43200
```

Start all services:

```bash
docker compose pull
docker compose up -d
```

Watch first boot:

```bash
docker compose logs -f tidx
docker compose logs -f blockscout-importer
docker compose logs -f caddy
```

## What Happens On First Boot

1. `postgres` starts and creates the `tidx_igra` database from `.env`.
2. `clickhouse` starts with persistent storage.
3. `tidx` applies DB schema, starts HTTP API, starts L2 sync, starts ClickHouse mirroring, and starts Kaspa provenance sync.
4. L2 realtime sync follows `head - adaptive_delay`; default minimum delay is `30` blocks.
5. Gap sync fills missing L2 history, newest gaps first.
6. `blockscout-importer` imports verified contract source/ABI from `https://explorer.igralabs.com`.
7. Kaspa provenance rows first appear in pending tables, then promote to final tables after `promotion_delay_secs`.
8. Grafana dashboards are rendered with `EXPLORER_BASE_URL`.
9. Caddy obtains/renews TLS for `EXPLORER_DOMAIN`.

## Persistent Volumes

This stack uses Docker named volumes:

```text
iidx-prod_postgres_data
iidx-prod_clickhouse_data
iidx-prod_grafana_data
iidx-prod_prometheus_data
iidx-prod_caddy_data
iidx-prod_caddy_config
```

Never run this in production unless intentionally destroying data:

```bash
docker compose down -v
```

Safe stop:

```bash
docker compose down
```

Safe restart:

```bash
docker compose up -d
```

## Verification

Basic checks:

```bash
curl https://explorer.igralabs.com/health
curl https://explorer.igralabs.com/status
curl "https://explorer.igralabs.com/query?chainId=38833&sql=SELECT%20max(num)%20FROM%20blocks"
```

DB checks:

```bash
docker compose exec postgres psql -U tidx -d tidx_igra -c 'SELECT count(*) FROM blocks;'
docker compose exec postgres psql -U tidx -d tidx_igra -c 'SELECT count(*) FROM l2_withdrawals;'
docker compose exec clickhouse clickhouse-client --query 'SHOW TABLES FROM tidx_38833'
docker compose exec clickhouse clickhouse-client --query 'SELECT count() FROM tidx_38833.blocks'
```

Full smoke:

```bash
./stability-smoke.sh
```

The smoke script checks API health, sync lag after subtracting adaptive delay, gap count, Prometheus, Grafana, Docker services, PostgreSQL withdrawals, ClickHouse withdrawals, and public query API.

## Backup

Create a PostgreSQL logical backup:

```bash
./backup-postgres.sh
```

Default output:

```text
./backups/tidx_igra_<utc timestamp>.dump
```

Minimum backup policy:

- PostgreSQL custom-format dump daily
- VM/disk snapshot daily
- ClickHouse volume snapshot weekly
- Off-host copy of backups

PostgreSQL is the most important durable state. ClickHouse can be rebuilt from PostgreSQL/RPC, but that costs time.

## Redeploy

Preferred redeploy:

```bash
cd ~/tidx/deploy/prod
git fetch origin
git checkout igra
git pull --ff-only
./redeploy.sh
```

`redeploy.sh` does:

1. refuses to run with local git changes unless `REQUIRE_CLEAN_GIT=0`
2. creates a PostgreSQL backup
3. pulls images
4. restarts `tidx`, `blockscout-importer`, `grafana`, `prometheus`, and `caddy`
5. runs `stability-smoke.sh`

Manual equivalent:

```bash
./backup-postgres.sh
docker compose pull
docker compose up -d tidx blockscout-importer grafana prometheus caddy
./stability-smoke.sh
```

## Rollback

Rollback is image-tag based.

Edit `.env`:

```env
TIDX_IMAGE=<previous-known-good-image>
```

Then:

```bash
docker compose pull
docker compose up -d tidx blockscout-importer
./stability-smoke.sh
```

Do not rollback by deleting volumes. If schema was migrated forward and rollback binary cannot read it, stop and restore PostgreSQL from a backup/snapshot instead of improvising.

## Kaspa Requirements

The prod host must run a local Kaspa node with Borsh wRPC listening on host loopback:

```text
127.0.0.1:17110
```

The `kaspa-wrpc-proxy` service runs in host networking and exposes that endpoint only on the Docker bridge address configured by:

```env
KASPA_PROXY_BIND_HOST=172.17.0.1
KASPA_PROXY_PORT=17111
```

The `tidx` container connects to:

```toml
rpc_url = "ws://host.docker.internal:17111"
```

Kaspa finality behavior:

- matching Igra Kaspa payloads first enter `kaspa_pending_entries` / `kaspa_pending_l2_submissions`
- explorer shows pending rows as `Pending finality until ...`
- after `promotion_delay_secs = 43200`, pending rows promote to `kaspa_entries` / `kaspa_l2_submissions`
- final rows are shown as `Final`

## Verified Contracts

The `blockscout-importer` service imports source and ABI from:

```env
BLOCKSCOUT_SOURCE_URL=https://explorer.igralabs.com
```

Logs:

```bash
docker compose logs -f blockscout-importer
```

This keeps the local explorer’s verified contract view aligned with the canonical Igra Blockscout source.

## Grafana

Grafana is available locally:

```text
http://127.0.0.1:3000
```

And, if the Caddy route remains enabled:

```text
https://explorer.igralabs.com/grafana
```

For production, prefer VPN/SSO/basic-auth protection for `/grafana` before sharing it broadly.

## Useful Logs

```bash
docker compose logs -f tidx
docker compose logs -f postgres
docker compose logs -f clickhouse
docker compose logs -f caddy
docker compose logs -f blockscout-importer
docker compose logs -f kaspa-wrpc-proxy
```

## Emergency Notes

- If public explorer is down but containers are up, check Caddy logs and DNS/TLS.
- If sync is behind, compare `/status` lag with `tidx_sync_head_delay_blocks`; intentional adaptive delay should not count as outage.
- If Kaspa rows are pending, this is expected until `promotion_delay_secs` passes.
- If PostgreSQL is unhealthy, do not repeatedly restart without checking disk space first.
- If disk is full, stop `tidx` before cleanup to avoid compounding DB errors.
