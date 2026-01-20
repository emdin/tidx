.PHONY: help up down logs seed build check test bench bench-gen bench-gen-compressed bench-compressed bench-open clean

.DEFAULT_GOAL := help

# Docker compose file selection
# Use DEVNET=1 for local devnet (TimescaleDB + Tempo node)
# Default uses production compose (external RPCs)
ifeq ($(DEVNET),1)
COMPOSE := docker compose -f docker/dev/docker-compose.yml
else
COMPOSE := docker compose -f docker/prod/docker-compose.yml
endif

# Default seed parameters
DURATION ?= 30
TPS ?= 100

# ============================================================================
# Environment
# ============================================================================

# Start all services (TimescaleDB + Tempo + Indexer)
up:
	@$(COMPOSE) up -d
ifeq ($(DEVNET),1)
	@echo "Waiting for TimescaleDB..."
	@until $(COMPOSE) exec -T timescaledb pg_isready -U ak47 -d ak47 > /dev/null 2>&1; do sleep 1; done
	@echo "✓ Ready."
endif

# Stop all services
down:
	@$(COMPOSE) down

# Tail indexer logs
logs:
	@$(COMPOSE) logs -f ak47

# ============================================================================
# Data
# ============================================================================

# Seed chain with transactions (uses dev mnemonic for pre-funded accounts)
seed:
	@echo "Seeding chain with $(TPS) TPS for $(DURATION) seconds..."
	@docker run --rm --network host ghcr.io/tempoxyz/tempo-bench:latest \
		run-max-tps --duration $(DURATION) --tps $(TPS) --accounts 10 \
		--target-urls http://localhost:8545 --disable-2d-nonces \
		--mnemonic "test test test test test test test test test test test junk"

# Heavy seed: ~1M+ txs with max variance (TIP-20, ERC-20, swaps, multicalls)
# Takes ~10 mins at 2000 TPS for 600 seconds
HEAVY_DURATION ?= 600
HEAVY_TPS ?= 2000
HEAVY_ACCOUNTS ?= 1000

seed-heavy:
	@echo "Heavy seeding: $(HEAVY_TPS) TPS for $(HEAVY_DURATION)s (~$$(($(HEAVY_TPS) * $(HEAVY_DURATION))) txs)"
	@docker run --rm --network host ghcr.io/tempoxyz/tempo-bench:latest \
		run-max-tps \
		--duration $(HEAVY_DURATION) \
		--tps $(HEAVY_TPS) \
		--accounts $(HEAVY_ACCOUNTS) \
		--target-urls http://localhost:8545 \
		--disable-2d-nonces \
		--mnemonic "test test test test test test test test test test test junk" \
		--tip20-weight 3 \
		--erc20-weight 2 \
		--swap-weight 2

# ============================================================================
# Build & Test
# ============================================================================

# Build Docker image
build:
	@$(COMPOSE) build ak47

# Run clippy lints
check:
	@cargo clippy --all-targets

# Devnet compose for tests
DEVNET_COMPOSE := docker compose -f docker/dev/docker-compose.yml

# Run tests (sequential execution due to shared DB)
test:
	@$(DEVNET_COMPOSE) up -d timescaledb tempo
	@echo "Waiting for TimescaleDB..."
	@until $(DEVNET_COMPOSE) exec -T timescaledb pg_isready -U ak47 -d postgres > /dev/null 2>&1; do sleep 1; done
	@$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d postgres -c "CREATE DATABASE ak47" > /dev/null 2>&1 || true
	@echo "Waiting for Tempo..."
	@until curl -s http://localhost:8545 -X POST -H "Content-Type: application/json" \
		-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | grep -q result; do sleep 1; done
	@echo "Running tests..."
	@DATABASE_URL=postgres://ak47:ak47@localhost:5433/ak47 RPC_URL=http://localhost:8545 \
		cargo test -- --test-threads=1 --nocapture

# Benchmark parameters
BENCH_TXS ?= 5000000
BENCH_ARTIFACT ?= .bench_seed.dump

# Check if benchmark data exists, restore from artifact or generate fresh
define check_bench_data
	@TX_COUNT=$$($(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47 -tAc "SELECT COUNT(*) FROM txs" 2>/dev/null || echo "0"); \
	if [ "$$TX_COUNT" -ge 1000000 ]; then \
		echo "Using existing data ($$TX_COUNT txs)"; \
	elif [ -f "$(BENCH_ARTIFACT)" ]; then \
		echo "Restoring from cached artifact..."; \
		$(MAKE) _bench_restore; \
	else \
		echo "No cached data found. Run 'make bench-gen' first, or seeding now..."; \
		$(MAKE) _bench_seed; \
	fi
endef

# Generate benchmark seed artifact (run once, reuse many times)
bench-gen:
	@echo "=== Generating benchmark seed artifact ==="
	@START_TIME=$$(date +%s); \
	$(DEVNET_COMPOSE) up -d timescaledb; \
	echo "Waiting for TimescaleDB..."; \
	until $(DEVNET_COMPOSE) exec -T timescaledb pg_isready -U ak47 -d postgres > /dev/null 2>&1; do sleep 1; done; \
	$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d postgres -c "DROP DATABASE IF EXISTS ak47_test WITH (FORCE)" > /dev/null 2>&1 || true; \
	$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d postgres -c "CREATE DATABASE ak47_test" > /dev/null; \
	echo "Seeding $(BENCH_TXS) synthetic transactions..."; \
	SEED_TXS=$(BENCH_TXS) DATABASE_URL=postgres://ak47:ak47@localhost:5433/ak47_test \
		cargo test --release --test seed_bench -- --ignored --nocapture; \
	echo "Dumping to artifact..."; \
	$(DEVNET_COMPOSE) exec -T timescaledb pg_dump -U ak47 -Fc ak47_test > $(BENCH_ARTIFACT); \
	END_TIME=$$(date +%s); \
	ELAPSED=$$((END_TIME - START_TIME)); \
	MINS=$$((ELAPSED / 60)); \
	SECS=$$((ELAPSED % 60)); \
	echo ""; \
	echo "=== Completed in $${MINS}m $${SECS}s ==="; \
	echo "Artifact: $(BENCH_ARTIFACT) ($$(du -h $(BENCH_ARTIFACT) | cut -f1))"; \
	$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47_test -c "SELECT COUNT(*) as blocks FROM blocks; SELECT COUNT(*) as txs FROM txs; SELECT COUNT(*) as logs FROM logs;"

# Generate compressed benchmark seed artifact (smaller file, for CI)
bench-gen-compressed:
	@echo "=== Generating compressed benchmark seed artifact ==="
	@START_TIME=$$(date +%s); \
	$(DEVNET_COMPOSE) up -d timescaledb; \
	echo "Waiting for TimescaleDB..."; \
	until $(DEVNET_COMPOSE) exec -T timescaledb pg_isready -U ak47 -d postgres > /dev/null 2>&1; do sleep 1; done; \
	$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d postgres -c "DROP DATABASE IF EXISTS ak47_test WITH (FORCE)" > /dev/null 2>&1 || true; \
	$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d postgres -c "CREATE DATABASE ak47_test" > /dev/null; \
	echo "Seeding $(BENCH_TXS) synthetic transactions..."; \
	SEED_TXS=$(BENCH_TXS) DATABASE_URL=postgres://ak47:ak47@localhost:5433/ak47_test \
		cargo test --release --test seed_bench -- --ignored --nocapture; \
	echo "Compressing hypertables..."; \
	$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47_test -c "SELECT compress_chunk(c) FROM show_chunks('blocks') c" > /dev/null 2>&1 || true; \
	$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47_test -c "SELECT compress_chunk(c) FROM show_chunks('txs') c" > /dev/null 2>&1 || true; \
	$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47_test -c "SELECT compress_chunk(c) FROM show_chunks('logs') c" > /dev/null 2>&1 || true; \
	$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47_test -c "SELECT compress_chunk(c) FROM show_chunks('receipts') c" > /dev/null 2>&1 || true; \
	echo "Dumping to artifact..."; \
	$(DEVNET_COMPOSE) exec -T timescaledb pg_dump -U ak47 -Fc ak47_test > $(BENCH_ARTIFACT); \
	END_TIME=$$(date +%s); \
	ELAPSED=$$((END_TIME - START_TIME)); \
	MINS=$$((ELAPSED / 60)); \
	SECS=$$((ELAPSED % 60)); \
	echo ""; \
	echo "=== Completed in $${MINS}m $${SECS}s ==="; \
	echo "Artifact: $(BENCH_ARTIFACT) ($$(du -h $(BENCH_ARTIFACT) | cut -f1))"; \
	$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47_test -c "SELECT COUNT(*) as blocks FROM blocks; SELECT COUNT(*) as txs FROM txs; SELECT COUNT(*) as logs FROM logs;"

# Internal: restore from artifact (fast)
_bench_restore:
	@$(DEVNET_COMPOSE) up -d timescaledb
	@until $(DEVNET_COMPOSE) exec -T timescaledb pg_isready -U ak47 -d postgres > /dev/null 2>&1; do sleep 1; done
	@$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d postgres -c "DROP DATABASE IF EXISTS ak47 WITH (FORCE)" > /dev/null 2>&1 || true
	@$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d postgres -c "CREATE DATABASE ak47" > /dev/null
	@cat $(BENCH_ARTIFACT) | $(DEVNET_COMPOSE) exec -T timescaledb pg_restore -U ak47 -d ak47 --no-owner --no-acl 2>/dev/null || true
	@TX_COUNT=$$($(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47 -tAc "SELECT COUNT(*) FROM txs"); \
	echo "Restored $$TX_COUNT txs from artifact"

# Internal: seed fresh (slow, used when no artifact exists)
_bench_seed:
	@$(DEVNET_COMPOSE) up -d timescaledb
	@echo "Waiting for TimescaleDB..."
	@until $(DEVNET_COMPOSE) exec -T timescaledb pg_isready -U ak47 -d postgres > /dev/null 2>&1; do sleep 1; done
	@$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d postgres -c "DROP DATABASE IF EXISTS ak47 WITH (FORCE)" > /dev/null 2>&1 || true
	@$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d postgres -c "CREATE DATABASE ak47" > /dev/null
	@echo "Seeding $(BENCH_TXS) synthetic transactions..."
	@SEED_TXS=$(BENCH_TXS) DATABASE_URL=postgres://ak47:ak47@localhost:5433/ak47 \
		cargo test --release --test seed_bench -- --ignored --nocapture
	@$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47 -c "SELECT COUNT(*) as blocks FROM blocks; SELECT COUNT(*) as txs FROM txs; SELECT COUNT(*) as logs FROM logs;"

# Run benchmarks (seeds 2M txs if data doesn't exist)
bench:
	@$(DEVNET_COMPOSE) up -d timescaledb tempo
	@sleep 2
	$(call check_bench_data)
	@echo "=== Running Query Benchmarks ==="
	@DATABASE_URL=postgres://ak47:ak47@localhost:5433/ak47 cargo bench --bench query_bench
	@echo "Report: target/criterion/report/index.html"

# Run benchmarks on compressed data (requires bench-gen-compressed artifact)
bench-compressed:
	@$(DEVNET_COMPOSE) up -d timescaledb tempo
	@sleep 2
	$(call check_bench_data)
	@echo "=== Running Query Benchmarks (Compressed) ==="
	@DATABASE_URL=postgres://ak47:ak47@localhost:5433/ak47 cargo bench --bench query_bench
	@echo "Report: target/criterion/report/index.html"

# Run benchmarks and open report
bench-open: bench
	@open target/criterion/report/index.html 2>/dev/null || xdg-open target/criterion/report/index.html 2>/dev/null || echo "Open target/criterion/report/index.html"

# Compare ak47 vs golden-axe sync performance
# Both index from the same live tempo chain
# Requires: golden-axe repo at ~/git/golden-axe
GOLDEN_AXE_DIR ?= $(HOME)/git/golden-axe
COMPARE_TXS ?= 1000000
COMPARE_TPS ?= 3000

bench-vs-golden-axe:
	@echo "============================================"
	@echo "=== ak47 vs golden-axe Sync Comparison ==="
	@echo "============================================"
	@echo ""
	@echo "=== Step 1: Starting Tempo node ==="
	@$(DEVNET_COMPOSE) up -d tempo timescaledb
	@until curl -sf http://localhost:8545 > /dev/null 2>&1; do sleep 1; done
	@echo "Tempo node ready"
	@echo ""
	@echo "=== Step 2: Seeding chain with $(COMPARE_TXS) txs ==="
	@docker run --rm --network host ghcr.io/tempoxyz/tempo-bench:latest \
		run-max-tps \
		--duration $$(($(COMPARE_TXS) / $(COMPARE_TPS))) \
		--tps $(COMPARE_TPS) \
		--accounts 1000 \
		--target-urls http://localhost:8545 \
		--disable-2d-nonces \
		--mnemonic "test test test test test test test test test test test junk"
	@CHAIN_HEAD=$$(curl -s http://localhost:8545 -X POST -H "Content-Type: application/json" \
		-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | jq -r '.result' | xargs printf "%d\n"); \
	echo "Chain seeded to block $$CHAIN_HEAD"
	@echo ""
	@echo "=== Step 3: Reset databases ==="
	@$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -c "DROP DATABASE IF EXISTS ak47" > /dev/null 2>&1 || true
	@$(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -c "CREATE DATABASE ak47" > /dev/null
	@dropdb --if-exists ga_bench 2>/dev/null || true
	@createdb ga_bench 2>/dev/null || true
	@psql ga_bench -f $(GOLDEN_AXE_DIR)/be/src/sql/schema.sql > /dev/null 2>&1
	@psql ga_bench -f $(GOLDEN_AXE_DIR)/be/src/sql/indexes.sql > /dev/null 2>&1
	@psql ga_bench -c "DELETE FROM config WHERE chain = 31337" > /dev/null 2>&1 || true
	@psql ga_bench -c "INSERT INTO config (chain, url, enabled, batch_size, concurrency, start_block) \
		VALUES (31337, 'http://localhost:8545', true, 100, 4, 0)" > /dev/null 2>&1
	@echo "Databases reset"
	@echo ""
	@echo "=== Step 4: Benchmark ak47 sync ==="
	@cargo build --release
	@echo "Starting ak47..."
	@START=$$(date +%s); \
	timeout 600 ./target/release/ak47 up \
		--rpc http://localhost:8545 \
		--db postgres://ak47:ak47@localhost:5433/ak47 2>&1 | head -100 || true; \
	END=$$(date +%s); \
	AK47_TIME=$$((END - START)); \
	AK47_TXS=$$($(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47 -tAc "SELECT COUNT(*) FROM txs" 2>/dev/null || echo "0"); \
	AK47_BLOCKS=$$($(DEVNET_COMPOSE) exec -T timescaledb psql -U ak47 -d ak47 -tAc "SELECT COUNT(*) FROM blocks" 2>/dev/null || echo "0"); \
	echo "ak47: $$AK47_TXS txs, $$AK47_BLOCKS blocks in $${AK47_TIME}s"; \
	echo "ak47: $$(echo "scale=0; $$AK47_TXS / $$AK47_TIME" | bc) txs/sec"; \
	echo "$$AK47_TIME $$AK47_TXS $$AK47_BLOCKS" > /tmp/ak47_result.txt
	@echo ""
	@echo "=== Step 5: Benchmark golden-axe sync ==="
	@cd $(GOLDEN_AXE_DIR) && cargo build --release -p be
	@echo "Starting golden-axe..."
	@START=$$(date +%s); \
	cd $(GOLDEN_AXE_DIR) && timeout 600 DATABASE_URL=postgres://localhost/ga_bench \
		./target/release/be 2>&1 | head -100 || true; \
	END=$$(date +%s); \
	GA_TIME=$$((END - START)); \
	GA_TXS=$$(psql ga_bench -tAc "SELECT COUNT(*) FROM txs" 2>/dev/null || echo "0"); \
	GA_BLOCKS=$$(psql ga_bench -tAc "SELECT COUNT(*) FROM blocks" 2>/dev/null || echo "0"); \
	echo "golden-axe: $$GA_TXS txs, $$GA_BLOCKS blocks in $${GA_TIME}s"; \
	echo "golden-axe: $$(echo "scale=0; $$GA_TXS / $$GA_TIME" | bc) txs/sec"; \
	echo "$$GA_TIME $$GA_TXS $$GA_BLOCKS" > /tmp/ga_result.txt
	@echo ""
	@echo "============================================"
	@echo "=== Results ==="
	@echo "============================================"
	@AK47=$$(cat /tmp/ak47_result.txt); GA=$$(cat /tmp/ga_result.txt); \
	AK47_TIME=$$(echo $$AK47 | cut -d' ' -f1); AK47_TXS=$$(echo $$AK47 | cut -d' ' -f2); \
	GA_TIME=$$(echo $$GA | cut -d' ' -f1); GA_TXS=$$(echo $$GA | cut -d' ' -f2); \
	echo "ak47:       $$AK47_TXS txs in $${AK47_TIME}s ($$(echo "scale=0; $$AK47_TXS / $$AK47_TIME" | bc) txs/sec)"; \
	echo "golden-axe: $$GA_TXS txs in $${GA_TIME}s ($$(echo "scale=0; $$GA_TXS / $$GA_TIME" | bc) txs/sec)"; \
	if [ $$AK47_TIME -lt $$GA_TIME ]; then \
		SPEEDUP=$$(echo "scale=1; $$GA_TIME / $$AK47_TIME" | bc); \
		echo ""; \
		echo "ak47 is $${SPEEDUP}x faster"; \
	else \
		SPEEDUP=$$(echo "scale=1; $$AK47_TIME / $$GA_TIME" | bc); \
		echo ""; \
		echo "golden-axe is $${SPEEDUP}x faster"; \
	fi

# Clean everything
clean:
	@$(COMPOSE) down -v
	@cargo clean

# ============================================================================
# Help
# ============================================================================

help:
	@echo "ak47 Development"
	@echo ""
	@echo "  DEVNET=1 make <cmd>    Use devnet compose (TimescaleDB + local Tempo)"
	@echo ""
	@echo "  make up                Start services (use DEVNET=1 for local devnet)"
	@echo "  make down              Stop all services"
	@echo "  make logs              Tail indexer logs"
	@echo "  make build             Build Docker image"
	@echo ""
	@echo "  make test              Run tests (uses devnet)"
	@echo "  make check             Run clippy lints"
	@echo ""
	@echo "  make seed              Generate transactions (DURATION=30 TPS=100)"
	@echo "  make seed-heavy        Generate ~1M+ txs with max variance"
	@echo ""
	@echo "  make bench             Run benchmarks (restores from artifact)"
	@echo "  make bench-compressed  Run benchmarks on compressed data"
	@echo "  make bench-gen         Generate 20M tx seed artifact (run once)"
	@echo ""
	@echo "  make clean             Stop services and clean"
