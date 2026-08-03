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
