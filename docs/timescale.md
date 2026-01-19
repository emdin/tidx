# TimescaleDB Optimization

ak47 uses TimescaleDB features for efficient analytics on blockchain data.

## Materialized Views

| View | Bucket | Purpose |
|------|--------|---------|
| `txs_hourly` | 1 hour | Recent transaction activity, alerting |
| `txs_daily` | 1 day | Daily transaction volume, trends |
| `blocks_hourly` | 1 hour | Recent block production, gas spikes |
| `blocks_daily` | 1 day | Daily block stats, gas trends |
| `logs_hourly` | 1 hour | Recent event activity by selector |
| `logs_daily` | 1 day | Daily event counts by selector |

### Refresh

Views are **not auto-refreshed** (standard PostgreSQL materialized views). Refresh manually:

```bash
# Refresh all views
ak47 compress --db $DATABASE_URL

# Or via SQL
REFRESH MATERIALIZED VIEW txs_hourly;
REFRESH MATERIALIZED VIEW txs_daily;
REFRESH MATERIALIZED VIEW blocks_hourly;
REFRESH MATERIALIZED VIEW blocks_daily;
REFRESH MATERIALIZED VIEW logs_hourly;
REFRESH MATERIALIZED VIEW logs_daily;
```

## Time Bucket Tradeoffs

| Period | Storage | Refresh Speed | Granularity | Best For |
|--------|---------|---------------|-------------|----------|
| 1 min | High | Slow | Very fine | Real-time dashboards |
| **1 hour** | Medium | Fast | Good | Recent activity, alerts |
| **1 day** | Low | Fastest | Coarse | Historical trends, reports |
| 1 week | Very low | Fastest | Very coarse | Long-term analytics |

**Smaller buckets** = more rows, slower refresh, finer detail  
**Larger buckets** = fewer rows, faster refresh, less detail

## Example Queries

### Hourly transaction counts
```sql
SELECT bucket, tx_count, total_gas 
FROM txs_hourly 
ORDER BY bucket DESC 
LIMIT 24;
```

### Daily block stats
```sql
SELECT bucket, block_count, max_gas, avg_gas::bigint 
FROM blocks_daily 
ORDER BY bucket DESC 
LIMIT 7;
```

### Daily unique senders
```sql
SELECT bucket, tx_count, unique_senders 
FROM txs_daily 
ORDER BY bucket DESC;
```

### Top events by selector (last 24h)
```sql
SELECT 
    encode(selector, 'hex') as event_selector,
    SUM(log_count) as total_logs
FROM logs_hourly 
WHERE bucket > NOW() - INTERVAL '24 hours'
GROUP BY selector
ORDER BY total_logs DESC
LIMIT 10;
```

## Hypertables (Optional)

For fresh installs wanting full TimescaleDB features (automatic compression, continuous aggregates with auto-refresh), use:

```bash
# Instead of standard migrations
psql $DATABASE_URL -f migrations/006_timescale_hypertables.sql
```

This provides:
- **Hypertables**: Auto-chunked by block number (2M blocks/chunk)
- **Compression**: 90%+ storage reduction on old chunks
- **Continuous aggregates**: Auto-refreshing materialized views
- **Chunk exclusion**: Query planner skips irrelevant time ranges

### Check compression stats (hypertables only)
```sql
SELECT 
    hypertable_name,
    total_chunks,
    number_compressed_chunks,
    pg_size_pretty(before_compression_total_bytes) as before,
    pg_size_pretty(after_compression_total_bytes) as after
FROM timescaledb_information.compression_settings cs
JOIN LATERAL hypertable_compression_stats(cs.hypertable_schema || '.' || cs.hypertable_name) ON true;
```
