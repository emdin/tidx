# Test-Prod Deployment Bundle

This bundle deploys a single-host `igra` test-prod environment with:

- `caddy` for public TLS termination
- `tidx` from your local `igra` branch/image
- `blockscout-importer` to keep verified contracts synced from the public Igra Blockscout
- `postgres` for primary chain data
- `clickhouse` for OLAP / analytics queries
- `prometheus` for scraping metrics
- `grafana` for dashboards, also exposed at `/grafana`

It is intentionally opinionated:

- only the explorer is public
- PostgreSQL, ClickHouse, Prometheus, and Grafana are not public by default
- admin explorer writes stay limited to `trusted_cidrs` from [config.toml](/Users/user/Source/igra/tidx/deploy/testprod/config.toml:1)

## Recommended Host

- Ubuntu 24.04
- 8 vCPU
- 32 GB RAM
- 500 GB NVMe SSD
- static public IPv4

This is appropriate for a first serious `test-prod` environment. Increase disk first if you expect long retention and active ClickHouse usage.

## DNS And Domain

Create one public DNS record:

- `A explore-test.igralabs.com -> <your server public IP>`

Recommended DNS settings:

- TTL: `300`
- if you use Cloudflare, start with `DNS only`, not proxied

Caddy will automatically request and renew the TLS certificate for `explore-test.igralabs.com` once:

1. DNS resolves to the server
2. ports `80` and `443` are reachable from the internet

You do not need to provision certificates manually.

## Firewall

Open only:

- `80/tcp`
- `443/tcp`
- `22/tcp` from office/VPN IPs only

Do not open:

- `5432`
- `8123`
- `9000`
- `8080`
- `9090`
- `3000`

The compose file keeps those services internal or bound to localhost only.

## First Boot

From the repo root on the target server:

```bash
git clone https://github.com/reshmem/tidx.git
cd tidx
git checkout igra
cd deploy/testprod
cp .env.example .env
```

Edit:

- [.env.example](/Users/user/Source/igra/tidx/deploy/testprod/.env.example:1) copied to `.env`
- [config.toml](/Users/user/Source/igra/tidx/deploy/testprod/config.toml:1)

Fields you must review:

- `EXPLORER_DOMAIN`
- `EXPLORER_BASE_URL`
- `LETSENCRYPT_EMAIL`
- `BLOCKSCOUT_SOURCE_URL`
- `BLOCKSCOUT_IMPORT_INTERVAL_SECONDS`
- `POSTGRES_PASSWORD`
- `GRAFANA_ADMIN_PASSWORD`
- `trusted_cidrs`
- `rpc_url` if your test-prod RPC endpoint changes

Build and start:

```bash
docker compose build tidx
docker compose up -d
```

Watch startup:

```bash
docker compose logs -f tidx
docker compose logs -f caddy
```

## What Each Service Does

- `caddy`
  - serves `https://explore-test.igralabs.com`
  - terminates TLS
  - proxies traffic to `tidx:8080`
- `postgres`
  - primary OLTP store for blocks, txs, receipts, logs, explorer metadata
- `clickhouse`
  - OLAP store for large scans and analytical queries
- `tidx`
  - indexer + explorer API + explorer frontend
- `blockscout-importer`
  - runs `tidx import-blockscout` on a loop against `https://explorer.igralabs.com`
  - imports historical verified contracts on first run
  - keeps newly verified contracts synced into the local explorer after that
- `prometheus`
  - scrapes `/metrics` from `tidx`
- `grafana`
  - dashboard UI on `https://explore-test.igralabs.com/grafana`
  - also bound on `127.0.0.1:3000` for direct host access
  - renders explorer drilldown URLs from `EXPLORER_BASE_URL` at container startup

## Access

Public explorer:

- `https://explore-test.igralabs.com`
- the domain root redirects to `/explore`

Grafana:

- public route: `https://explore-test.igralabs.com/grafana`
- local on the host: `http://127.0.0.1:3000`
- login user: `admin`
- password: the `GRAFANA_ADMIN_PASSWORD` value from `.env`
- or via SSH tunnel:

```bash
ssh -L 3000:127.0.0.1:3000 user@your-server
```

Prometheus:

- local on the host: `http://127.0.0.1:9091`

Provisioned Grafana resources:

- `ClickHouse` datasource pointed at the internal `clickhouse:8123`
- `Prometheus` datasource pointed at `prometheus:9090`
- `Igra ClickHouse Analytics` dashboard
- `Igra Financial Overview` dashboard
- `Igra Rollup Health` dashboard
- `Igra Application Activity` dashboard
- `Igra Investigator Workbench` dashboard
- `Igra Risk Monitor` dashboard

## Explorer Admin Writes

The explorer admin panel for:

- labels
- metadata refresh
- contract verification imports

is allowed only from `trusted_cidrs`.

If you use Tailscale, a common setting is:

```toml
trusted_cidrs = ["100.64.0.0/10"]
```

If you leave `trusted_cidrs` empty, only loopback is trusted.

## Runtime Notes

- Keep `trust_rpc = false` for now.
  - During local testing on April 13, 2026, the `igra` chain showed reorg handling in the live sync logs.
- Keep `head_delay_blocks = 30` for Igra staging unless metrics show the safe head can be tightened.
  - `head_num` remains the true RPC head.
  - `tip_num` follows `head_num - current adaptive delay`.
  - Adaptive delay starts at `30`, increases up to `100` if repeated near-tip instability is detected, and later decreases back to `30` after a quiet window.
  - Alerts subtract the current delay so intentional safety lag does not page the team.
- Your RPC currently blocks `eth_getBlockReceipts`.
  - `tidx` will fall back to batched `eth_getTransactionReceipt`.
  - This works, but it is slower than block-level receipt fetching.
- Verified contract source and ABI are imported from the canonical Igra Blockscout.
  - The first importer run backfills old verified contracts.
  - Later runs skip already imported contracts and only pick up new ones.
- ClickHouse is enabled in this bundle because you asked for the whole package.
  - The explorer can run without it, but analytics and heavier scans benefit from it.

## Smoke Checks

After startup:

```bash
curl https://explore-test.igralabs.com/health
curl https://explore-test.igralabs.com/status
curl "https://explore-test.igralabs.com/explore/api/tokens?chainId=38833&limit=5"
```

Run the full staging stability check from this directory:

```bash
./stability-smoke.sh
```

It validates API health, effective sync lag beyond the configured head delay, persistent gap count, Prometheus, Grafana, PostgreSQL, ClickHouse, and a known L2 withdrawal row.

Blockscout importer logs:

```bash
docker compose logs -f blockscout-importer
```

If DNS is not ready yet, run the same checks locally on the server with:

```bash
curl http://127.0.0.1/health
```

or:

```bash
docker compose exec caddy wget -qO- http://tidx:8080/health
```

## Backups

Minimum backup policy:

- daily PostgreSQL logical dump
- daily VM or disk snapshot
- weekly ClickHouse volume snapshot

PostgreSQL matters most. ClickHouse can be rebuilt from reindexing if necessary, but that takes time.

## Upgrade Flow

When you want to deploy a newer `igra` branch:

```bash
git pull
git checkout igra
cd deploy/testprod
docker compose build tidx
docker compose up -d tidx
```

Watch:

```bash
docker compose logs -f tidx
```

## Optional Hardening

- add Cloudflare proxy after direct DNS-only validation succeeds
- add HTTP basic auth in [Caddyfile](/Users/user/Source/igra/tidx/deploy/testprod/Caddyfile:1) if you want the explorer private during test-prod
- move Grafana behind a second authenticated reverse-proxy route if you want remote dashboards without SSH tunneling
- create separate Grafana users instead of sharing the initial admin password
