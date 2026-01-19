use anyhow::Result;
use std::fmt::Write;
use std::pin::Pin;
use tokio_postgres::binary_copy::BinaryCopyInWriter;
use tokio_postgres::types::Type;

use crate::db::Pool;
use crate::types::{BlockRow, LogRow, SyncState, TxRow};

pub async fn write_block(pool: &Pool, block: &BlockRow) -> Result<()> {
    write_blocks(pool, &[block.clone()]).await
}

/// Batch insert multiple blocks in a single query
pub async fn write_blocks(pool: &Pool, blocks: &[BlockRow]) -> Result<()> {
    if blocks.is_empty() {
        return Ok(());
    }

    let conn = pool.get().await?;

    // Build multi-row VALUES clause
    let mut query = String::from(
        "INSERT INTO blocks (num, hash, parent_hash, timestamp, timestamp_ms, gas_limit, gas_used, miner, extra_data) VALUES ",
    );

    for (i, _block) in blocks.iter().enumerate() {
        if i > 0 {
            query.push_str(", ");
        }
        let base = i * 9;
        write!(
            &mut query,
            "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
            base + 1, base + 2, base + 3, base + 4, base + 5, base + 6, base + 7, base + 8, base + 9
        )?;
    }

    query.push_str(" ON CONFLICT (num) DO NOTHING");

    // Collect params - need to store values to extend lifetime
    let param_values: Vec<_> = blocks
        .iter()
        .flat_map(|b| {
            vec![
                &b.num as &(dyn tokio_postgres::types::ToSql + Sync),
                &b.hash,
                &b.parent_hash,
                &b.timestamp,
                &b.timestamp_ms,
                &b.gas_limit,
                &b.gas_used,
                &b.miner,
                &b.extra_data,
            ]
        })
        .collect();

    conn.execute(&query, &param_values).await?;

    Ok(())
}

/// Batch insert transactions using COPY for maximum throughput
pub async fn write_txs(pool: &Pool, txs: &[TxRow]) -> Result<()> {
    if txs.is_empty() {
        return Ok(());
    }

    let conn = pool.get().await?;

    // COPY doesn't support ON CONFLICT, so we use an unlogged staging table + INSERT SELECT
    // Using unlogged for speed since staging data is transient
    conn.execute(
        "CREATE UNLOGGED TABLE IF NOT EXISTS txs_staging (LIKE txs INCLUDING DEFAULTS)",
        &[],
    )
    .await?;

    conn.execute("TRUNCATE txs_staging", &[]).await?;

    // Binary COPY into staging table
    let types = &[
        Type::INT8,       // block_num
        Type::TIMESTAMPTZ, // block_timestamp
        Type::INT4,       // idx
        Type::BYTEA,      // hash
        Type::INT2,       // type
        Type::BYTEA,      // from
        Type::BYTEA,      // to
        Type::TEXT,       // value
        Type::BYTEA,      // input
        Type::INT8,       // gas_limit
        Type::TEXT,       // max_fee_per_gas
        Type::TEXT,       // max_priority_fee_per_gas
        Type::INT8,       // gas_used
        Type::BYTEA,      // nonce_key
        Type::INT8,       // nonce
        Type::BYTEA,      // fee_token
        Type::BYTEA,      // fee_payer
        Type::JSONB,      // calls
        Type::INT2,       // call_count
        Type::INT8,       // valid_before
        Type::INT8,       // valid_after
        Type::INT2,       // signature_type
    ];

    let sink = conn
        .copy_in(
            r#"COPY txs_staging (block_num, block_timestamp, idx, hash, type, "from", "to", value, input,
                gas_limit, max_fee_per_gas, max_priority_fee_per_gas, gas_used,
                nonce_key, nonce, fee_token, fee_payer, calls, call_count,
                valid_before, valid_after, signature_type) FROM STDIN BINARY"#,
        )
        .await?;

    let writer = BinaryCopyInWriter::new(sink, types);
    let mut pinned_writer: Pin<Box<BinaryCopyInWriter>> = Box::pin(writer);

    for tx in txs {
        pinned_writer
            .as_mut()
            .write(&[
                &tx.block_num,
                &tx.block_timestamp,
                &tx.idx,
                &tx.hash,
                &tx.tx_type,
                &tx.from,
                &tx.to,
                &tx.value,
                &tx.input,
                &tx.gas_limit,
                &tx.max_fee_per_gas,
                &tx.max_priority_fee_per_gas,
                &tx.gas_used,
                &tx.nonce_key,
                &tx.nonce,
                &tx.fee_token,
                &tx.fee_payer,
                &tx.calls,
                &tx.call_count,
                &tx.valid_before,
                &tx.valid_after,
                &tx.signature_type,
            ])
            .await?;
    }

    pinned_writer.as_mut().finish().await?;

    // Move from staging to main table, ignoring conflicts
    conn.execute(
        r#"INSERT INTO txs SELECT * FROM txs_staging ON CONFLICT (block_num, idx) DO NOTHING"#,
        &[],
    )
    .await?;

    Ok(())
}

/// Batch insert logs using COPY for maximum throughput
pub async fn write_logs(pool: &Pool, logs: &[LogRow]) -> Result<()> {
    if logs.is_empty() {
        return Ok(());
    }

    let conn = pool.get().await?;

    // COPY doesn't support ON CONFLICT, so we use an unlogged staging table + INSERT SELECT
    conn.execute(
        "CREATE UNLOGGED TABLE IF NOT EXISTS logs_staging (LIKE logs INCLUDING DEFAULTS)",
        &[],
    )
    .await?;

    conn.execute("TRUNCATE logs_staging", &[]).await?;

    // Binary COPY into staging table
    let types = &[
        Type::INT8,       // block_num
        Type::TIMESTAMPTZ, // block_timestamp
        Type::INT4,       // log_idx
        Type::INT4,       // tx_idx
        Type::BYTEA,      // tx_hash
        Type::BYTEA,      // address
        Type::BYTEA,      // selector
        Type::BYTEA_ARRAY, // topics
        Type::BYTEA,      // data
    ];

    let sink = conn
        .copy_in(
            "COPY logs_staging (block_num, block_timestamp, log_idx, tx_idx, tx_hash, address, selector, topics, data) FROM STDIN BINARY",
        )
        .await?;

    let writer = BinaryCopyInWriter::new(sink, types);
    let mut pinned_writer: Pin<Box<BinaryCopyInWriter>> = Box::pin(writer);

    for log in logs {
        pinned_writer
            .as_mut()
            .write(&[
                &log.block_num,
                &log.block_timestamp,
                &log.log_idx,
                &log.tx_idx,
                &log.tx_hash,
                &log.address,
                &log.selector,
                &log.topics,
                &log.data,
            ])
            .await?;
    }

    pinned_writer.as_mut().finish().await?;

    // Move from staging to main table, ignoring conflicts
    conn.execute(
        "INSERT INTO logs SELECT * FROM logs_staging ON CONFLICT (block_num, log_idx) DO NOTHING",
        &[],
    )
    .await?;

    Ok(())
}

pub async fn load_sync_state(pool: &Pool) -> Result<Option<SyncState>> {
    let conn = pool.get().await?;

    let row = conn
        .query_opt(
            "SELECT chain_id, head_num, synced_num FROM sync_state WHERE id = 1",
            &[],
        )
        .await?;

    Ok(row.map(|r| SyncState {
        chain_id: r.get::<_, i64>(0) as u64,
        head_num: r.get::<_, i64>(1) as u64,
        synced_num: r.get::<_, i64>(2) as u64,
    }))
}

pub async fn save_sync_state(pool: &Pool, state: &SyncState) -> Result<()> {
    let conn = pool.get().await?;

    conn.execute(
        r#"
        INSERT INTO sync_state (id, chain_id, head_num, synced_num, updated_at)
        VALUES (1, $1, $2, $3, NOW())
        ON CONFLICT (id) DO UPDATE SET
            chain_id = EXCLUDED.chain_id,
            head_num = EXCLUDED.head_num,
            synced_num = EXCLUDED.synced_num,
            updated_at = NOW()
        "#,
        &[
            &(state.chain_id as i64),
            &(state.head_num as i64),
            &(state.synced_num as i64),
        ],
    )
    .await?;

    Ok(())
}
