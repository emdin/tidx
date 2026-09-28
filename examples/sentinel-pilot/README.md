# Sentinel pilot — tidx SQL for watched-wallet movements (Igra, chain 38833)

Deliverable for the "Minimal pilot" spec (8 Sep 2026). Three parameterized,
per-source SQL queries against tidx's existing `/query` endpoint — **no new
service, no materialized view** — plus cutoff/reorg helpers, a runner that
preserves exact inputs and outputs, and acceptance fixtures on real Igra
transactions (one local-chain fixture where Igra has no real example).

Everything here was executed against production tidx on 2026-09-28. Every
claim about tidx behaviour below was verified on that date; where something is
inferred it says so.

```
sql/q1_native.sql          top-level native iKAS transfers      (txs ⋈ receipts)
sql/q2_internal.sql        internal native transfers via traces (internal_txs), reverted-subtree-safe
sql/q3_erc20.sql           ERC-20 Transfer events, all tokens   (logs), NFT-excluded
sql/q0a_cutoff_hash.sql    pin the complete-indexed cutoff
sql/q0b_pin_check.sql      reorg detection on resume; also fetches TS_LO
sql/q0c_orphaned_range.sql what a reorg invalidated (for corrections)
fixtures/run.py            substitute → validate → POST /query → save input+output
fixtures/*.params.json     exact inputs;  fixtures/*.output.json  exact responses
config.example.json        watchlist / token decimals / contract classification
```

## How the bot uses it

1. **Cutoff.** `GET /status` → `chains[0].synced_num`, `gap_blocks`, `gaps[]`.
   `synced_num` is the highest block below which tidx has verified the
   `blocks` table contiguous. Pin it: `q0a` → store `(cutoff, cutoff_hash)`.
   **Never scan past `synced_num`.** If `gap_blocks > 0`, the window is not
   complete — report it, don't scan into it.
2. **Window.** `[checkpoint+1, cutoff]`, capped at `max_window_blocks`
   (200 000 recommended — see benchmark). `TS_LO` = `timestamp` of `BLOCK_LO`
   via `q0b`. `TS_LO` is only an index-friendly prefilter; the exact bound is
   always `block_num`.
3. **Run q1, q2, q3** with the same window and watchlist, each paginated to
   completion (below) **before** advancing the checkpoint.
4. **Recipients.** Every `to_address` that is not a known contract joins the
   *off-chain* watchlist. Then **rescan that address from its first observed
   receipt through the pinned cutoff** (same window logic, watchlist = the new
   address) so movements it made *later in the same block or before the
   cutoff* are not missed. Rescans overlap earlier scans by construction —
   deduplicate on the key below.
5. **Classify** `called_contract`/`selector` against `config.contracts`:
   exit, Hyperlane router, known pool/DEX, else *unknown* → all of these are
   review flags. A designation is **never** propagated through a pool or
   exchange to its other users; the bot adds only the direct counterparty.
6. Advance the checkpoint to the cutoff. Persist `(checkpoint, hash)`.

### Pagination (deterministic, keyset)
Order is `(block_num, tx_index, log_index)` for q3, `(…, trace_path)` for q2,
`(block_num, tx_index)` for q1. Request `PAGE_PLUS_1 = page_size + 1`:

* `row_count == page_size + 1` → **more pages**; cursor = keys of row
  `page_size` (the last one you keep); discard the sentinel row.
* `row_count <= page_size` → last page.

The `/query` response has **no `has_more` field** — this sentinel is the only
signal. Never use `OFFSET`. Fixture F8 proves `page1 + page2 == one 10-row
query`, no overlap; F9 proves an identical re-run returns identical rows.

### Failure is never an empty page
`ok:false` (timeout, validator rejection — these come back as **HTTP 4xx with a
JSON body**, not 200 — row-cap violation, transport error) → `run.py` exits 1
and records it verbatim. Treat any non-ok as "retry this page", never as
"nothing happened". Limits: default `timeout_ms` 5000 (max 30000); page cap
10 000 (Postgres) — `LIMIT 10001` is rejected loudly.

### Dedupe key (restart-safe alerts)
`(chain_id, tx_hash, event_kind, log_index | trace_path)`. Store it with the
alert; rescans and restarts re-emit the same rows (F9) and must be idempotent.

### Reorg / gap
Before each poll, `q0b` on the stored checkpoint block: hash equal → continue;
different → replay from the fork point (`reorgs.fork_point`), enumerate what
was invalidated via `q0c` and by re-pointing q1–q3 at `orphaned_txs` /
`orphaned_internal_txs` / `orphaned_logs` (identical columns), and issue
**corrections** for alerts in that range. Igra has recorded **0 reorgs** to
date (F10); the path is exercised but has no live example.

## Correctness rules and where each is enforced

| Rule (spec) | Enforcement | Verified |
|---|---|---|
| Committed only | q1/q2 join `receipts` and require `status = 1`. q3 does not: **logs exist only for successful txs** | 0 logs in 1 124 reverted txs (300k-block window) |
| Reverted internal subtrees excluded | q2 CTE `reverted`: each reverted frame's DFS-preorder interval `(start_p, end_p)`; frames inside any are excluded. **`error IS NULL` alone is wrong** — descendants of a reverted frame carry `NULL` | 863 of 909 reverted parents had error-free children; fixture F4 |
| DELEGATECALL value is not a transfer | q2 `call_type IN ('CALL','CREATE','SELFDESTRUCT')` | 25 DELEGATECALL frames with value>0 per 300k blocks |
| No top-level/internal double count | `txs` = depth 0, `internal_txs` = depth ≥ 1, disjoint by construction | schema |
| Bridge event not counted twice | an exit is one q1 row (native → exit contract); its internal frames are contract→contract, so q2 matches nothing for the watched sender | F7 |
| ERC-20 ≠ NFT | q3 `topic3 IS NULL` | 273 407 ERC-20 vs 6 656 NFT events, clean split |
| Exact amounts | `amount_raw` is text (`format_uint` for logs; `abi_uint` would NULL ≥ 2^96 through the JSON layer) | — |
| Unknown decimals stay unknown | q3 returns `decimals = NULL`; bot maps by `token_address` from config (WiKAS = 18 via `eth_call`) | — |

## Acceptance fixtures (all with full hashes; inputs/outputs in `fixtures/`)

| # | Case | Fixture | Result |
|---|---|---|---|
| F1 | seed → recipient → next recipient **between two polls** | seed `0x7826f542…cbe45`. Poll 1 `[17959300,17959360]`: WiKAS **277.36** → `0x13f197c7…0bd5` in `0xf210f33312bcd4b1fb65ed850022ebe5548820f13ab07befd88f4390f4358c2b` (blk 17959343). Recipient rescan `[17959343,17959360]` → only that inbound. Poll 2 `[17959361,17960200]` with both watched: `0x13f1…` → `0xc281cb25…14d7` **398.59** in `0xa8fdae0b11134066987a35f0562c43bc854cc0bb1854a5a9be975a0e361120d5` (17959374) and **186.83** in `0x553cc102008302d5c9cf833002608959daebb6b4432260684c0712b1f2a0505c` (17960101) | ✅ forwarding caught in poll 2; every leg has `called_contract = 0xa5b0946d…`, `selector 0x38ed1739` → **review flag** (router). Counter-legs of another token appear too (`token_address` distinguishes) |
| F2 | small positive transfer | `0xaeb835081aa881a5cc86dc761cbf300fe7eae0cf7d36031d0d08fef84d335af6` (17315165), **105 raw** WiKAS from `0xe3ec9732…25b8` | ✅ returned |
| F3 | failed transaction | `0x7b8ed91308ef638f2fbc53ed338d4a47b09734047b4e6b699c67ddbb28c3c48b` (17886245), 2 885 iKAS, `status = 0` | ✅ q1 returns **0 rows**; control query confirms the tx exists |
| F4 | successful tx, reverted internal call with a **nonzero-value child** | **local-chain fixture** in the tidx test DB (block 999001): reverted frame p1 with value-5 child p2; sibling p5 value 7 outside; p6 DELEGATECALL value 9; trailing reverted p7 with value-11 child p8 | ✅ q2 returns **only p5 = 7**; control without the CTE returns p2, p8 too. *Igra has no real instance: 1 717 successful txs contain a reverted frame, none with value inside the subtree* |
| F5 | native / WiKAS / other ERC-20 | F7 (native), F1/F2 (WiKAS), F1/F6 counter-legs (other tokens) | ✅ |
| F6 | swap / pool — **no propagation** | `0x448cb992c152006e117640a23c7cf6692f6e8addc6108dc6eb899e60a8060b4c` (17962263): `0x30b3e9d6…7caa` → pool `0xbe3c61c2…e7bd` 264.55 WiKAS, receives 5.73e24 of another token | ✅ rows carry `called_contract = pool`, `selector 0x4d13dd92` → review; the pool's other users are **not** added |
| F7 | bridge without duplicate value | `0x47071e4d10ad9a2a17f49accff69e23c765a8e400a573bd073504f2765904c75` (17949927): **10 020 iKAS** → canonical exit `0x4bb88c21…b2d0`, `selector 0x5f8b1cce` | ✅ q1 **1 row**, q2 **0 rows** — counted once. Its Hyperlane `Dispatch` is emitted by `0x3a867fcf…2aa7` in the same tx (link on `tx_hash`) |
| F8 | full-page continuation | `0xc281cb25…14d7`, 2M blocks, page 5 | ✅ `page1+page2 == single 10-row query`; no overlap; cursor `(15978373, 4, 5)` |
| F9 | restart without duplicate alerts | identical re-run of page 1 | ✅ identical rows → dedupe key makes re-emission a no-op |
| F10 | reorg / gap | `/status` cutoff pinned via q0a; q0b on block 17959343 matches the stored hash `0x7c6e65d3…b764`; q0c over the F1 range → 0; `reorgs` = 0 | ✅ mechanism exercised; no live reorg exists |

Cross-table completeness (`fixtures/` run): every fixture tx is present and
consistent in `txs`, `receipts`, `logs`, `internal_txs` (e.g. F1 legs: receipt 1,
4 logs, 16 frames; F7: receipt 1, 6 logs, 11 frames; F3: receipt 0, 0 logs).

## Benchmark
See `benchmark.md` (generated from `fixtures/B_*.output.json`).

## Coverage gaps (recorded, not hidden)

* **Exits (payload 0x3) are not parsed by tidx.** L2→L1 exits have no L1-side
  provenance row. On the L2 side an exit **is** fully visible as a native tx
  to the canonical exit contract (F7) — alert on that.
* **`l2_withdrawals` holds L1→L2 *entries* (deposits)**, not exits. Do not
  read it as exits.
* **Hyperlane routers:** the nine fork-fixture addresses are not on the tidx
  host and were **not guessed**. `config.example.json` has an empty slot for
  them plus three evidence-based on-chain candidates (the sole `Dispatch`
  emitter and two contracts present in every exit tx).
* **NFT Transfer events** are excluded (`topic3 IS NULL`).
* **`block_timestamp` is synthetic** (DAA-derived, ~10 min behind wall clock
  today). Exact bounds are `block_num`; `blocks.real_timestamp` is L1 time.
* **`/query` is unauthenticated** and has no bind parameters. `run.py`
  validates every address (`0x` + 40 hex) and every number before
  substitution; tidx's AST validator (single SELECT, table/function
  allowlist) is the second line. Do not put this URL anywhere untrusted.

## Index note (for "choose extra indexes from measurements")
`txs`/`internal_txs` from/to indexes are `(addr, block_timestamp)`; `logs`
topic2 is `(topic2)` alone. The `block_num` bound is applied *after* the
index, so cost scales with the address's total history, not the window
(observed: 68 k rows filtered on one internal-branch lookup). Mitigation
already in the queries: `TS_LO`. If a heavy address becomes a problem, the
one index worth adding is `logs(topic2, block_num)`.
