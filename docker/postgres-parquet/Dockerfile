# PostgreSQL with pg_parquet for Parquet COPY support
# - pg_parquet: Native Parquet COPY support using PostgreSQL indexes
#
# Note: OLAP queries are handled by tidx's native in-process DuckDB engine,
# so pg_duckdb is no longer required.

ARG PG_VERSION=16
ARG PG_PARQUET_VERSION=v0.5.1
ARG PGRX_VERSION=0.16.0

# =============================================================================
# Stage 1: Build pg_parquet extension
# =============================================================================
FROM postgres:${PG_VERSION} AS parquet-builder

ARG PG_PARQUET_VERSION
ARG PGRX_VERSION

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    curl \
    git \
    pkg-config \
    libssl-dev \
    clang \
    libclang-dev \
    postgresql-server-dev-16 \
    libpq-dev \
    && rm -rf /var/lib/apt/lists/*

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

# Install cargo-pgrx (must match pg_parquet's pgrx version)
RUN cargo install cargo-pgrx --version "${PGRX_VERSION}" --locked

# Initialize pgrx with the system PostgreSQL
RUN cargo pgrx init --pg16=$(which pg_config)

# Clone and build pg_parquet
WORKDIR /build
RUN git clone --depth 1 --branch ${PG_PARQUET_VERSION} https://github.com/CrunchyData/pg_parquet.git
WORKDIR /build/pg_parquet
RUN cargo pgrx install --release --pg-config=$(which pg_config)

# Verify extension was installed correctly
RUN ls -la $(pg_config --sharedir)/extension | grep pg_parquet
RUN ls -la $(pg_config --pkglibdir) | grep pg_parquet

# =============================================================================
# Stage 2: Final image with pg_parquet
# =============================================================================
FROM postgres:${PG_VERSION}

# Copy pg_parquet extension files
COPY --from=parquet-builder /usr/share/postgresql/16/extension/pg_parquet* /usr/share/postgresql/16/extension/
COPY --from=parquet-builder /usr/lib/postgresql/16/lib/pg_parquet.so /usr/lib/postgresql/16/lib/

# Create init script to load extension
COPY init-parquet.sql /docker-entrypoint-initdb.d/

# Create data directory for Parquet files with proper permissions
RUN mkdir -p /data && chown postgres:postgres /data && chmod 755 /data
