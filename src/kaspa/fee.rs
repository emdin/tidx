//! Fee computation for Kaspa L1 carrier txs.
//!
//! Split into two layers:
//!
//! 1. [`compute_fee_sompi`] — pure arithmetic, `sum(inputs) - sum(outputs)`.
//!    Extensively unit-tested; every conceivable edge case for the pipeline
//!    is exercised here so higher layers can trust the primitive.
//!
//! 2. [`FeeResolver`] — bridges the arithmetic to a running kaspad. For each
//!    `kaspa_txid` we already know is in the DB, it walks:
//!      a. Look up txid → block_hash in `kaspa_tx_index` (our own PG index).
//!      b. `get_block(hash, true)` to read the tx's inputs (has previous
//!         outpoints) + outputs (has amounts).
//!      c. For each input's previous outpoint, look up its containing block
//!         via the same index, then `get_block(hash, true)` to read the
//!         output at the referenced index → that's the input's amount.
//!      d. `compute_fee_sompi` on the collected numbers.
//!
//! kaspad's own RPC returns outputs with amounts but inputs only as
//! `previousOutpoint` references. api.kaspa.org has a `resolve_previous_outpoints`
//! flag that does this walking server-side; kaspad doesn't. That's the whole
//! reason `kaspa_tx_index` exists — it's the primitive kaspad is missing.

use std::collections::HashMap;
use std::future::Future;

use anyhow::{Result, anyhow, bail};
use kaspa_rpc_core::{RpcHash, RpcTransaction, api::rpc::RpcApi};

use crate::db::Pool;

/// Compute the miner fee in sompi given a Kaspa tx's inputs' amounts and
/// its outputs' amounts. Returns `None` if outputs exceed inputs — this
/// happens for coinbase txs (0 inputs, positive outputs from the block
/// subsidy) and would happen for an invalid non-coinbase tx (which we
/// treat as "can't compute" rather than assume the underflow).
///
/// Kaspa fees fit in u64 in practice (KAS supply is capped; sompi = KAS × 1e8;
/// u64 covers ~184 million KAS), so unchecked arithmetic on the sums is safe;
/// we still use `checked_sub` on the final subtraction as a belt-and-suspenders
/// against an inversion bug in the caller.
pub fn compute_fee_sompi(input_amounts: &[u64], output_amounts: &[u64]) -> Option<u64> {
    let sum_in: u64 = input_amounts.iter().sum();
    let sum_out: u64 = output_amounts.iter().sum();
    sum_in.checked_sub(sum_out)
}

/// Outcome of trying to resolve one tx's fee locally. Callers use this to
/// decide whether to persist a fee value, skip the row for now (retention
/// boundary), or record "no fee applicable" (coinbase). Structured — not a
/// bare `Option<u64>` — so the enricher can log why NULL was chosen and
/// dashboards can distinguish "no data yet" from "known coinbase".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeResolution {
    /// Miner fee in sompi.
    Fee(u64),
    /// Zero inputs → coinbase transaction. No fee is meaningful.
    Coinbase,
    /// Some txid in the resolution chain is not in `kaspa_tx_index`. This is
    /// expected at the retention boundary (kaspad no longer holds the block)
    /// or in a pre-tidx historical gap. The row stays NULL and can be
    /// re-enriched later if the index grows.
    NotInIndex,
}

/// Abstraction over "look up which block contains a given txid." Backed in
/// production by the PG `kaspa_tx_index` table; mockable in tests without
/// spinning up postgres.
pub trait TxLocator {
    fn locate(
        &self,
        txid: &[u8; 32],
    ) -> impl Future<Output = Result<Option<[u8; 32]>>> + Send;
}

/// Abstraction over "fetch a Kaspa block's transactions by hash." Backed in
/// production by a `KaspaRpcClient` speaking Borsh wRPC (calling
/// `get_block(hash, true)` and returning `.transactions`); mockable in tests
/// with an in-memory HashMap. Returning `Vec<RpcTransaction>` (not the full
/// `RpcBlock`) means tests don't have to construct block headers full of
/// merkle roots + blue-work — the resolver only ever reads `transactions`.
pub trait BlockFetcher {
    fn get_block_txs(
        &self,
        hash: RpcHash,
    ) -> impl Future<Output = Result<Vec<RpcTransaction>>> + Send;
}

/// Composes a `TxLocator` and a `BlockFetcher` into an end-to-end fee
/// computation. Holds no state; a fresh `FeeResolver` per resolution is
/// fine, but reusing one is also fine (cheap).
pub struct FeeResolver<'a, L: TxLocator, F: BlockFetcher> {
    locator: &'a L,
    fetcher: &'a F,
}

impl<'a, L: TxLocator, F: BlockFetcher> FeeResolver<'a, L, F> {
    pub fn new(locator: &'a L, fetcher: &'a F) -> Self {
        Self { locator, fetcher }
    }

    /// Resolve the miner fee for a single Kaspa tx.
    ///
    /// Returns:
    /// - `Ok(FeeResolution::Fee(n))` on success.
    /// - `Ok(FeeResolution::Coinbase)` if the tx has zero inputs.
    /// - `Ok(FeeResolution::NotInIndex)` if any txid in the resolution chain
    ///   isn't in `kaspa_tx_index` (retention or historical gap).
    /// - `Err` if kaspad or PG fails, OR if invariants are violated
    ///   (indexed tx not present in the referenced block; input outpoint
    ///   index out of bounds; non-coinbase with outputs > inputs).
    ///
    /// Blocks fetched during resolution are cached for the duration of ONE
    /// call: when a tx has multiple inputs spending outputs from the same
    /// previous block, we only pay the round-trip once. Not persisted
    /// across calls — kaspad's own block cache is where cross-call reuse
    /// happens.
    pub async fn resolve(&self, txid: &[u8; 32]) -> Result<FeeResolution> {
        // Step 1 — locate the tx's own block.
        let Some(block_hash) = self.locator.locate(txid).await? else {
            return Ok(FeeResolution::NotInIndex);
        };

        // Cache blocks fetched during this call. Small — typically 1-3 blocks
        // for a single fee resolution.
        let mut block_cache: HashMap<[u8; 32], Vec<RpcTransaction>> = HashMap::with_capacity(4);
        let owner_txs = self.fetch_cached(block_hash.into(), &mut block_cache).await?;
        let tx = find_tx_by_id(&owner_txs, txid).ok_or_else(|| {
            anyhow!(
                "txid {} indexed to block {} but not present in it",
                hex::encode(txid),
                hex::encode(block_hash)
            )
        })?;

        if tx.inputs.is_empty() {
            return Ok(FeeResolution::Coinbase);
        }

        // Step 2 — for each input, resolve its amount by looking up the prev
        // tx's block, finding the prev tx in it, and reading the output at
        // the referenced index.
        let output_amounts: Vec<u64> = tx.outputs.iter().map(|o| o.value).collect();
        let mut input_amounts: Vec<u64> = Vec::with_capacity(tx.inputs.len());

        // Clone what we need before dropping the borrow on owner_txs so
        // block_cache is free to reuse for prev-block fetches.
        let inputs: Vec<(RpcHash, u32)> = tx
            .inputs
            .iter()
            .map(|i| (i.previous_outpoint.transaction_id, i.previous_outpoint.index))
            .collect();
        drop(owner_txs); // release the clone from fetch_cached

        for (prev_txid, out_index) in inputs {
            let prev_txid_bytes = prev_txid.as_bytes();
            let Some(prev_block_hash) = self.locator.locate(&prev_txid_bytes).await? else {
                return Ok(FeeResolution::NotInIndex);
            };
            let prev_txs = self.fetch_cached(prev_block_hash.into(), &mut block_cache).await?;
            let prev_tx = find_tx_by_id(&prev_txs, &prev_txid_bytes).ok_or_else(|| {
                anyhow!(
                    "prev txid {} indexed to block {} but not present in it",
                    hex::encode(prev_txid_bytes),
                    hex::encode(prev_block_hash)
                )
            })?;
            let out = prev_tx.outputs.get(out_index as usize).ok_or_else(|| {
                anyhow!(
                    "input references output index {} on prev tx {} but it has only {} outputs",
                    out_index,
                    hex::encode(prev_txid_bytes),
                    prev_tx.outputs.len()
                )
            })?;
            input_amounts.push(out.value);
        }

        match compute_fee_sompi(&input_amounts, &output_amounts) {
            Some(fee) => Ok(FeeResolution::Fee(fee)),
            None => bail!(
                "non-coinbase tx {} has sum(outputs)={} > sum(inputs)={} — invalid",
                hex::encode(txid),
                output_amounts.iter().sum::<u64>(),
                input_amounts.iter().sum::<u64>(),
            ),
        }
    }

    /// Fetch a block's tx list, using and populating a per-call HashMap
    /// cache. The key is the raw 32-byte hash. Cloning `Vec<RpcTransaction>`
    /// is O(n) but n is small (~5 txs/block avg on Kaspa) — negligible next
    /// to the RPC round-trip we're avoiding.
    async fn fetch_cached(
        &self,
        hash: RpcHash,
        cache: &mut HashMap<[u8; 32], Vec<RpcTransaction>>,
    ) -> Result<Vec<RpcTransaction>> {
        let key = hash.as_bytes();
        if let Some(cached) = cache.get(&key) {
            return Ok(cached.clone());
        }
        let txs = self.fetcher.get_block_txs(hash).await?;
        cache.insert(key, txs.clone());
        Ok(txs)
    }
}

/// Scan a slice of transactions for one whose verbose_data.transaction_id
/// matches `wanted`. Returns None if not found or if verbose_data is missing
/// (which shouldn't happen — `get_block(_, include_transactions=true)` always
/// populates it — but we guard anyway).
fn find_tx_by_id<'b>(
    txs: &'b [RpcTransaction],
    wanted: &[u8; 32],
) -> Option<&'b RpcTransaction> {
    txs.iter().find(|tx| {
        tx.verbose_data
            .as_ref()
            .is_some_and(|v| v.transaction_id.as_bytes() == *wanted)
    })
}

/// Production `TxLocator` backed by the PG `kaspa_tx_index` table.
pub struct PgTxLocator<'a> {
    pool: &'a Pool,
}

impl<'a> PgTxLocator<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }
}

impl<'a> TxLocator for PgTxLocator<'a> {
    async fn locate(&self, txid: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        let conn = self.pool.get().await?;
        let row = conn
            .query_opt(
                "SELECT block_hash FROM kaspa_tx_index WHERE txid = $1",
                &[&txid.as_slice()],
            )
            .await?;
        let Some(row) = row else { return Ok(None) };
        let bytes: Vec<u8> = row.get(0);
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("kaspa_tx_index.block_hash has wrong length: {}", bytes.len()))?;
        Ok(Some(arr))
    }
}

/// Production `BlockFetcher` backed by a live `KaspaRpcClient`. Always
/// requests transaction bodies (`include_transactions=true`) — we need the
/// tx list to find our target tx by id.
pub struct KaspadBlockFetcher<'a> {
    client: &'a kaspa_wrpc_client::KaspaRpcClient,
}

impl<'a> KaspadBlockFetcher<'a> {
    pub fn new(client: &'a kaspa_wrpc_client::KaspaRpcClient) -> Self {
        Self { client }
    }
}

impl<'a> BlockFetcher for KaspadBlockFetcher<'a> {
    async fn get_block_txs(&self, hash: RpcHash) -> Result<Vec<RpcTransaction>> {
        let block = self.client.get_block(hash, true).await?;
        Ok(block.transactions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_tx_positive_fee() {
        // 2 inputs totalling 1500 sompi, 2 outputs totalling 1400, fee = 100.
        assert_eq!(compute_fee_sompi(&[1000, 500], &[1200, 200]), Some(100));
    }

    #[test]
    fn zero_fee_is_valid() {
        // Extremely rare in practice but arithmetically legit; must not
        // collapse to None. (Kaspa's mempool won't accept fee=0 txs from
        // most peers, but a self-relayed tx could still land at exactly
        // sum_in == sum_out if the sender chose so.)
        assert_eq!(compute_fee_sompi(&[1000], &[1000]), Some(0));
    }

    #[test]
    fn coinbase_returns_none() {
        // Coinbase: no inputs, output = block subsidy. sum_in - sum_out
        // underflows u64 — we return None. The caller uses this signal
        // to skip storing a fee for coinbase txs (there is no fee to
        // compute — the miner IS the recipient).
        assert_eq!(compute_fee_sompi(&[], &[50_000]), None);
    }

    #[test]
    fn empty_both_zero_fee() {
        // Degenerate; wouldn't appear in a real block but the arithmetic
        // is well-defined and returning Some(0) is more honest than
        // silently returning None.
        assert_eq!(compute_fee_sompi(&[], &[]), Some(0));
    }

    #[test]
    fn invalid_outputs_exceed_inputs_returns_none() {
        // A non-coinbase tx with outputs > inputs is invalid; a real
        // kaspad would never accept one. If we ever see it (bug in the
        // walker, malformed block, whatever) we return None rather than
        // wrap-around a garbage value into the DB.
        assert_eq!(compute_fee_sompi(&[100], &[200]), None);
    }

    #[test]
    fn single_input_single_output() {
        // Simplest real-world shape: one input 1_000_000 sompi, one
        // output 999_000, fee = 1000 sompi. Sanity check the fast path.
        assert_eq!(compute_fee_sompi(&[1_000_000], &[999_000]), Some(1000));
    }

    #[test]
    fn multi_input_multi_output_realistic_scale() {
        // Numbers roughly matching a real Igra L2 submission — 1 KAS input
        // (1e8 sompi), 0.99999 KAS output, 1000 sompi fee.
        let inputs = [100_000_000u64];
        let outputs = [99_999_000u64];
        assert_eq!(compute_fee_sompi(&inputs, &outputs), Some(1000));
    }

    #[test]
    fn no_overflow_at_realistic_supply_scale() {
        // 100 inputs at 1M KAS each = 1e14 sompi, still comfortably in u64.
        // Ensures the sum doesn't panic on typical shapes.
        let inputs = vec![100_000_000_000_000u64; 100];
        let outputs = vec![99_999_999_999_999u64; 100];
        assert_eq!(compute_fee_sompi(&inputs, &outputs), Some(100));
    }
}

// -----------------------------------------------------------------------
// Resolver tests — exercise the full FeeResolver::resolve flow against
// in-memory fake TxLocator + BlockFetcher. No real DB, no real kaspad.
// -----------------------------------------------------------------------
#[cfg(test)]
mod resolver_tests {
    use super::*;
    use kaspa_rpc_core::{
        RpcSubnetworkId, RpcTransactionInput, RpcTransactionOutpoint, RpcTransactionOutput,
        RpcTransactionVerboseData,
    };
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// In-memory `TxLocator` — feed it a HashMap of txid → block_hash.
    struct FakeLocator {
        map: HashMap<[u8; 32], [u8; 32]>,
    }

    impl TxLocator for FakeLocator {
        async fn locate(&self, txid: &[u8; 32]) -> Result<Option<[u8; 32]>> {
            Ok(self.map.get(txid).copied())
        }
    }

    /// In-memory `BlockFetcher` — feed it a HashMap of block_hash → tx list.
    /// Tracks call count via `AtomicU32` (Send-safe) so tests can assert the
    /// per-call block cache works.
    struct FakeFetcher {
        blocks: HashMap<[u8; 32], Vec<RpcTransaction>>,
        calls: AtomicU32,
    }

    impl BlockFetcher for FakeFetcher {
        async fn get_block_txs(&self, hash: RpcHash) -> Result<Vec<RpcTransaction>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.blocks.get(&hash.as_bytes()) {
                Some(txs) => Ok(txs.clone()),
                None => Err(anyhow!("fake fetcher: block {} not seeded", hash)),
            }
        }
    }

    fn h(tag: u8) -> [u8; 32] {
        let mut a = [0u8; 32];
        a[0] = tag;
        a
    }

    fn tx(txid_tag: u8, inputs: Vec<([u8; 32], u32)>, outputs: Vec<u64>) -> RpcTransaction {
        // ScriptPublicKey and RpcSubnetworkId both round-trip through serde,
        // and their Debug/Default impls exist through their base types. We
        // deserialize from an empty JSON to get zero values without pulling
        // kaspa_txscript / kaspa_consensus_core as direct deps.
        let empty_spk: kaspa_rpc_core::RpcScriptPublicKey =
            serde_json::from_str(r#"{"version":0,"script":""}"#)
                .expect("empty scriptPublicKey JSON should deserialize");

        RpcTransaction {
            version: 0,
            inputs: inputs
                .into_iter()
                .map(|(prev_txid, index)| RpcTransactionInput {
                    previous_outpoint: RpcTransactionOutpoint {
                        transaction_id: prev_txid.into(),
                        index,
                    },
                    signature_script: vec![],
                    sequence: 0,
                    sig_op_count: 0,
                    verbose_data: None,
                })
                .collect(),
            outputs: outputs
                .into_iter()
                .map(|value| RpcTransactionOutput {
                    value,
                    script_public_key: empty_spk.clone(),
                    verbose_data: None,
                })
                .collect(),
            lock_time: 0,
            subnetwork_id: RpcSubnetworkId::from_byte(0),
            gas: 0,
            payload: vec![],
            mass: 0,
            verbose_data: Some(RpcTransactionVerboseData {
                transaction_id: h(txid_tag).into(),
                hash: h(txid_tag).into(),
                compute_mass: 0,
                block_hash: h(0xff).into(),
                block_time: 0,
            }),
        }
    }

    fn block(_block_tag: u8, txs: Vec<RpcTransaction>) -> Vec<RpcTransaction> {
        // Kept for test readability — a "block" is just its tx list in this
        // simplified fake since the resolver only reads transactions.
        txs
    }

    #[tokio::test]
    async fn resolve_normal_two_input_tx_returns_fee() {
        // Prev tx A in block 0x10 with outputs [700, 300]. Target tx spends
        // A's output 0 (700). Prev tx B in block 0x11 with outputs [800].
        // Target tx spends B's output 0 (800). Target outputs [1400].
        // Fee = (700 + 800) - 1400 = 100.
        let prev_a = tx(0xAA, vec![], vec![700, 300]);
        let prev_b = tx(0xBB, vec![], vec![800]);
        let target = tx(0xCC, vec![(h(0xAA), 0), (h(0xBB), 0)], vec![1400]);

        let blocks = HashMap::from([
            (h(0x10), block(0x10, vec![prev_a])),
            (h(0x11), block(0x11, vec![prev_b])),
            (h(0x12), block(0x12, vec![target])),
        ]);
        let map = HashMap::from([
            (h(0xAA), h(0x10)),
            (h(0xBB), h(0x11)),
            (h(0xCC), h(0x12)),
        ]);
        let fetcher = FakeFetcher {
            blocks,
            calls: AtomicU32::new(0),
        };
        let locator = FakeLocator { map };
        let resolver = FeeResolver::new(&locator, &fetcher);

        let outcome = resolver.resolve(&h(0xCC)).await.unwrap();
        assert_eq!(outcome, FeeResolution::Fee(100));
    }

    #[tokio::test]
    async fn resolve_txid_not_in_index_returns_not_in_index() {
        // Target txid isn't in the locator → NotInIndex, no fetch made.
        let fetcher = FakeFetcher {
            blocks: HashMap::new(),
            calls: AtomicU32::new(0),
        };
        let locator = FakeLocator {
            map: HashMap::new(),
        };
        let resolver = FeeResolver::new(&locator, &fetcher);

        let outcome = resolver.resolve(&h(0xCC)).await.unwrap();
        assert_eq!(outcome, FeeResolution::NotInIndex);
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0, "should not fetch when txid unindexed");
    }

    #[tokio::test]
    async fn resolve_prev_txid_not_in_index_returns_not_in_index() {
        // Target IS indexed, but one of its inputs' prev txid isn't. The
        // owner block was fetched; we bail on the missing prev without an
        // error, since this is the retention boundary case.
        let target = tx(0xCC, vec![(h(0xAA), 0)], vec![100]);
        let blocks = HashMap::from([(h(0x12), block(0x12, vec![target]))]);
        let map = HashMap::from([(h(0xCC), h(0x12))]); // AA not indexed
        let fetcher = FakeFetcher {
            blocks,
            calls: AtomicU32::new(0),
        };
        let locator = FakeLocator { map };
        let resolver = FeeResolver::new(&locator, &fetcher);

        let outcome = resolver.resolve(&h(0xCC)).await.unwrap();
        assert_eq!(outcome, FeeResolution::NotInIndex);
    }

    #[tokio::test]
    async fn resolve_coinbase_zero_inputs_returns_coinbase() {
        // Target is coinbase (0 inputs). No prev lookups needed; return
        // Coinbase immediately.
        let target = tx(0xCC, vec![], vec![50_000_000_000]);
        let blocks = HashMap::from([(h(0x12), block(0x12, vec![target]))]);
        let map = HashMap::from([(h(0xCC), h(0x12))]);
        let fetcher = FakeFetcher {
            blocks,
            calls: AtomicU32::new(0),
        };
        let locator = FakeLocator { map };
        let resolver = FeeResolver::new(&locator, &fetcher);

        let outcome = resolver.resolve(&h(0xCC)).await.unwrap();
        assert_eq!(outcome, FeeResolution::Coinbase);
        assert_eq!(
            fetcher.calls.load(Ordering::SeqCst),
            1,
            "coinbase only needs the owner block"
        );
    }

    #[tokio::test]
    async fn resolve_indexed_but_block_missing_tx_returns_err() {
        // The index says txid CC is in block 0x12, but block 0x12 doesn't
        // contain a tx with that verbose_data.transaction_id. Invariant
        // violation → Err.
        let other = tx(0xDD, vec![], vec![100]);
        let blocks = HashMap::from([(h(0x12), block(0x12, vec![other]))]);
        let map = HashMap::from([(h(0xCC), h(0x12))]);
        let fetcher = FakeFetcher {
            blocks,
            calls: AtomicU32::new(0),
        };
        let locator = FakeLocator { map };
        let resolver = FeeResolver::new(&locator, &fetcher);

        let err = resolver.resolve(&h(0xCC)).await.unwrap_err();
        assert!(
            err.to_string().contains("not present"),
            "expected 'not present' error, got: {err}"
        );
    }

    #[tokio::test]
    async fn resolve_input_index_out_of_bounds_returns_err() {
        // Prev tx has only 1 output, target references output at index 5.
        // Invariant violation → Err.
        let prev = tx(0xAA, vec![], vec![100]);
        let target = tx(0xCC, vec![(h(0xAA), 5)], vec![50]);
        let blocks = HashMap::from([
            (h(0x10), block(0x10, vec![prev])),
            (h(0x12), block(0x12, vec![target])),
        ]);
        let map = HashMap::from([(h(0xAA), h(0x10)), (h(0xCC), h(0x12))]);
        let fetcher = FakeFetcher {
            blocks,
            calls: AtomicU32::new(0),
        };
        let locator = FakeLocator { map };
        let resolver = FeeResolver::new(&locator, &fetcher);

        let err = resolver.resolve(&h(0xCC)).await.unwrap_err();
        assert!(
            err.to_string().contains("output index"),
            "expected 'output index' error, got: {err}"
        );
    }

    #[tokio::test]
    async fn resolve_outputs_exceed_inputs_returns_err() {
        // Non-coinbase tx with 100 in / 200 out. compute_fee_sompi returns
        // None; resolver treats this as an error (invalid tx / bug), not
        // a silent Coinbase.
        let prev = tx(0xAA, vec![], vec![100]);
        let target = tx(0xCC, vec![(h(0xAA), 0)], vec![200]);
        let blocks = HashMap::from([
            (h(0x10), block(0x10, vec![prev])),
            (h(0x12), block(0x12, vec![target])),
        ]);
        let map = HashMap::from([(h(0xAA), h(0x10)), (h(0xCC), h(0x12))]);
        let fetcher = FakeFetcher {
            blocks,
            calls: AtomicU32::new(0),
        };
        let locator = FakeLocator { map };
        let resolver = FeeResolver::new(&locator, &fetcher);

        let err = resolver.resolve(&h(0xCC)).await.unwrap_err();
        assert!(
            err.to_string().contains("invalid"),
            "expected 'invalid' error, got: {err}"
        );
    }

    #[tokio::test]
    async fn resolve_shares_block_across_inputs_caches_fetch() {
        // Two inputs, both spending outputs from the same prev block.
        // Assert the fetcher is called only 2 times total: 1 for the
        // target's owner block, 1 for the shared prev block (NOT 3).
        let prev = tx(0xAA, vec![], vec![100, 200]); // two outputs
        let target = tx(0xCC, vec![(h(0xAA), 0), (h(0xAA), 1)], vec![250]);
        let blocks = HashMap::from([
            (h(0x10), block(0x10, vec![prev])),
            (h(0x12), block(0x12, vec![target])),
        ]);
        let map = HashMap::from([(h(0xAA), h(0x10)), (h(0xCC), h(0x12))]);
        let fetcher = FakeFetcher {
            blocks,
            calls: AtomicU32::new(0),
        };
        let locator = FakeLocator { map };
        let resolver = FeeResolver::new(&locator, &fetcher);

        let outcome = resolver.resolve(&h(0xCC)).await.unwrap();
        assert_eq!(outcome, FeeResolution::Fee(50)); // (100+200) - 250
        assert_eq!(
            fetcher.calls.load(Ordering::SeqCst),
            2,
            "should cache the shared prev block within one resolve call"
        );
    }

    #[tokio::test]
    async fn resolve_fetch_transport_error_propagates() {
        // The fetcher errors on the owner block. Resolver returns Err
        // (not swallowed to NotInIndex — this is a transport failure,
        // caller should retry, not persist NULL).
        let map = HashMap::from([(h(0xCC), h(0x12))]);
        let fetcher = FakeFetcher {
            blocks: HashMap::new(), // 0x12 not seeded → fake errors
            calls: AtomicU32::new(0),
        };
        let locator = FakeLocator { map };
        let resolver = FeeResolver::new(&locator, &fetcher);

        let err = resolver.resolve(&h(0xCC)).await.unwrap_err();
        assert!(
            err.to_string().contains("not seeded"),
            "expected fetcher's transport-style error to bubble up, got: {err}"
        );
    }
}
