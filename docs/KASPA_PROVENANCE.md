# Kaspa Provenance Integration

Igra is a based rollup whose L2 transactions are carried by Kaspa L1 transaction
payloads. iidx syncs this L1 provenance as a sidecar to the normal EVM indexer.

## Runtime Config

Enable per chain:

```toml
[chains.kaspa]
enabled = true
rpc_url = "ws://127.0.0.1:17110"
txid_prefix = "97b1"
poll_interval_ms = 1000
initial_tip_distance = 100
promotion_delay_secs = 43200
```

Use Borsh wRPC locally. The staging Kaspa node exposes:

```text
127.0.0.1:16210  gRPC
127.0.0.1:17110  wRPC Borsh
127.0.0.1:18110  wRPC JSON
```

## Sync Flow

```text
get_virtual_chain_from_block_v2(checkpoint, Full, tip_distance)
  -> accepted Kaspa txs only
  -> txid prefix filter
  -> Igra envelope parser
  -> pending Postgres rows
  -> promotion delay / finality window
  -> final Postgres rows
  -> final ClickHouse mirror if configured
```

Payload types:

```text
0x94  L2 transaction payload. l2_tx_hash = keccak256(payload_body).
0x92  native iKAS entry. body = 20-byte recipient + 8-byte little-endian sompi.
```

## Postgres Tables

Persistent operational metadata:

```text
kaspa_provenance_meta
kaspa_sync_state
kaspa_provenance_gaps
```

Hot-zone rows:

```text
kaspa_pending_l2_submissions
kaspa_pending_entries
```

These are normal tables, not PostgreSQL temporary tables. They survive process
restarts and are cleaned by row lifecycle:

```text
promoted row -> delete from pending
reorged accepting block -> delete from pending
unrecoverable gap -> record in kaspa_provenance_gaps
```

Final compact facts:

```text
kaspa_l2_submissions
kaspa_entries
```

Current deployment uses DB per chain, so no `chain_id` is stored per row:

```text
Postgres:    tidx_igra
ClickHouse:  tidx_38833
```

The configured chain id is stored once in `kaspa_provenance_meta` and checked on
startup.

## ClickHouse

If ClickHouse is configured, only final rows are mirrored:

```text
kaspa_l2_submissions
kaspa_entries
```

Pending rows are intentionally not mirrored because they are reorg-sensitive.

## Smoke Queries

Postgres:

```bash
curl "https://stage-roman.igralabs.com/query?chainId=38833&sql=SELECT%20*%20FROM%20kaspa_provenance_meta"
curl "https://stage-roman.igralabs.com/query?chainId=38833&sql=SELECT%20*%20FROM%20kaspa_sync_state"
curl "https://stage-roman.igralabs.com/query?chainId=38833&sql=SELECT%20count(*)%20FROM%20kaspa_pending_l2_submissions"
curl "https://stage-roman.igralabs.com/query?chainId=38833&sql=SELECT%20count(*)%20FROM%20kaspa_l2_submissions"
curl "https://stage-roman.igralabs.com/query?chainId=38833&sql=SELECT%20encode(l2_tx_hash,%20'hex')%20AS%20l2_tx_hash,%20encode(kaspa_txid,%20'hex')%20AS%20kaspa_txid,%20created_at%20FROM%20kaspa_l2_submissions%20ORDER%20BY%20created_at%20DESC%20LIMIT%2010"
```

ClickHouse:

```bash
curl "https://stage-roman.igralabs.com/query?chainId=38833&engine=clickhouse&sql=SELECT%20count()%20FROM%20kaspa_l2_submissions"
curl "https://stage-roman.igralabs.com/query?chainId=38833&engine=clickhouse&sql=SELECT%20l2_tx_hash,%20kaspa_txid,%20created_at%20FROM%20kaspa_l2_submissions%20ORDER%20BY%20created_at%20DESC%20LIMIT%2010"
curl "https://stage-roman.igralabs.com/query?chainId=38833&engine=clickhouse&sql=SELECT%20recipient,%20sum(amount_sompi)%20AS%20amount_sompi%20FROM%20kaspa_entries%20GROUP%20BY%20recipient%20ORDER%20BY%20amount_sompi%20DESC%20LIMIT%2020"
```
