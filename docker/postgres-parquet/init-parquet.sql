-- Initialize pg_parquet extension for Parquet COPY support
--
-- This extension enables COPY TO PARQUET for exporting data to columnar format.
-- OLAP queries are handled by tidx's native in-process DuckDB engine.

CREATE EXTENSION IF NOT EXISTS pg_parquet;

-- Grant usage on the data directory for Parquet exports
-- The tidx compress job writes Parquet files here
