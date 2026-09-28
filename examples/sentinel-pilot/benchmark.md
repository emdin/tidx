# Benchmark — 2026-09-28, production tidx (Igra 38833), via `/query`

Watchlist = the top-N WiKAS-active senders of the last 2M blocks
(`fixtures/bench_addresses.json`). Window ends at tip 17 962 686. "cold" = first
run, "warm" = immediate re-run. Page cap 10 000 (`PAGE_PLUS_1 = 10000`); a
`rows` of 10000 means the cap was hit and pagination is required.

| query | addrs | window | rows | cold ms | warm ms |
|---|---|---|---|---|---|
| q1 native   | 1  | 200k | 0     | 5     | 1     |
| q2 internal | 1  | 200k | 0     | 104   | 56    |
| q3 erc20    | 1  | 200k | 800   | 169   | 141   |
| q1 native   | 1  | 2M   | 0     | 2     | 2     |
| q2 internal | 1  | 2M   | 0     | 112   | 95    |
| q3 erc20    | 1  | 2M   | 8 500 | 580   | 200   |
| q1 native   | 10 | 200k | 250   | 187   | 17    |
| q2 internal | 10 | 200k | 497   | 136   | 207   |
| q3 erc20    | 10 | 200k | 5 317 | 240   | 223   |
| q1 native   | 10 | 2M   | 1 754 | 1 279 | 156   |
| q2 internal | 10 | 2M   | 4 650 | 2 118 | 1 978 |
| q3 erc20    | 10 | 2M   | **10 000 (cap)** | 1 479 | 562 |
| q1 native   | 50 | 200k | 265   | 97    | 25    |
| q2 internal | 50 | 200k | 533   | 171   | 211   |
| q3 erc20    | 50 | 200k | 5 537 | 228   | 215   |
| q1 native   | 50 | 2M   | 1 946 | 356   | 117   |
| q2 internal | 50 | 2M   | 5 091 | 2 026 | 2 087 |
| q3 erc20    | 50 | 2M   | **10 000 (cap)** | 833 | 812 |

q2 figures are for the `candidate_txs`-restricted query. Before that change q2
cost 640–940 ms cold for one address with zero rows (its reverted-frame CTE
walked the whole window); now 104–112 ms. For large result sets at 2M the
change is roughly neutral (10 addrs: 7 942 → 2 118 cold, 1 409 → 1 978 warm).

**Post-review re-measure (2026-09-28, warm)** after the review edits — `direction`
column on q1–q3, `amount > 0` on q3, `CREATE2` on q2 — same order of magnitude:

| query | addrs | window | rows | warm ms |
|---|---|---|---|---|
| q2 | 1  | 200k | 0     | 100   |
| q2 | 50 | 200k | 533   | 168   |
| q3 | 50 | 200k | 5 537 | 262   |
| q2 | 50 | 2M   | 5 091 | 2 322 |
| q3 | 50 | 2M   | cap   | 1 288 |

`q0d_internal_coverage.sql` (the interim trace-coverage signal) is **not** a
replay tool: 106 ms on a 200-block poll window, **27 s** on a 200k window
(it anti-joins every successful tx against `internal_txs`). Run it per poll
window, not per replay chunk.

## What this means for the bot

* **Recent-block polling (200k-block window ≈ 2.6 days, 50 addresses): every
  query ≤ 0.25 s, all three ≤ 0.6 s per poll.** Comfortably inside the 5 s
  default timeout; no pagination needed at these watchlist sizes.
* **Historical replay (2M window ≈ 25 days):** q2 reaches ~2 s and q3 hits the
  10 000-row cap for ≥10 addresses. Use `timeout_ms=30000`, page through, and
  prefer chunking replay into ≤200k-block windows — that also keeps each
  checkpoint advance small and cheap to redo after a reorg.
* Cost is driven by address activity, not watchlist length: 10 → 50 addresses
  barely moves the numbers, because the from/to indexes are per-address and
  the block bound is a post-filter (see README "Index note").
* No extra index or precomputation is required for the pilot at these sizes.
  The first index worth adding if a hot address appears is
  `logs(topic2, block_num)`.
