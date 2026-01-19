# AK47 Implementation Plan

## Project Structure

```
ak47/
├── Cargo.toml
├── Dockerfile                  # Multi-stage build
├── docker-compose.yml          # Full stack (server + db)
├── docker-compose.test.yml     # Test environment (fast ephemeral DB)
│
├── src/
│   ├── main.rs                 # CLI entrypoint
│   ├── lib.rs                  # Library root (for benchmarks/tests)
│   │
│   ├── cli/                    # CLI commands
│   │   ├── mod.rs
│   │   ├── up.rs
│   │   ├── down.rs
│   │   ├── status.rs
│   │   ├── query.rs
│   │   └── sync.rs
│   │
│   ├── config.rs               # Config + network definitions
│   ├── types.rs                # Shared types
│   │
│   ├── sync/                   # Sync engine
│   │   ├── mod.rs
│   │   ├── coordinator.rs      # Job scheduling
│   │   ├── fetcher.rs          # RPC fetching
│   │   ├── decoder.rs          # Tempo tx decoding
│   │   ├── reorg.rs            # Reorg detection
│   │   ├── writer.rs           # DB writer (COPY protocol)
│   │   └── jobs.rs             # Job queue
│   │
│   ├── db/                     # Database layer
│   │   ├── mod.rs
│   │   ├── pool.rs             # Connection pool
│   │   ├── schema.rs           # Schema management
│   │   └── partitions.rs       # Dynamic partition creation
│   │
│   ├── query/                  # Query engine
│   │   ├── mod.rs
│   │   ├── api.rs              # HTTP API
│   │   ├── parser.rs           # Signature parsing
│   │   ├── abi.rs              # ABI decoding + CTE generation
│   │   └── ratelimit.rs        # Rate limiting
│   │
│   └── tempo/                  # Tempo protocol primitives
│       ├── mod.rs
│       ├── transaction.rs      # Tx type 0x76
│       ├── block.rs
│       ├── receipt.rs
│       └── signature.rs        # Multi-sig types
│
├── migrations/                 # SQL migrations
│   ├── 001_blocks.sql
│   ├── 002_txs.sql
│   ├── 003_logs.sql
│   ├── 004_sync_state.sql
│   └── 005_rate_limits.sql
│
├── benches/                    # Criterion benchmarks
│   ├── query_bench.rs          # Query latency benchmarks
│   ├── write_bench.rs          # DB write throughput
│   ├── decode_bench.rs         # Tempo tx decoding
│   └── e2e_bench.rs            # End-to-end sync benchmarks
│
├── tests/                      # Integration tests
│   ├── common/
│   │   ├── mod.rs
│   │   ├── fixtures.rs         # Test data generators
│   │   └── testdb.rs           # Ephemeral DB setup
│   ├── sync_test.rs
│   ├── query_test.rs
│   └── reorg_test.rs
│
├── fixtures/                   # Test fixtures
│   ├── blocks/                 # Sample block JSON
│   ├── txs/                    # Sample Tempo tx data
│   └── golden_axe_baseline.json # Performance baselines
│
├── scripts/
│   ├── install.sh              # Curl installer
│   ├── bench.sh                # Run benchmarks
│   └── test-fast.sh            # Fast test runner
│
└── docs/
    ├── ARCHITECTURE.md
    └── IMPLEMENTATION_PLAN.md
```

---

## Cargo.toml

```toml
[package]
name = "ak47"
version = "0.1.0"
edition = "2024"
license = "MIT"
repository = "https://github.com/tempoxyz/ak47"
description = "High-throughput Tempo blockchain indexer"

[[bin]]
name = "ak47"
path = "src/main.rs"

[lib]
name = "ak47"
path = "src/lib.rs"

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# HTTP
axum = "0.8"
reqwest = { version = "0.12", features = ["json"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["cors", "trace"] }

# Database
tokio-postgres = { version = "0.7", features = ["with-chrono-0_4"] }
deadpool-postgres = "0.14"
refinery = { version = "0.8", features = ["tokio-postgres"] }

# Ethereum/Tempo primitives
alloy = { version = "1", features = ["full"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# CLI
clap = { version = "4", features = ["derive"] }

# Observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
metrics = "0.24"
metrics-exporter-prometheus = "0.16"

# Crypto
sha3 = "0.10"

# Error handling
thiserror = "2"
anyhow = "1"

# Rate limiting
governor = "0.7"

# Testing & Benchmarks
criterion = { version = "0.5", features = ["async_tokio", "html_reports"] }
proptest = "1"
test-case = "3"
rand = "0.8"
hex-literal = "0.4"

[[bench]]
name = "query_bench"
harness = false

[[bench]]
name = "write_bench"
harness = false

[[bench]]
name = "decode_bench"
harness = false

[[bench]]
name = "e2e_bench"
harness = false

[profile.bench]
debug = true  # Enable debug symbols for profiling

[profile.test]
opt-level = 1  # Faster test builds with some optimization
```

---

## Phase 1: Core Infrastructure

### Task 1.1: Project Scaffolding
```bash
cargo init ak47
mkdir -p src/{cli,sync,db,query,tempo}
mkdir -p migrations benches tests/common fixtures
```

### Task 1.2: Tempo Protocol Primitives

```rust
// src/tempo/transaction.rs

use alloy::primitives::{Address, Bytes, B256, U256};

/// Tempo transaction type identifier
pub const TEMPO_TX_TYPE: u8 = 0x76;

/// Signature types supported by Tempo
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignatureType {
    Secp256k1 = 0,
    P256 = 1,
    WebAuthn = 2,
}

/// A single call within a Tempo batch transaction
#[derive(Debug, Clone)]
pub struct Call {
    pub to: Option<Address>,  // None = CREATE
    pub value: U256,
    pub input: Bytes,
}

impl Call {
    pub fn selector(&self) -> Option<[u8; 4]> {
        if self.input.len() >= 4 {
            Some(self.input[..4].try_into().unwrap())
        } else {
            None
        }
    }
}

/// Tempo native transaction (type 0x76)
#[derive(Debug, Clone)]
pub struct TempoTransaction {
    pub chain_id: u64,
    
    // Batch calls
    pub calls: Vec<Call>,
    
    // 2D nonce system
    pub nonce_key: U256,
    pub nonce: u64,
    
    // Gas
    pub gas_limit: u64,
    pub max_priority_fee_per_gas: u128,
    pub max_fee_per_gas: u128,
    
    // Time windows
    pub valid_before: Option<u64>,
    pub valid_after: Option<u64>,
    
    // Fee sponsorship
    pub fee_token: Option<Address>,
    pub fee_payer: Option<Address>,
    
    // Signature
    pub signature_type: SignatureType,
    pub signature: Bytes,
}
```

---

## Phase 4: CLI

### Task 4.1: CLI Structure

```rust
// src/main.rs

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ak47")]
#[command(about = "High-throughput Tempo blockchain indexer")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the indexer
    Up {
        /// RPC URLs (auto-detects chain IDs)
        #[arg(value_name = "RPC_URL")]
        rpcs: Vec<String>,
        
        /// PostgreSQL connection URL
        #[arg(long, env = "DATABASE_URL")]
        db: Option<String>,
        
        /// API server port
        #[arg(long, default_value = "8080")]
        port: u16,
    },
    
    /// Stop the indexer
    Down,
    
    /// Show sync status
    Status {
        #[arg(short, long)]
        watch: bool,
    },
    
    /// Execute a query
    Query {
        /// SQL query or helper expression
        query: String,
        
        #[arg(short, long)]
        chain: Option<u64>,
        
        #[arg(short, long)]
        format: Option<OutputFormat>,
    },
    
    /// Sync control
    Sync(SyncCommands),
}

#[derive(Subcommand)]
enum SyncCommands {
    Forward {
        #[arg(long)]
        from: Option<u64>,
        #[arg(long)]
        to: Option<String>,  // number or "tip"
    },
    Backward {
        #[arg(long)]
        from: Option<String>,  // number or "tip"
        #[arg(long)]
        to: Option<u64>,
    },
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    
    match cli.command {
        Commands::Up { rpcs, db, port } => {
            commands::up::run(rpcs, db, port).await
        }
        Commands::Down => commands::down::run().await,
        Commands::Status { watch } => commands::status::run(watch).await,
        Commands::Query { query, chain, format } => {
            commands::query::run(query, chain, format).await
        }
        Commands::Sync(cmd) => commands::sync::run(cmd).await,
    }
}
```

### Task 4.2: Up Command

```rust
// src/cli/up.rs

pub async fn run(rpcs: Vec<String>, db: Option<String>, port: u16) -> Result<()> {
    // Require database URL
    let db_url = db.ok_or_else(|| anyhow!(
        "Database URL required. Set --db or DATABASE_URL env var.\n\
         Example: ak47 up https://rpc.tempo.xyz --db postgres://ak47:ak47@localhost:5432/ak47\n\n\
         To start a local TimescaleDB:\n\
         docker compose up -d"
    ))?;
    
    // Load or create config
    let config_path = dirs::home_dir().unwrap().join(".ak47/config.toml");
    let mut config = Config::load_or_default(&config_path)?;
    config.database_url = db_url.clone();
    
    // Auto-detect chain IDs from RPC URLs
    if !rpcs.is_empty() {
        config.chains.clear();
        for rpc in &rpcs {
            let chain_id = fetch_chain_id(rpc).await?;
            let name = detect_chain_name(chain_id);
            config.chains.push(ChainConfig { id: chain_id, name, rpc: rpc.clone() });
        }
        config.save(&config_path)?;
        println!("Saved config to {}", config_path.display());
    }
    
    if config.chains.is_empty() {
        return Err(anyhow!("No chains configured. Run: ak47 up <rpc_url> --db <pg_url>"));
    }
    
    // Connect to database
    println!("Connecting to database...");
    let pool = create_pool(&db_url).await?;
    
    // Run migrations
    println!("Running migrations...");
    run_migrations(&pool).await?;
    
    // Start sync coordinator
    println!("Starting sync for {} chain(s)...", config.chains.len());
    let coordinator = SyncCoordinator::new(pool.clone(), config.chains.clone());
    tokio::spawn(coordinator.run());
    
    // Start API server
    println!("\n✓ AK47 is running!");
    println!("  API: http://localhost:{}", port);
    println!("  DB:  {}", redact_password(&db_url));
    println!("\nChains:");
    for chain in &config.chains {
        println!("  {} ({}): {}", chain.name, chain.id, chain.rpc);
    }
    println!("\nRun 'ak47 status' to check sync progress");
    
    let api = query::api::router(pool);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    axum::serve(listener, api).await?;
    
    Ok(())
}

async fn fetch_chain_id(rpc: &str) -> Result<u64> {
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(rpc)
        .json(&json!({"jsonrpc": "2.0", "method": "eth_chainId", "params": [], "id": 1}))
        .send()
        .await?
        .json()
        .await?;
    
    let hex = resp["result"].as_str().ok_or_else(|| anyhow!("Invalid response"))?;
    Ok(u64::from_str_radix(hex.trim_start_matches("0x"), 16)?)
}

fn detect_chain_name(chain_id: u64) -> String {
    match chain_id {
        4217 => "tempo-mainnet".to_string(),
        42429 => "tempo-testnet".to_string(),
        42431 => "tempo-moderato".to_string(),
        _ => format!("chain-{}", chain_id),
    }
}
```

### Task 4.3: Status Command

```rust
// src/cli/status.rs

pub async fn run(watch: bool) -> Result<()> {
    loop {
        let status = fetch_status().await?;
        
        if watch {
            print!("\x1B[2J\x1B[1;1H");  // Clear screen
        }
        
        println!("AK47 Indexer Status");
        println!("═══════════════════\n");
        
        println!("{:15} │ {:>10} │ {:>10} │ {:>10} │ {:>10} │ {:>8}",
            "Network", "Forward", "Backward", "Gap", "Head", "Lag");
        println!("{}", "─".repeat(80));
        
        for chain in &status.chains {
            println!("{:15} │ {:>10} │ {:>10} │ {:>10} │ {:>10} │ {:>8}",
                chain.name,
                chain.forward_cursor,
                chain.backward_cursor.map(|n| n.to_string()).unwrap_or("-".into()),
                chain.gap().map(|n| n.to_string()).unwrap_or("-".into()),
                chain.head,
                format_lag(chain.lag()));
        }
        
        if !watch {
            break;
        }
        
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    
    Ok(())
}
```

---

## Query API

```rust
// src/query/api.rs

async fn handle_query(
    State(state): State<AppState>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>> {
    // Check rate limit
    if !state.rate_limiter.check(&req.api_key).await? {
        return Err(Error::RateLimited);
    }
    
    // Validate SQL (SELECT only)
    validate_sql(&req.sql)?;
    
    // Execute with timeout
    let result = tokio::time::timeout(
        Duration::from_millis(req.timeout_ms.unwrap_or(200)),
        state.db.query(&req.sql, &[])
    ).await??;
    
    Ok(Json(QueryResponse { rows: result }))
}

async fn query_logs(
    State(state): State<AppState>,
    Path(signature): Path<String>,
    Query(params): Query<LogQueryParams>,
) -> Result<Json<QueryResponse>> {
    // JIT: signature -> selector (Golden Axe pattern)
    let schema = Schema::parse(&signature)?;
    
    // Generate CTE query (uses selector index, no physical tables)
    let cte_sql = schema.to_cte_sql();
    let sql = format!(
        "WITH {} SELECT * FROM {} WHERE block_timestamp > $1 LIMIT $2",
        cte_sql,
        schema.name
    );
    
    let rows = state.db.query(&sql, &[
        &params.after,
        &params.limit.unwrap_or(100),
    ]).await?;
    
    Ok(Json(QueryResponse { rows }))
}
```
