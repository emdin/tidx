# ak47

**High-throughput Tempo blockchain indexer in Rust**

[![Build Status](https://github.com/tempoxyz/ak47/actions/workflows/ci.yml/badge.svg)](https://github.com/tempoxyz/ak47/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

[**Documentation**](./docs/PLAN.md) | [**Installation**](#installation) | [**Quick Start**](#quick-start)

## What is ak47?

ak47 is a high-performance blockchain indexer built specifically for [Tempo](https://tempo.xyz), the blockchain for payments at scale. Inspired by [golden-axe](https://github.com/indexsupply/golden-axe), it indexes blocks, transactions, and logs into TimescaleDB for fast querying.

**Key features:**
- **Tempo-native**: Built for Tempo's instant finality—no reorg handling needed
- **High throughput**: PostgreSQL COPY protocol + pipelined sync for maximum write speed
- **TimescaleDB**: Hypertables with columnar compression for fast analytics
- **SQL queries**: Direct SQL access to indexed data
- **Real-time sync**: Continuous block following with sub-second latency

## Performance

ak47 is optimized for high-throughput indexing:

| Optimization | Description |
|-------------|-------------|
| **Binary COPY** | Uses PostgreSQL's COPY protocol for bulk inserts (2-3x faster than INSERT) |
| **Pipelined sync** | Fetches next batch while writing current batch (overlapped I/O) |
| **Batch RPC** | Fetches blocks and receipts in parallel batches |
| **Gzip compression** | Compresses RPC responses to reduce network overhead |
| **UNNEST/VALUES** | Multi-row inserts for blocks, COPY for txs/logs |
| **Unlogged staging** | Uses unlogged staging tables for transient COPY data |

### Write Path

```
RPC Batch Fetch ──► Decode ──► COPY to staging ──► INSERT SELECT to main table
     │                              │
     │ (parallel)                   │ (pipelined with next fetch)
     ▼                              ▼
 eth_getBlockByNumber          COPY ... BINARY
 eth_getBlockReceipts          INSERT INTO txs SELECT * FROM txs_staging
```

### Benchmarks

Run benchmarks with:
```bash
cargo bench --bench sync_bench
```

Benchmark groups:
- `batch_writes` - Individual table write throughput
- `sync_batch` - Realistic multi-table sync simulation
- `copy_throughput` - COPY performance at scale (1k-30k rows)

## Installation

### From Source

```bash
git clone https://github.com/tempoxyz/ak47
cd ak47
cargo build --release
```

### Using Docker

```bash
docker pull ghcr.io/tempoxyz/ak47:latest
```

## Quick Start

```bash
make dev-up                # Start TimescaleDB + Tempo
make seed DURATION=30 TPS=100   # Generate transactions
make sync FROM=1 TO=2000   # Sync blocks
make status                # Check status
make query SQL="SELECT * FROM txs_hourly LIMIT 10"
make dev-down              # Stop services
```

## Make Targets

| Command | Purpose |
|---------|---------|
| `make dev-up` | Start TimescaleDB + Tempo node |
| `make dev-down` | Stop all services |
| `make seed` | Generate transactions (`DURATION=30 TPS=100`) |
| `make sync` | Sync blocks (`FROM=1 TO=1000`) |
| `make query` | Run SQL query (`SQL="..."`) |
| `make status` | Show sync status and stats |
| `make compress` | Refresh aggregates |
| `make reset` | Reset database (drop all data) |
| `make build` | Build release binary |
| `make test` | Run tests |

### Manual Usage

```bash
# Sync from Tempo testnet
cargo run -- up \
  --rpc https://rpc.testnet.tempo.xyz \
  --db postgres://ak47:ak47@localhost:5433/ak47_test

# Check sync status
cargo run -- status --db postgres://ak47:ak47@localhost:5433/ak47_test

# Sync a specific block range
cargo run -- sync \
  --rpc https://rpc.testnet.tempo.xyz \
  --db postgres://ak47:ak47@localhost:5433/ak47_test \
  forward --from 0 --to 1000

# Query indexed data
cargo run -- query \
  --db postgres://ak47:ak47@localhost:5433/ak47_test \
  "SELECT num, encode(hash, 'hex') FROM blocks ORDER BY num DESC LIMIT 10"

# Refresh aggregates
cargo run -- compress --db postgres://ak47:ak47@localhost:5433/ak47_test
```

### Generate Test Data

```bash
# Seed the local Tempo node with transactions
./scripts/seed.sh 60 5000  # 60 seconds, 5000 TPS
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `ak47 up` | Start continuous sync from chain head |
| `ak47 status` | Show current sync status |
| `ak47 sync forward` | Backfill a specific block range |
| `ak47 query` | Run SQL queries against indexed data |
| `ak47 compress` | Refresh aggregates (and compress chunks for hypertables) |

## Configuration

All commands accept these common flags:

| Flag | Env Variable | Description |
|------|--------------|-------------|
| `--rpc <url>` | `AK47_RPC_URL` | Tempo RPC endpoint |
| `--db <url>` | `AK47_DATABASE_URL` | PostgreSQL/TimescaleDB connection URL |

## Tempo Networks

| Network | Chain ID | RPC |
|---------|----------|-----|
| Presto (mainnet) | 4217 | `https://rpc.presto.tempo.xyz` |
| Andantino (testnet) | 42429 | `https://rpc.testnet.tempo.xyz` |
| Moderato | 42431 | `https://rpc.moderato.tempo.xyz` |

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        ak47 Indexer                          │
├─────────────────────────────────────────────────────────────┤
│  RPC Client ──► Decoder ──► Writer ──► Sync State           │
│      │                         │                             │
│      ▼                         ▼                             │
│  Tempo Node              TimescaleDB                         │
│  (JSON-RPC)        (blocks, txs, logs tables)               │
└─────────────────────────────────────────────────────────────┘
```

### Project Structure

```
ak47/
├── src/
│   ├── cli/          # CLI commands (up, status, sync, query)
│   ├── db/           # Database pool, migrations, partitions
│   ├── sync/         # RPC fetcher, decoder, writer, engine
│   └── types.rs      # Core types (BlockRow, TxRow, LogRow)
├── migrations/       # SQL schema migrations
├── tests/            # Integration tests
└── benches/          # Performance benchmarks
```

## Development

```bash
# Check code
cargo check

# Run tests (requires Docker)
docker compose -f docker-compose.test.yml up -d
cargo test

# Run specific test suites
cargo test --test smoke_test           # Basic sync tests
cargo test --test sync_optimizations_test  # COPY and batch write tests

# Run benchmarks
cargo bench --bench sync_bench   # Write throughput benchmarks
cargo bench --bench query_bench  # Query performance benchmarks
cargo bench --bench write_bench  # Legacy write benchmarks
```

### Test Infrastructure

Tests use a real Tempo node and TimescaleDB instance:
- `docker-compose.test.yml` - Spins up TimescaleDB + Tempo node
- `tests/common/testdb.rs` - Test database helpers with auto-seeding
- `tests/common/tempo.rs` - Tempo node client for test fixtures

## Database Schema

Tables use TimescaleDB hypertables partitioned by block number (2M blocks per chunk):

| Table | Primary Key | Description |
|-------|-------------|-------------|
| **blocks** | `(num)` | Block headers with hash, timestamp, gas info |
| **txs** | `(block_num, idx)` | Transactions with Tempo-specific fields (2D nonces, fee sponsorship, batch calls) |
| **logs** | `(block_num, log_idx)` | Event logs with selector indexing |
| **sync_state** | `(id)` | Current sync progress |

### Compression

TimescaleDB columnar compression is enabled on all hypertables:
- Chunks older than 2M blocks are automatically compressed
- Compression ratios of 10-20x are typical for blockchain data
- Compressed chunks use columnar storage for fast analytical queries

## License

MIT License - see [LICENSE](./LICENSE) for details.

## Acknowledgments

- [golden-axe](https://github.com/indexsupply/golden-axe) - Inspiration for the indexing architecture
- [Tempo](https://github.com/tempoxyz/tempo) - The blockchain we're indexing
- [Reth](https://github.com/paradigmxyz/reth) - Rust Ethereum patterns and practices
