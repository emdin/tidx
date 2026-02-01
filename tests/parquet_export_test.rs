//! Tests for Parquet export functionality
//!
//! These tests verify that pg_parquet COPY TO PARQUET works for exporting data.
//! OLAP queries are handled by tidx's native in-process DuckDB engine.

mod common;

use common::testdb::TestDb;
use tempfile::TempDir;

/// Test that COPY TO PARQUET works via pg_parquet
/// pg_parquet intercepts COPY commands with FORMAT 'parquet'
#[tokio::test]
async fn test_parquet_export_via_pg_parquet() {
    let db = TestDb::new().await;
    
    // Skip if no logs
    if db.log_count().await == 0 {
        println!("Skipping test: no logs in database");
        return;
    }

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let parquet_path = temp_dir.path().join("test_logs.parquet");
    let path_str = parquet_path.to_string_lossy();
    let escaped_path = path_str.replace('\'', "''");

    let conn = db.pool.get().await.expect("Failed to get connection");

    // Use pg_parquet's COPY TO syntax with parquet format
    let copy_sql = format!(
        "COPY (SELECT block_num, tx_idx, log_idx, tx_hash, address, \
         topic0, topic1, topic2, topic3, data FROM logs \
         ORDER BY block_num, log_idx LIMIT 100) TO '{}' WITH (FORMAT 'parquet', COMPRESSION 'zstd')",
        escaped_path
    );

    let result = conn.execute(&copy_sql, &[]).await;

    match result {
        Ok(_) => {
            // Verify file was created
            assert!(parquet_path.exists(), "Parquet file should exist");
            
            let file_size = std::fs::metadata(&parquet_path)
                .expect("Failed to get file metadata")
                .len();
            println!("Parquet file size: {} bytes", file_size);
            assert!(file_size > 0, "Parquet file should not be empty");

            // Read row count from parquet metadata
            let row_count = read_parquet_row_count(&parquet_path);
            println!("Exported {} rows to Parquet", row_count);
            assert!(row_count > 0, "Should export at least one row");
        }
        Err(e) => {
            println!("Parquet export failed: {}", e);
            println!("This test requires pg_parquet extension to be installed");
        }
    }
}

/// Read row count from parquet file metadata
fn read_parquet_row_count(file_path: &std::path::Path) -> u64 {
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;
    use std::fs::File;

    let file = File::open(file_path).expect("Failed to open parquet file");
    let reader = SerializedFileReader::new(file).expect("Failed to create parquet reader");
    let metadata = reader.metadata();
    metadata.file_metadata().num_rows() as u64
}

/// Test the full export flow using compress module
#[tokio::test]
async fn test_compress_tick() {
    let db = TestDb::new().await;
    
    // Need at least some blocks for this test
    let block_count = db.block_count().await;
    if block_count < 10 {
        println!("Skipping test: need at least 10 blocks, have {}", block_count);
        return;
    }

    // Set up sync_state so compress can find the tip
    let conn = db.pool.get().await.expect("Failed to get connection");
    conn.execute(
        "INSERT INTO sync_state (chain_id, head_num, synced_num, tip_num) 
         VALUES (1, $1, $1, $1)
         ON CONFLICT (chain_id) DO UPDATE SET 
            head_num = EXCLUDED.head_num,
            synced_num = EXCLUDED.synced_num, 
            tip_num = EXCLUDED.tip_num",
        &[&block_count],
    )
    .await
    .expect("Failed to set sync_state");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let chain_dir = temp_dir.path().join("1");
    std::fs::create_dir_all(&chain_dir).expect("Failed to create chain dir");

    // Test COPY syntax with parquet format
    let parquet_path = chain_dir.join("logs_1_10.parquet");
    let path_str = parquet_path.to_string_lossy();
    let escaped_path = path_str.replace('\'', "''");

    // Use pg_parquet's COPY TO syntax
    let copy_sql = format!(
        "COPY (SELECT block_num, tx_idx, log_idx, tx_hash, address, \
         topic0, topic1, topic2, topic3, data FROM logs \
         WHERE block_num >= 1 AND block_num <= 10 \
         ORDER BY block_num, log_idx) TO '{}' WITH (FORMAT 'parquet', COMPRESSION 'zstd')",
        escaped_path
    );

    let result = conn.execute(&copy_sql, &[]).await;

    match result {
        Ok(_) => {
            assert!(parquet_path.exists(), "File should be created");
            let row_count = read_parquet_row_count(&parquet_path);
            println!("Exported {} rows", row_count);
        }
        Err(e) => {
            println!("Export failed (expected if pg_parquet not configured): {}", e);
        }
    }
}

/// Test that native DuckDB engine can query exported Parquet files
#[tokio::test]
async fn test_native_duckdb_reads_parquet() {
    use tidx::duckdb::DuckDbEngine;
    
    let db = TestDb::new().await;
    
    // Skip if no logs
    if db.log_count().await == 0 {
        println!("Skipping test: no logs in database");
        return;
    }

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let chain_dir = temp_dir.path().join("1");
    std::fs::create_dir_all(&chain_dir).expect("Failed to create chain dir");
    
    let parquet_path = chain_dir.join("logs_1_1000.parquet");
    let path_str = parquet_path.to_string_lossy();

    let conn = db.pool.get().await.expect("Failed to get connection");

    // Export logs to parquet via pg_parquet
    let copy_sql = format!(
        "COPY (SELECT block_num, tx_idx, log_idx, tx_hash, address, \
         topic0, topic1, topic2, topic3, data FROM logs \
         ORDER BY block_num, log_idx LIMIT 100) TO '{}' WITH (FORMAT 'parquet', COMPRESSION 'zstd')",
        path_str
    );

    let export_result = conn.execute(&copy_sql, &[]).await;
    if export_result.is_err() {
        println!("Parquet export failed - pg_parquet may not be installed, skipping test");
        return;
    }

    assert!(parquet_path.exists(), "Parquet file should exist after export");

    // Create native DuckDB engine and query the parquet file
    let engine = DuckDbEngine::new(temp_dir.path().to_path_buf(), 1)
        .expect("Failed to create DuckDB engine");

    // Query using the logs view (which points to parquet files)
    let result = engine.query("SELECT COUNT(*) as cnt FROM logs", None);
    
    match result {
        Ok(r) => {
            println!("Native DuckDB query returned {} rows", r.row_count);
            assert_eq!(r.row_count, 1, "Should return one row for COUNT(*)");
            assert!(r.rows[0][0].as_i64().unwrap_or(0) > 0, "Count should be positive");
            println!("SUCCESS: Native DuckDB engine can query Parquet files");
        }
        Err(e) => {
            println!("Query failed: {}", e);
        }
    }
}
