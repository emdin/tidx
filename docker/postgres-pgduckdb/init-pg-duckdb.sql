-- Initialize PostgreSQL extensions for Ethereum indexing
-- This is run automatically when the PostgreSQL container starts
--
-- Extensions:
--   pg_duckdb: DuckDB columnar engine for OLAP queries
--   pg_parquet: Native Parquet COPY support (uses PostgreSQL indexes)
--   tidx_abi: Ethereum ABI decoding functions

-- Create extensions
CREATE EXTENSION IF NOT EXISTS pg_duckdb;
CREATE EXTENSION IF NOT EXISTS pg_parquet;

-- Allow loading unsigned extensions (tidx_abi is not signed)
ALTER SYSTEM SET duckdb.allow_unsigned_extensions = true;
SELECT pg_reload_conf();

-- Allow directories for DuckDB before loading extensions
-- Must be set before LOAD to allow access to extension path
SELECT duckdb.raw_query($$ SET allowed_directories TO ['/data', '/usr/share/duckdb/extensions'] $$);

-- Load tidx_abi extension into DuckDB for ABI decoding functions
-- This enables: abi_address(), abi_uint(), abi_uint256(), abi_bool(), abi_bytes32()
SELECT duckdb.raw_query($$ LOAD '/usr/share/duckdb/extensions/tidx_abi.duckdb_extension' $$);

-- Grant file write permissions for pg_parquet COPY TO
-- This allows the tidx role to export Parquet files to /data
GRANT pg_write_server_files TO tidx;

-- Note: pg_duckdb settings are applied per-session
-- The indexer sets these via SET commands in execute_query_pg_duckdb()
