//! Large-scale benchmarks with 20M rows to test realistic OLAP performance.
//!
//! Run with:
//! ```sh
//! cargo bench --bench scale_bench
//! ```

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Runtime;

use ak47::db::DuckDbPool;

const TARGET_ROWS: usize = 20_000_000;

/// Generate synthetic data directly in DuckDB using SQL.
fn setup_duckdb_with_rows(row_count: usize) -> Arc<DuckDbPool> {
    let pool = DuckDbPool::in_memory().expect("Failed to create DuckDB pool");

    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let conn = pool.conn().await;

        println!("Generating {row_count} blocks...");
        let start = Instant::now();

        // Generate blocks using generate_series (very fast)
        conn.execute(
            &format!(
                r#"INSERT INTO blocks (num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner)
                   SELECT 
                       i as num,
                       '0x' || lpad(printf('%x', i), 64, '0') as hash,
                       '0x' || lpad(printf('%x', i - 1), 64, '0') as parent_hash,
                       make_timestamptz(2024, 1, 1, 0, 0, 0) + to_seconds(i) as timestamp,
                       1704067200000 + i * 1000 as timestamp_ms,
                       30000000 as gas_limit,
                       15000000 + (i % 10000000) as gas_used,
                       '0x' || lpad(printf('%x', i % 1000), 40, '0') as miner
                   FROM generate_series(1, {row_count}) as t(i)"#
            ),
            [],
        )
        .expect("Failed to insert blocks");

        println!("  Blocks inserted in {:?}", start.elapsed());

        // Generate transactions - use block_num directly as i, with single tx per block
        let tx_count = row_count.min(2_000_000); // Cap at 2M for txs
        println!("Generating {tx_count} transactions...");
        let start = Instant::now();

        conn.execute(
            &format!(
                r#"INSERT INTO txs (block_num, block_timestamp, idx, hash, type, "from", "to", value, input,
                                   gas_limit, max_fee_per_gas, max_priority_fee_per_gas, gas_used,
                                   nonce_key, nonce, call_count)
                   SELECT 
                       i as block_num,
                       make_timestamptz(2024, 1, 1, 0, 0, 0) + to_seconds(i) as block_timestamp,
                       0 as idx,
                       '0x' || lpad(printf('%x', i), 64, '0') as hash,
                       (i % 3)::smallint as type,
                       '0x' || lpad(printf('%x', i % 10000), 40, '0') as "from",
                       '0x' || lpad(printf('%x', (i + 1) % 10000), 40, '0') as "to",
                       (i % 1000000000)::text as value,
                       '0x' || lpad(printf('%x', i % 256), 8, '0') as input,
                       21000 + (i % 1000000) as gas_limit,
                       '1000000000' as max_fee_per_gas,
                       '100000000' as max_priority_fee_per_gas,
                       21000 as gas_used,
                       '0x' || lpad(printf('%x', i % 10000), 40, '0') as nonce_key,
                       (i / 10000)::bigint as nonce,
                       1::smallint as call_count
                   FROM generate_series(1, {tx_count}) as t(i)"#
            ),
            [],
        )
        .expect("Failed to insert txs");

        println!("  Transactions inserted in {:?}", start.elapsed());

        // Generate logs (Transfer events) - one log per block for unique (block_num, log_idx)
        let log_count = row_count;
        println!("Generating {log_count} logs (Transfer events)...");
        let start = Instant::now();

        // Transfer event topic0
        let transfer_topic0 = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

        conn.execute(
            &format!(
                r#"INSERT INTO logs (block_num, block_timestamp, log_idx, tx_idx, tx_hash, address, selector, topics, data)
                   SELECT 
                       i as block_num,
                       make_timestamptz(2024, 1, 1, 0, 0, 0) + to_seconds(i) as block_timestamp,
                       0 as log_idx,
                       0 as tx_idx,
                       '0x' || lpad(printf('%x', i), 64, '0') as tx_hash,
                       '0x' || lpad(printf('%x', i % 100), 40, '0') as address,
                       '0xddf252ad' as selector,
                       [
                           '{transfer_topic0}',
                           '0x' || lpad(printf('%x', i % 10000), 64, '0'),
                           '0x' || lpad(printf('%x', (i + 1) % 10000), 64, '0')
                       ] as topics,
                       '0x' || lpad(printf('%x', i % 1000000000000), 64, '0') as data
                   FROM generate_series(1, {log_count}) as t(i)"#
            ),
            [],
        )
        .expect("Failed to insert logs");

        println!("  Logs inserted in {:?}", start.elapsed());

        // Verify counts
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM blocks").unwrap();
        let block_count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();

        let mut stmt = conn.prepare("SELECT COUNT(*) FROM txs").unwrap();
        let tx_count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();

        let mut stmt = conn.prepare("SELECT COUNT(*) FROM logs").unwrap();
        let log_count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();

        println!("\nData loaded: {block_count} blocks, {tx_count} txs, {log_count} logs");
    });

    Arc::new(pool)
}

fn bench_scale_olap(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    println!("\n=== Setting up DuckDB with {TARGET_ROWS} rows ===\n");
    let start = Instant::now();
    let duck_pool = setup_duckdb_with_rows(TARGET_ROWS);
    println!("\nTotal setup time: {:?}\n", start.elapsed());

    let mut group = c.benchmark_group("scale_20m");
    group.significance_level(0.05);
    group.sample_size(20); // Fewer samples for large-scale tests

    // COUNT(*) - full table scan
    group.bench_function("count_blocks", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool.query("SELECT COUNT(*) FROM blocks").await.unwrap();
        });
    });

    group.bench_function("count_logs", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool.query("SELECT COUNT(*) FROM logs").await.unwrap();
        });
    });

    // SUM aggregation
    group.bench_function("sum_gas_used", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query("SELECT SUM(gas_used) FROM blocks")
                .await
                .unwrap();
        });
    });

    // GROUP BY with high cardinality
    group.bench_function("group_by_miner", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query("SELECT miner, COUNT(*) as cnt FROM blocks GROUP BY miner ORDER BY cnt DESC LIMIT 10")
                .await
                .unwrap();
        });
    });

    // GROUP BY on txs
    group.bench_function("group_by_from", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query(r#"SELECT "from", COUNT(*) as cnt FROM txs GROUP BY "from" ORDER BY cnt DESC LIMIT 10"#)
                .await
                .unwrap();
        });
    });

    // COUNT DISTINCT
    group.bench_function("count_distinct_senders", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query(r#"SELECT COUNT(DISTINCT "from") FROM txs"#)
                .await
                .unwrap();
        });
    });

    group.bench_function("count_distinct_addresses", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query("SELECT COUNT(DISTINCT address) FROM logs")
                .await
                .unwrap();
        });
    });

    // Window functions
    group.bench_function("running_sum_gas", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query(
                    "SELECT num, gas_used,
                            SUM(gas_used) OVER (ORDER BY num ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) as running
                     FROM blocks
                     LIMIT 10000",
                )
                .await
                .unwrap();
        });
    });

    // ABI decoding with native UDFs
    group.bench_function("transfer_decode_all", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query(
                    r#"SELECT 
                           topic_address_native(topics[2]) AS "from",
                           topic_address_native(topics[3]) AS "to",
                           abi_uint_native(data, 0) AS value
                       FROM logs
                       WHERE selector = '0xddf252ad'
                       LIMIT 100000"#,
                )
                .await
                .unwrap();
        });
    });

    group.bench_function("transfer_group_by_to", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query(
                    r#"WITH transfer AS (
                        SELECT 
                            topic_address_native(topics[3]) AS "to"
                        FROM logs
                        WHERE selector = '0xddf252ad'
                    )
                    SELECT "to", COUNT(*) as cnt
                    FROM transfer
                    GROUP BY "to"
                    ORDER BY cnt DESC
                    LIMIT 10"#,
                )
                .await
                .unwrap();
        });
    });

    group.bench_function("transfer_sum_values", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query(
                    r#"SELECT SUM(abi_uint_native(data, 0))
                       FROM logs
                       WHERE selector = '0xddf252ad'"#,
                )
                .await
                .unwrap();
        });
    });

    // Point lookups (should still be fast with indexes)
    group.bench_function("point_lookup_block", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query("SELECT * FROM blocks WHERE num = 10000000")
                .await
                .unwrap();
        });
    });

    // Range scan
    group.bench_function("range_scan_1000_blocks", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query("SELECT * FROM blocks WHERE num BETWEEN 10000000 AND 10001000")
                .await
                .unwrap();
        });
    });

    // Complex analytics query
    group.bench_function("hourly_gas_stats", |b| {
        b.to_async(&rt).iter(|| async {
            let _result = duck_pool
                .query(
                    "SELECT 
                        date_trunc('hour', timestamp) as hour,
                        COUNT(*) as block_count,
                        SUM(gas_used) as total_gas,
                        AVG(gas_used) as avg_gas,
                        MAX(gas_used) as max_gas
                     FROM blocks
                     GROUP BY 1
                     ORDER BY 1 DESC
                     LIMIT 24",
                )
                .await
                .unwrap();
        });
    });

    group.finish();
}

// Scaling comparison: run same query at different row counts
fn bench_scaling_curve(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("scaling_curve");
    group.significance_level(0.05);
    group.sample_size(10);

    for row_count in [100_000, 1_000_000, 5_000_000, 10_000_000] {
        println!("\n=== Setting up DuckDB with {row_count} rows ===\n");
        let duck_pool = setup_duckdb_with_rows(row_count);

        group.bench_with_input(
            BenchmarkId::new("count_logs", row_count),
            &row_count,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    let _result = duck_pool.query("SELECT COUNT(*) FROM logs").await.unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("sum_gas", row_count),
            &row_count,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    let _result = duck_pool
                        .query("SELECT SUM(gas_used) FROM blocks")
                        .await
                        .unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("group_by_address", row_count),
            &row_count,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    let _result = duck_pool
                        .query("SELECT address, COUNT(*) FROM logs GROUP BY address ORDER BY 2 DESC LIMIT 10")
                        .await
                        .unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("abi_decode_sum", row_count),
            &row_count,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    let _result = duck_pool
                        .query(
                            "SELECT SUM(abi_uint_native(data, 0)) FROM logs WHERE selector = '0xddf252ad'",
                        )
                        .await
                        .unwrap();
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_scale_olap, bench_scaling_curve
);
criterion_main!(benches);
