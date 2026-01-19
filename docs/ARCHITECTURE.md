# AK47 Architecture Plan

> A high-throughput indexer for Tempo, focused on sub-millisecond OLTP and OLAP queries.

## BLUF (Bottom Line Up Front)

- **Golden Axe limitations**: No OLAP support, single-chain queries, no reorg handling, memory-based rate limits
- **AK47 solution**: Hybrid OLTP/OLAP via TimescaleDB, dynamic selector tables, bidirectional sync, Tempo-native
- **Target**: Sub-millisecond queries for both row-based (OLTP) and column-based (OLAP) workloads
- **Scope**: Tempo mainnet + testnets only (Ethereum interop deferred)

---

## 1. High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              AK47 Indexer                                    │
├─────────────────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐        │
│  │   Config    │  │    Sync     │  │   Query     │  │    Ops      │        │
│  │  + Network  │  │ Coordinator │  │    API      │  │  (Metrics)  │        │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘  └─────────────┘        │
│         │                │                │                                  │
│  ┌──────┴────────────────┴────────────────┴──────┐                          │
│  │              Ingestion Pipeline               │                          │
│  │  RPC Fetch → Tempo Decode → Reorg → DB Write  │                          │
│  └───────────────────────┬───────────────────────┘                          │
│                          │                                                   │
│  ┌───────────────────────┴───────────────────────┐                          │
│  │           Query Engine (JIT CTE)              │                          │
│  │     signature → selector → indexed query      │                          │
│  └───────────────────────────────────────────────┘                          │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                         TimescaleDB (PostgreSQL)                             │
├─────────────────────────────────────────────────────────────────────────────┤
│  ┌─────────────────────┐  ┌─────────────────────┐  ┌─────────────────────┐ │
│  │   Static Tables     │  │  Selector Indexes   │  │   Continuous        │ │
│  │   (blocks, txs,     │  │  (chain, selector,  │  │   Aggregates        │ │
│  │   logs)             │  │   block_timestamp)  │  │   (analytics)       │ │
│  │                     │  │                     │  │                     │ │
│  └─────────────────────┘  └─────────────────────┘  └─────────────────────┘ │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │  Hypercore: Rowstore (hot) ←→ Columnstore (cold, compressed 90-98%)    ││
│  └─────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Module Breakdown

### 2.1 Core Layer

| Module | Responsibility |
|--------|---------------|
| **Config + Network Registry** | Tempo mainnet/testnet chain IDs, RPC URLs, finality params |
| **Sync Coordinator** | Range job scheduling, cursor management, backpressure control |
| **Reorg Manager** | Detects chain reorgs via parent hash mismatch, rolls back orphaned blocks |

### 2.2 Ingestion Pipeline

| Module | Responsibility |
|--------|---------------|
| **RPC Fetchers** | Batched block/receipt fetching with retries, hedging, circuit breakers |
| **Tempo Decoder** | Parses tx type 0x76, batch calls, 2D nonces, subblocks, system txs |
| **DB Writer** | High-throughput COPY + upsert, idempotent writes, partition management |

### 2.3 Query Layer

| Module | Responsibility |
|--------|---------------|
| **Query API** | Read-only SQL with statement timeout, row limits, prepared statements |
| **Selector Registry** | JIT signature→selector conversion, dynamic table creation/lookup |
| **ABI Store** | Optional user-provided ABIs for JIT decoding (cached in memory) |

### 2.4 Operations

| Module | Responsibility |
|--------|---------------|
| **Reth Hydration** | Import reth snapshots to bootstrap historical data (future) |
| **Rate Limiter** | Postgres-backed token buckets (survives restarts, works across replicas) |
| **Observability** | Prometheus metrics, structured logs, OTLP tracing |

---

## 3. Database Schema Design

### 3.1 Design Principles

- **Hypertables** for time-series data (blocks, events) with automatic chunking
- **Columnstore** for historical data (>24h) with 90-98% compression
- **Rowstore** for recent data enabling fast OLTP point queries
- **Raw bytea** storage (not hex text) for efficiency
- **Soft-delete reorgs** via `is_canonical` flag for safety

### 3.2 Static Tables

Following Golden Axe's schema patterns with Tempo extensions:

```sql
-- Blocks (partitioned by chain, then by block range)
-- Matches Golden Axe: be/src/sql/schema.sql
CREATE TABLE blocks (
  chain INT8 NOT NULL,
  num INT8 NOT NULL,
  timestamp TIMESTAMPTZ NOT NULL,
  size INT4 DEFAULT 0,
  
  gas_limit NUMERIC NOT NULL,
  gas_used NUMERIC NOT NULL,
  nonce BYTEA NOT NULL,
  hash BYTEA NOT NULL,
  receipts_root BYTEA NOT NULL,
  state_root BYTEA NOT NULL,
  extra_data BYTEA NOT NULL,
  miner BYTEA NOT NULL
) PARTITION BY LIST(chain);

-- Transactions (Tempo tx type 0x76 native)
-- Matches Golden Axe with Tempo extensions
CREATE TABLE txs (
  chain INT8 NOT NULL,
  block_num INT8 NOT NULL,
  block_timestamp TIMESTAMPTZ NOT NULL,
  idx INT4 NOT NULL,
  type INT2 NOT NULL,  -- 0x76 for Tempo native
  
  gas NUMERIC NOT NULL,
  gas_price NUMERIC NOT NULL,
  nonce BYTEA NOT NULL,
  hash BYTEA NOT NULL,
  "from" BYTEA NOT NULL,
  "to" BYTEA NOT NULL,
  input BYTEA NOT NULL,
  value NUMERIC NOT NULL,
  fee_token BYTEA,
  calls JSONB,
  fee_payer BYTEA,
  nonce_key BYTEA,              -- 2D nonce key
  valid_before INT8,
  valid_after INT8,
  key_authorization JSONB,      -- spending limits per key
  authorization_list JSONB,     -- EIP-7702
  signature JSONB               -- {type: "secp256k1"|"p256"|"webauthn", r, s, v?, x?, y?, prehash?, authData?, clientData?}
) PARTITION BY LIST(chain);

-- Logs (partitioned like Golden Axe, with selector for fast queries)
CREATE TABLE logs (
  chain INT8 NOT NULL,
  block_num INT8 NOT NULL,
  block_timestamp TIMESTAMPTZ NOT NULL,
  log_idx INT4 NOT NULL,
  
  tx_hash BYTEA NOT NULL,
  address BYTEA NOT NULL,
  selector BYTEA,         -- first 4 bytes of topics[1], extracted during sync
  topics BYTEA[] NOT NULL,
  data BYTEA NOT NULL
) PARTITION BY LIST(chain);

-- ABI helper functions (from Golden Axe: be/src/sql/schema.sql)
CREATE OR REPLACE FUNCTION abi_uint(input BYTEA) RETURNS NUMERIC AS $$
DECLARE n NUMERIC := 0;
BEGIN
  FOR i IN 1..length(input) LOOP
    n := n * 256 + get_byte(input, i - 1);
  END LOOP;
  RETURN n;
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE OR REPLACE FUNCTION abi_address(input BYTEA) RETURNS BYTEA AS $$
BEGIN RETURN substring(input FROM 13 FOR 20); END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE OR REPLACE FUNCTION abi_fixed_bytes(input BYTEA, pos INT, n INT) RETURNS BYTEA AS $$
BEGIN RETURN substring(input FROM pos+1 FOR n); END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE OR REPLACE FUNCTION abi_bytes(input BYTEA) RETURNS BYTEA AS $$
DECLARE length INT;
BEGIN
  length := get_byte(input, 31);
  RETURN substring(input FROM 33 FOR length);
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

-- Indexes for fast selector queries (no physical tables needed)
CREATE INDEX idx_logs_selector ON logs (chain, selector, block_timestamp DESC);
CREATE INDEX idx_logs_address ON logs (chain, address, block_timestamp DESC);
CREATE INDEX idx_txs_selector ON txs (chain, substring(input, 1, 4), block_timestamp DESC);
```

### 3.3 Dynamic Partitioning (from Golden Axe)

Partitions created dynamically per chain and block range (2M blocks per partition):

```sql
-- Template from Golden Axe: be/src/sync.rs setup_tables()
-- Creates partitions on-demand as sync progresses

-- Chain partition (once per chain)
CREATE TABLE IF NOT EXISTS blocks_c{{chain}}
  PARTITION OF blocks FOR VALUES IN ({{chain}})
  PARTITION BY RANGE (num);

CREATE TABLE IF NOT EXISTS txs_c{{chain}}
  PARTITION OF txs FOR VALUES IN ({{chain}})
  PARTITION BY RANGE (block_num);

CREATE TABLE IF NOT EXISTS logs_c{{chain}}
  PARTITION OF logs FOR VALUES IN ({{chain}})
  PARTITION BY RANGE (block_num);

-- Block range partition (every 2M blocks)
CREATE TABLE IF NOT EXISTS blocks_c{{chain}}_b{{label}}
  PARTITION OF blocks_c{{chain}}
  FOR VALUES FROM ({{from}}) TO ({{to}});

CREATE TABLE IF NOT EXISTS txs_c{{chain}}_b{{label}}
  PARTITION OF txs_c{{chain}}
  FOR VALUES FROM ({{from}}) TO ({{to}});
ALTER TABLE txs_c{{chain}}_b{{label}} SET (toast_tuple_target = 128);

CREATE TABLE IF NOT EXISTS logs_c{{chain}}_b{{label}}
  PARTITION OF logs_c{{chain}}
  FOR VALUES FROM ({{from}}) TO ({{to}});
ALTER TABLE logs_c{{chain}}_b{{label}} SET (toast_tuple_target = 128);
```

### 3.4 Sync State

```sql
CREATE TABLE sync_cursors (
  chain INT8 PRIMARY KEY,
  head_num INT8 NOT NULL DEFAULT 0,
  finalized_num INT8 NOT NULL DEFAULT 0,
  forward_cursor INT8 NOT NULL DEFAULT 0,
  backward_cursor INT8,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sync_jobs (
  id BIGSERIAL PRIMARY KEY,
  chain INT8 NOT NULL,
  job_type TEXT NOT NULL,  -- 'forward', 'backward', 'realtime'
  from_block INT8 NOT NULL,
  to_block INT8 NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  started_at TIMESTAMPTZ,
  completed_at TIMESTAMPTZ,
  error TEXT
);
```

---

## 4. Performance Optimizations

### 4.1 Database (Low-Hanging Fruit)

| Optimization | Impact | Implementation |
|--------------|--------|----------------|
| **COPY vs INSERT** | 10-50x faster bulk loads | Use `COPY` binary protocol for sync |
| **Prepared statements** | 2-5x query speedup | Cache parsed queries per connection |
| **Unlogged staging tables** | 3-5x faster writes | Stage data, then `INSERT...SELECT` |
| **Disable fsync (initial sync)** | 2-3x faster | `synchronous_commit = off` during backfill |
| **Partial indexes** | Faster lookups, smaller index | `WHERE is_canonical = true` |
| **Connection pooling** | Eliminate connection overhead | `deadpool-postgres` with 20-50 conns |

```sql
-- Partial index for canonical blocks only
CREATE INDEX idx_blocks_canonical ON blocks (chain, num DESC) 
  WHERE is_canonical = true;

-- Covering index to avoid heap lookups
CREATE INDEX idx_txs_hash ON txs (chain, hash) 
  INCLUDE (block_num, idx, "from", "to");

-- BRIN indexes for time-ordered data (100-1000x smaller than B-tree)
-- Perfect for block_num/block_timestamp since data is naturally ordered
CREATE INDEX idx_logs_block_num_brin ON logs USING brin (block_num) 
  WITH (pages_per_range = 32);
CREATE INDEX idx_txs_block_num_brin ON txs USING brin (block_num) 
  WITH (pages_per_range = 32);

-- BRIN works because:
--   1. Data inserted in block order (append-only)
--   2. Queries filter by block range (WHERE block_num BETWEEN x AND y)
--   3. Index stores min/max per 32 pages vs every row
--   4. ~100KB index vs ~10GB B-tree for 1B rows
```

### 4.2 RPC Fetching

| Optimization | Impact | Implementation |
|--------------|--------|----------------|
| **Batch JSON-RPC** | 10-20x fewer round trips | `[{method: eth_getBlockByNumber}, ...]` |
| **Connection keep-alive** | Eliminate TCP/TLS handshake | `reqwest` with connection pool |
| **Parallel fetchers** | Linear speedup | 8-16 concurrent fetchers per chain |
| **Request hedging** | Cut tail latency | Fire duplicate after p95 timeout |
| **Bloom filter check** | Skip empty log fetches | Check `logs_bloom != 0` before eth_getLogs |

---

## 5. Tempo-Specific Features

### 5.1 Transaction Type 0x76 Support

| Feature | Description | Indexed |
|---------|-------------|---------|
| Batch Calls | Multiple atomic calls per tx | ✅ |
| 2D Nonces | `(nonce_key, nonce)` parallelization | ✅ |
| Fee Sponsorship | `fee_payer` + `fee_token` | ✅ |
| Time Windows | `valid_before`, `valid_after` | ✅ |
| Key Authorizations | Spending limits per key | ✅ |
| Multi-Sig | secp256k1, P256, WebAuthn | ✅ |

### 5.2 Query Examples

```sql
-- Find all sponsored transactions
SELECT * FROM txs 
WHERE fee_payer IS NOT NULL 
  AND fee_payer != "from";

-- Batch transactions with 3+ calls
SELECT hash, jsonb_array_length(calls) as call_count 
FROM txs 
WHERE jsonb_array_length(calls) >= 3;

-- Time-windowed transactions
SELECT * FROM txs 
WHERE valid_after IS NOT NULL 
  AND valid_before IS NOT NULL;

-- Fee token distribution
SELECT fee_token, COUNT(*) 
FROM txs 
WHERE fee_token IS NOT NULL 
GROUP BY fee_token;

-- Query logs by event signature (JIT selector lookup)
SELECT * FROM log_ddf252ad  -- Transfer(address,address,uint256)
WHERE chain = 1 
  AND block_timestamp > now() - interval '1 day';

-- Decode batch call selectors
SELECT hash, call->>'to' as to_addr, 
       substring(decode(call->>'input', 'hex') for 4) as selector
FROM txs, jsonb_array_elements(calls) as call
WHERE calls IS NOT NULL;
```

---

## 6. Implementation Phases

### Phase 0: Test Harness (Day 1)
- [ ] Criterion benchmarks (OLTP + OLAP)
- [ ] Ephemeral test DB (docker-compose.test.yml)
- [ ] CI pipeline with regression gates
- [ ] Golden Axe performance baselines

### Phase 1: Core Infrastructure (2-3 weeks)
- [ ] Project scaffolding (single crate, Cargo.toml)
- [ ] TimescaleDB setup with hypertables
- [ ] Static table schema + migrations
- [ ] Basic RPC fetcher for Tempo
- [ ] Tempo transaction decoder (tx type 0x76)

### Phase 2: Sync Engine (2-3 weeks)
- [ ] Forward historical sync
- [ ] Backward historical sync
- [ ] Realtime tail sync
- [ ] Reorg detection and handling
- [ ] Job queue with retry logic

### Phase 3: Query Engine (1-2 weeks)
- [ ] Selector index strategy (no physical tables)
- [ ] JIT CTE rewriting (Golden Axe pattern)
- [ ] ABI decoding helpers

### Phase 4: Query API (1-2 weeks)
- [ ] Raw SQL endpoint
- [ ] Rate limiting
- [ ] Statement timeout + row limits

### Phase 5: CLI & Docker (1 week)
- [ ] up/down/status/query commands
- [ ] sync control
- [ ] Dockerfile + docker-compose.yml
- [ ] Curl installer script

### Phase 6: Observability & Polish (1 week)
- [ ] Prometheus metrics
- [ ] Structured logging
- [ ] Health checks
- [ ] Documentation

---

## 7. Future Considerations

### Deferred to v2
- [ ] Multi-chain queries (cross-chain analytics)
- [ ] Ethereum L1 interop
- [ ] GraphQL API
- [ ] WebSocket subscriptions
- [ ] Reth snapshot hydration
