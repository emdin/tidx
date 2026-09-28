# Sentinel pilot — tidx SQL for watched-wallet movements (Igra, chain 38833)

Deliverable for the "Minimal pilot" spec (8 Sep 2026). Three parameterized,
per-source SQL queries against tidx's existing `/query` endpoint — **no new
service, no materialized view** — plus cutoff/reorg/coverage helpers, orphan
twins of the three queries, a runner that preserves exact inputs and outputs,
and acceptance fixtures on real Igra transactions (local-chain fixtures where
Igra has no real instance).

Everything here was executed against production tidx on 2026-09-28 (image
`iidx-local:igra-15fb1e0`, PR #29). Every claim about tidx behaviour below was
verified on that date; where something is inferred or unresolved it says so.

```
sql/q1_native.sql             top-level native iKAS transfers      (txs ⋈ receipts ⋈ blocks)
sql/q2_internal.sql           internal native transfers via traces (internal_txs), reverted-subtree-safe
sql/q3_erc20.sql              ERC-20 Transfer events, all tokens   (logs), NFT- and zero-value-excluded
sql/q1o_native_orphaned.sql   } the same three over the reorg archive, joined on reorg_id —
sql/q2o_internal_orphaned.sql } return the ORPHANED block hash so a correction can be
sql/q3o_erc20_orphaned.sql    } keyed on what was originally alerted
sql/q0a_cutoff_hash.sql       pin the complete-indexed cutoff
sql/q0b_pin_check.sql         reorg detection on resume and after each scan; also fetches TS_LO
sql/q0c_orphaned_range.sql    which blocks a reorg invalidated, with reorg_id
sql/q0d_internal_coverage.sql INTERIM trace-coverage heuristic (until trace_outcomes is deployed)
sql/q0e_trace_coverage.sql    trace coverage from tidx's per-tx trace record (this PR's Rust change)
fixtures/run.py               substitute → validate → save input → POST /query → save outcome
fixtures/classify.py          reference expansion classifier: block-pinned eth_getCode + config (step 4)
fixtures/acceptance.py        repeatable acceptance run (prod fixtures + classifier decisions + DB fixtures)
fixtures/*.params.json        exact inputs;  *.input.json  what was sent;  *.output.json  what came back
fixtures/local/setup.sql      synthetic DATABASE QUERY FIXTURES (F1c, F4/F4c, F10b/F10c) — disposable test DB only
fixtures/local/run_local.py   runs the SAME sql files through psql against that DB
config.example.json           watchlist / token decimals / contract classification / rpc_url / state schema
state.example.json            shape of the persisted bot state (two checkpoints + unresolved-trace backlog)
benchmark.md                  measured cost per query × watchlist size × window
```

## Repeatable acceptance command

```sh
# production fixtures only (network access to tidx.igralabs.com)
python3 fixtures/acceptance.py --skip-local

# production + database fixtures; <db> is a libpq URL for psql, or
# docker:<container>:<user>:<dbname> for a DISPOSABLE tidx test database
# (migrations applied, marked once with `CREATE TABLE sentinel_disposable_db ();`)
python3 fixtures/acceptance.py --local-db docker:tidx-test-pg:tidx:tidx

# classifier rules alone, offline (stubbed code lookup)
python3 fixtures/classify.py --selftest
```

Exit code 0 only if every assertion holds. Each line prints PASS/FAIL with the
evidence. Last run 2026-09-28: 24 production query checks + 8 classifier
decisions (live block-pinned `eth_getCode`) + 10 database-fixture checks,
ALL PASS. The production checks establish query behaviour; they do **not**
establish complete trace coverage (see "Internal coverage").

## How the bot uses it

1. **Cutoff.** `GET /status` → `chains[0].synced_num`, `gap_blocks`, `gaps[]`.
   `synced_num` is the highest block below which tidx has verified the
   `blocks` table contiguous. Pin it: `q0a` → store `(cutoff, cutoff_hash)`.
   **Never scan past `synced_num`.** If `gap_blocks > 0`, the window is not
   complete — report it, don't scan into it.
2. **Windows.** Split `[checkpoint+1, cutoff]` into windows of at most
   `max_window_blocks` (200 000 recommended — see benchmark). `TS_LO` =
   `timestamp` of `BLOCK_LO` via `q0b`. `TS_LO` is only an index-friendly
   prefilter; the exact bound is always `block_num`.
3. **Per window: run q1, q2, q3** with the same window and watchlist, each
   paginated to completion (below), then `q0d` for the window (record its
   `possibly_untraced`; if > 0 the window's internal coverage is
   **unresolved**, see below).
4. **Watchlist expansion — classify before adding.** The reference
   implementation is `fixtures/classify.py` (`decide(row, …)`); the rules:
   * `direction = 'out'` — the watched address is the sender. Inbound rows
     are reported, never expand (they would add the payer).
   * positive value — enforced in SQL (`value > 0` for q1/q2,
     `abi_uint(data) > 0` for q3); the classifier re-checks it.
   * eligible watch-start position — the row's `(block_num, tx_index,
     log_index | trace_path)` is at or after the position from which the
     **sender** is watched (`watch_from_block` for a seed; the first observed
     inbound row for an added recipient). Earlier rows are history: reported.
   * **explicit address classification**, same function for the direct
     recipient (`to_address`) and the top-level callee (`called_contract`),
     first match wins: `zero_address` → `exit` → `hyperlane_router` (the nine
     routers) → `known_pool` → `token` (config or the row's own
     `token_address`) → otherwise **`eth_getCode(addr, row.block_num)`**
     against `config.rpc_url`: non-empty → `contract`, empty → `eoa`, lookup
     failure → `unknown`. Appearing in `called_contract` does *not* make an
     address a contract (q1 puts the recipient wallet there for a plain
     payment), and absence from config or from the run does *not* make it an
     EOA — only the block-pinned code lookup decides those two, and
     `unknown` is retained for review, never treated as EOA.
   * **expand** only if the recipient is `eoa` **and** the callee is one of
     the two allowed cases: `callee == recipient` (plain native payment to a
     wallet, F12) or `callee == token_address` (ordinary ERC-20
     `transfer()` — the token contract is the callee while the recipient may
     be a wallet, F1b A→B). Every other callee (exit, router, pool, any other
     contract, unknown) → **review**.
   Unknown contracts and services stay in human review; only reviewed
   decisions add them to `config.contracts`. A designation is never
   propagated through a pool, router or exchange to its other users; the bot
   adds at most the direct counterparty. Evidence that the code lookup is
   load-bearing: F1b's recipient C (`0x923ba6be…cf92`) *looks* like a wallet
   in the query row but has 8 KB of code at that block and 1 217 calls into
   it — the classifier returns `review`, not `expand`.
5. **Recipient rescan.** For every address added in step 4, run q1–q3 with
   watchlist = that address from its first observed inbound position
   through **the end of the current window**. This catches movements later
   in the **same block** (F1c) and later in the same window (F1b) that the
   original scan could not see. Rescans overlap earlier scans by
   construction — deduplicate on the key below. Recurse for recipients found
   by rescans, same rule.
6. **Window checkpoints.** After every page of q1–q3 for the window *and* every
   rescan it triggered are complete, re-run `q0b` on the window's last block:
   hash unchanged → advance the checkpoints (below) to that window end and
   persist `(block, hash)`; changed → treat as a reorg before advancing.
   The **overall cutoff** is reached only after all windows and all recipient
   rescans finish; never advance to the pinned cutoff early.

### Checkpoints and the unresolved-trace backlog (persisted state, required)
`state.example.json` is the shape. Two checkpoints advance **independently**:

* `scan_checkpoint` — q1 + q3 (native top-level + ERC-20). Advances per
  window as in step 6.
* `internal_checkpoint` — q2. May advance through a window **only** when
  `q0d` reported `possibly_untraced = 0` for it, **or** the window has been
  appended to `unresolved_internal_ranges` `{block_lo, block_hi,
  possibly_untraced, recorded_at, resolved_at: null}`.

Advancing `scan_checkpoint` therefore never discards an internal range: the
backlog entry survives restarts and is replayed — q2 over exactly that range,
same watchlist, then the step-4/5 rules — after a trace repair (`tidx
backfill-traces --from --to` today; the trace-outcome record once it lands),
and only then gets `resolved_at`. `pending_rescans` is persisted for the same
reason: a restart between a discovery and its rescan must not lose it.

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
`run.py` records five outcomes: `ok`, `api_error` (HTTP 200 with `ok:false`),
`http_error` (4xx/5xx — validator rejections and timeouts come back as **HTTP
4xx with a JSON body**, saved verbatim), `malformed_response` (2xx but not the
expected JSON shape), `transport_error` (DNS/TLS/connection/socket timeout).
It writes `<fixture>.input.json` **before** the request and the outcome after,
so a failed attempt leaves both. Exit code is 0 only for `ok`. Treat any
non-ok as "retry this page", never as "nothing happened". Limits: default
`timeout_ms` 5000 (max 30000); page cap 10 000 (Postgres) — `LIMIT 10001` is
rejected loudly.

### Dedupe key (restart-safe alerts)
`(chain_id, tx_hash, event_kind, log_index | trace_path)`. Store it with the
alert; rescans and restarts re-emit the same rows (F9) and must be idempotent.

### Reorg handling
tidx archives every reorg atomically (`reorgs` + `orphaned_blocks/txs/
receipts/logs/internal_txs`, all keyed by `reorg_id`; PR #13). Igra has
recorded **0 reorgs** to date (F10). The orphan queries are therefore
exercised only on synthetic `reorgs`/`orphaned_*` rows (F10b/F10c) — that
tests the SQL over the archive schema, **not** tidx's reorg handling itself.

* **Detect.** `q0b` on the stored checkpoint block before each poll, and on
  the window's last block after each scan (step 6). Hash differs → reorg.
* **Identify.** `q0c` over `[checkpoint - depth_guess, checkpoint]` returns
  the orphaned blocks with their `reorg_id`; `reorgs.fork_point` is the
  replay start.
* **Correct from the retained original observations.** Run `q1o`/`q2o`/`q3o`
  with `REORG_ID` = that id and the same watchlist, **paginated with the same
  keyset cursors as q1–q3** (a reorg can orphan more than a page of movements
  in one block — F10c). They join
  `orphaned_txs ⋈ orphaned_receipts ⋈ orphaned_blocks` (and `orphaned_logs`
  / `orphaned_internal_txs`) **within the same `reorg_id`** — never the
  canonical tables, which by then hold the replacement block (or nothing).
  Each row carries the **orphaned** `block_hash`, i.e. exactly what was
  alerted on; emit a correction keyed on the dedupe key + that hash.
* **Replay** q1–q3 from `fork_point` over the canonical tables, then continue.

Do not "re-point" q1–q3 at the orphaned tables ad hoc: their joins to
canonical `txs`/`receipts`/`blocks` would drop rows or attach the
replacement block hash. That is what the `*o` twins exist for.

### Internal coverage — resolved by the trace-outcome record (this PR)
This PR adds the tidx-side fix in the same branch (`src/sync/trace.rs`,
`db/trace_outcomes.sql`, `src/sync/engine.rs`, `src/cli/backfill_traces.rs`,
tests in `tests/trace_outcomes_test.rs`):

* **`trace_outcomes`** — one row per traced tx: `ok` (frames written),
  `empty` (traced, no nested call), `failed` (every attempt errored, `error`
  kept), with cumulative `attempts`. A tx with no row was never traced.
* **Retry** — each trace gets 3 attempts with backoff on the realtime and
  gap-fill paths; a failure is recorded, never skipped.
* **Gap-fill traces** — `sync_range_standalone` now traces gap-filled blocks
  when tracing is enabled (it never did before).
* **Repair loop** — a background task re-traces `failed` (up to 12 cumulative
  attempts) and never-traced txs within the last 100k blocks below
  `synced_num`, 200 per pass.
* **`tidx backfill-traces`** — default selection is now failed + never-traced
  (not "no internal_txs rows", which conflated `empty` with untraced);
  `--mark-existing` stamps `ok` on already-traced history without RPC;
  `--all` re-traces everything. Reorgs drop outcomes for orphaned blocks.
* **`/query`** exposes `trace_outcomes`; `q0e_trace_coverage.sql` partitions a
  window into `ok / empty / failed / untraced` and returns `resolved` only when
  `failed = 0 AND untraced = 0`. The acceptance run executes F0e once the
  table is live on the target tidx and prints SKIP until then.

Until that deploy, `q0d` is the only signal. It exists because tidx before
this change had **no per-transaction trace outcome** (verified in source
2026-09-28):

* the realtime tracer logs `Failed to trace tx; skipping` and moves on — no
  retry, no marker (`src/sync/trace.rs`);
* the gap-fill writer (`sync_range_standalone`) fetches blocks + receipts and
  **never traces**;
* a trace that succeeds with zero nested frames writes nothing, so "no rows"
  is indistinguishable from "never traced";
* `tidx backfill-traces` re-traces every tx lacking rows, blindly.

So an empty q2 can mean missing evidence, and a maximum traced block number
would not detect any of the above. `q0d` bounds the uncertainty per window:
a receipt with `gas_used = 21000` cannot have nested frames (certainly
covered); every other successful tx either has `internal_txs` rows (traced)
or has none — **either** a leaf call that made no nested call (common: an
ERC-20 `transfer()` emits a log but calls nothing) **or** untraced. It cannot
tell those apart. Measured: F1b window (200 blocks, 126 txs) → 123 with
frames, 3 possibly untraced, 106 ms; a 200k window → 85 603 / 1 191, 27 s
(run it per poll window, not per replay chunk).

**Bot rule:** report `internal_coverage: unresolved` for any window where
`q0e.resolved` is false (or, pre-deploy, `q0d.possibly_untraced > 0`) **and
persist it in `unresolved_internal_ranges`** (above) so the range is replayed
after repair rather than skipped; run `tidx backfill-traces --from --to` on it
and re-run q2 to resolve. **Upgrade order:** run `tidx backfill-traces
--mark-existing --from 0 --to <synced_num>` (chunked per `--batch-size`, no
RPC for already-traced txs) *before* starting the new engine image; otherwise
the repair loop re-traces the last 100k blocks (~42k txs on prod) over RPC.
Failed txs are retried no sooner than 10 min after their last attempt, so an
RPC outage costs one pass, not the attempt budget.

### Historical trace coverage — established 2026-09-28 (fixtures `HIST_*`)
Full history was brought under the trace record after the PR #30 deploy:

| Step | Evidence |
|---|---|
| cutoff pinned via q0a | block **17 976 809**, hash `0x768b6481f2f69de1315791a70530301464826e7797012292848121f3b8300ac0`, ts 2026-09-28T21:36:31Z (`HIST_cutoff.output.json`) |
| already-traced history stamped `ok` (no RPC) | 6 954 705 txs, 950 s |
| `tidx backfill-traces --from 0 --to 17976809` | 160 438 never-traced txs traced in 27 min, **519 715 internal frames recovered**, 4 transient failures (`error decoding response body`) retried to `empty` (`HIST_backfill_traces.log`) |
| q0e over `[0, 17976809]` (`HIST_q0e_full.rendered.sql` → `HIST_q0e_full.output.csv`) | `successful_txs 7 061 877 = ok 6 946 798 + empty 115 079`, **failed 0, untraced 0, resolved = true** |
| every tx ≤ cutoff, any receipt status | 7 117 173 txs, 0 without an outcome, 0 failed |
| pin re-check after the run (q0b) | hash unchanged (`HIST_pincheck.output.json`) |

Above the cutoff the realtime path records outcomes as blocks land and the
repair loop covers the trailing 100k blocks; re-run q0e per window to prove
it for any later range. The 519 715 recovered frames are internal movements
that were invisible before this change — gap-fill-synced blocks had never
been traced.

## Correctness rules and where each is enforced

| Rule (spec) | Enforcement | Verified |
|---|---|---|
| Committed only | q1/q2 join `receipts` and require `status = 1`. q3 does not: **logs exist only for successful txs** | 0 logs in 1 124 reverted txs (300k-block window); F3 |
| Reverted internal subtrees excluded | q2 CTE `reverted`: each reverted frame's DFS-preorder interval `(start_p, end_p)`; frames inside any are excluded. **`error IS NULL` alone is wrong** — descendants of a reverted frame carry `NULL` | 863 of 909 reverted parents had error-free children; F4 |
| DELEGATECALL value is not a transfer | q2 `call_type IN ('CALL','CREATE','CREATE2','SELFDESTRUCT')` | 25 DELEGATECALL frames with value>0 per 300k blocks; F4 (p6) |
| CREATE2 with value is a transfer | included in q2 with the same ancestor-revert check | F4c: CREATE2 outside a reverted subtree returned, CREATE2 inside one excluded. *Igra has 0 value-carrying CREATE2 frames on chain today* |
| Positive amounts only | q1/q2 `value > 0`; q3 `abi_uint(data) > 0` evaluated server-side on NUMERIC (full uint256) — only *returned* `abi_uint` values hit the JSON 2^96 ceiling, which is why `amount_raw` uses `format_uint` | F3b |
| No top-level/internal double count | `txs` = depth 0, `internal_txs` = depth ≥ 1, disjoint by construction | schema |
| Bridge event not counted twice | an exit is one q1 row (native → exit contract); its internal frames are contract→contract, so q2 matches nothing for the watched sender | F7 |
| ERC-20 ≠ NFT | q3 `topic3 IS NULL` | 273 407 ERC-20 vs 6 656 NFT events, clean split |
| Direction explicit | every row carries `direction` (`out` = watched address is the sender); only `out` rows may expand the watchlist | F1/F1b/F1c/F11 assert on `out` |
| Unknown decimals stay unknown | q3 returns `decimals = NULL`; bot maps by `token_address` from config (WiKAS = 18 via `eth_call`) | — |

## Acceptance fixtures (full hashes; inputs/outputs in `fixtures/`)

| # | Case | Fixture | Result |
|---|---|---|---|
| F1 | seed → recipient → next recipient **between two polls** (router-mediated) | seed `0x7826f542…cbe45`. Poll 1 `[17959300,17959360]`: WiKAS **277.36** → `0x13f197c7…0bd5` in `0xf210f33312bcd4b1fb65ed850022ebe5548820f13ab07befd88f4390f4358c2b` (blk 17959343). Recipient rescan `[17959343,17959360]` → only that inbound. Poll 2 `[17959361,17960200]` with both watched: `0x13f1…` → `0xc281cb25…14d7` **398.59** in `0xa8fdae0b11134066987a35f0562c43bc854cc0bb1854a5a9be975a0e361120d5` (17959374) and **186.83** in `0x553cc102008302d5c9cf833002608959daebb6b4432260684c0712b1f2a0505c` (17960101) | ✅ forwarding caught in poll 2; every leg has `called_contract = 0xa5b0946d…`, `selector 0x38ed1739` → **review flag** (router), so under step 4 these do *not* expand the watchlist |
| F1b | **ordinary-wallet forwarding inside the initial scanned range** | direct `transfer()` legs, A = `0xc87dfcebdb5de6ba38975bdb1418174b88b51e00`, B = `0x725836b4d485ab446c1698d3b622dae0084b1c98`, C = `0x923ba6be4bbe5d89975e259a0d3e09842045cf92`, window `[17114600,17114800]`. Seed A: A→B **9 990** WiKAS in `0xca275fc2b46504f5b96b5734840363c5362bb30119b2e3fd68f848324f5d048a` (17114606). Rescan B over the SAME window: B→C **100** in `0x244884b027d68af104e215a5690f9869624b141cf1502b901d09a93490da53fe` (17114613) and B→C 9 700 in `0x0c2894dd8ef84cc04e94238d3a9c6be44fc13e94ce5d222f95a4b801f99d168d` (17114741) | ✅ both legs `direction = out`, `called_contract` = the token itself, `selector 0xa9059cbb`. Classifier: A→B **expand** (B has no code at 17114606); B→C **review** — C has code (a contract with 1 217 inbound calls), which nothing in the row reveals |
| F1c | forwarding **in the same block** | **local-chain** block 999002: tx#0 A→B 500 T, tx#1 B→C 200 T | ✅ poll with A returns tx#0 (`out`); rescan with B over the same one-block window returns tx#1 (`out`). *Igra has no real same-block instance among direct wallet transfers* |
| F2 | small positive transfer | `0xaeb835081aa881a5cc86dc761cbf300fe7eae0cf7d36031d0d08fef84d335af6` (17315165), **105 raw** WiKAS from `0xe3ec9732…25b8` | ✅ returned |
| F3 | failed transaction | `0x7b8ed91308ef638f2fbc53ed338d4a47b09734047b4e6b699c67ddbb28c3c48b` (17886245), 2 885 iKAS, `status = 0` | ✅ q1 returns **0 rows** |
| F3b | zero-value Transfer event | `0x0effb1d6a66404f1e337fd7e939a6a6ae4716b8b09f402f91fb60fb7d06e6648` (17766544), Transfer of 0 involving `0xb5270013…` | ✅ q3 returns **0 rows** for that address/window |
| F4 | successful tx, reverted internal call with a **nonzero-value child** | **local-chain** block 999001, 12 frames: reverted p1 with value-5 child p2; p5 value 7 outside; p6 DELEGATECALL 9; trailing reverted p7 with value-11 child p8; p9 CREATE2 13 outside; p10 reverted with CREATE2 17 child p11 | ✅ q2 returns **exactly p5 = 7 (CALL) and p9 = 13 (CREATE2)**; control without the CTE also returns p2, p8, p11. *Igra: 1 717 successful txs contain a reverted frame, none with value inside the subtree* |
| F4c | CREATE2 inclusion + ancestor check | same fixture (p9 in, p11 out) | ✅ |
| F5 | native / WiKAS / other ERC-20 | F7, F11 (native), F1/F1b/F2 (WiKAS), F1/F6 counter-legs (other tokens) | ✅ |
| F6 | swap / pool — **no propagation** | `0x448cb992c152006e117640a23c7cf6692f6e8addc6108dc6eb899e60a8060b4c` (17962263): `0x30b3e9d6…7caa` → pool `0xbe3c61c2…e7bd` 264.55 WiKAS, receives 5.73e24 of another token | ✅ rows carry `called_contract = pool`, `selector 0x4d13dd92` → review; `to_address` is the pool → not eligible; the pool's other users are **not** added |
| F7 | bridge without duplicate value | `0x47071e4d10ad9a2a17f49accff69e23c765a8e400a573bd073504f2765904c75` (17949927): **10 020 iKAS** → canonical exit `0x4bb88c21…b2d0`, `selector 0x5f8b1cce` | ✅ q1 **1 row**, q2 **0 rows** — counted once. Its Hyperlane `Dispatch` is emitted by `0x3a867fcf…2aa7` in the same tx |
| F8 | full-page continuation | `0xc281cb25…14d7`, 2M blocks, page 5 | ✅ `page1+page2 == single 10-row query`; no overlap; cursor `(15978373, 4, 5)` |
| F9 | identical re-run | identical re-run of page 1 | ✅ identical rows. **Restart persistence and alert dedup are bot-lifecycle behaviour — explicitly deferred to the bot deliverable**, not demonstrated here |
| F10 | cutoff pin / reorg detection | `/status` cutoff pinned via q0a; q0b on block 17959343 matches `0x7c6e65d3…b764`; q0c over the F1 range → 0; `reorgs` = 0 | ✅ mechanism exercised; no live reorg exists on Igra |
| F10b | reorg correction from the archive | **database fixture** (synthetic `reorgs`/`orphaned_*` rows — not an executed reorg): reorg 999 orphaned block 999003 (hash `0xdead…9003`) holding txs `feed…04` (A→B 5 wei + 900 T) and `feed…05` (7 wei, five transfers 901–905 T, three internal frames); canonical replacement block is empty | ✅ q1o (2 rows), q2o (3), q3o (6) with REORG_ID 999, watched A, every row **with the orphaned hash**; canonical q3 over the same range returns 0 rows |
| F10c | **more than a page of orphaned rows in one block** | same fixture, page sizes 1 / 2 / 4 | ✅ q1o 2 pages, q2o 2 pages, q3o 2 pages; each `page1+…+pageN == single query`, no overlap |
| F12 | plain native payment wallet→wallet | `0x076f281b28f45104e855594aa4a0a8822c6a6f68cfd36dbf8f6441326b2809f3` (17950630): `0x2f296ed8…` → `0xa0416cde…` **100 iKAS**, `gas_used 21000` | ✅ q1 1 row `out`; `called_contract == to_address` (a wallet, no code at that block) → classifier **expand** |
| C | expansion decisions | `fixtures/classify.py` over F1b, F12, F1, F6, F7, F11 with live block-pinned `eth_getCode` | ✅ expand ×2 (F1b A→B, F12); review ×6 (contract recipient, router callee, pool, exit, zero address, Hyperlane router); offline `--selftest` covers lookup failure → review and watch-start positions |
| F11 | representative **Hyperlane** movement | `0x244c4fdc9822fb620bc2e47ad81d8a6b5d931d665628a6403778e3643d7f3350` (17293324): `0xaaf9e5f4…` calls token router `0xa5b8bf90…35e7` (`selector 0x81b4e8b4`), which burns **176 479 296 raw** (Transfer → `0x0`) and emits `Dispatch`; the tx also sends **4.797 iKAS** native to the router | ✅ q3 1 row `out` to the zero address, q1 1 row `out` to the router → both classify as bridge-out via router → review, not expansion |
| F0d | coverage is reported, not assumed | `q0d` over the F1b window and over a 200k window | ✅ counts partition `successful_txs` exactly; 3 (resp. 1 191) `possibly_untraced` → those windows are `unresolved` |

Cross-table completeness: every fixture tx is present and consistent in
`txs`, `receipts`, `logs`, `internal_txs` (e.g. F1 legs: receipt 1, 4 logs,
16 frames; F7: receipt 1, 6 logs, 11 frames; F3: receipt 0, 0 logs).

Database fixtures (`fixtures/local/`) are synthetic rows inserted straight
into tidx's tables. They run the **same SQL files** through psql; the only
rendering difference is that `'0x…'` literals become `'\x…'::bytea` directly
(what `/query`'s hex rewriter does server-side). They exercise the queries
over the schema; they do not run the indexer. `setup.sql` deletes by block
range / reorg id, so it is restricted to a **disposable test database**: it
refuses to run unless the operator has marked the database with
`CREATE TABLE sentinel_disposable_db ();` and it holds no large chain.
Outputs are saved next to the runner as `*.local.output.csv`.

## Benchmark
See `benchmark.md` (generated from `fixtures/B_*.output.json`; re-measured
after the review edits).

## Coverage gaps (recorded, not hidden)

* **Internal-trace coverage** is measurable per window once `trace_outcomes`
  is deployed (q0e); before that q0d only bounds it. Either way the persisted
  backlog keeps unresolved windows replayable.
* **Exits (payload 0x3) are not parsed by tidx.** L2→L1 exits have no L1-side
  provenance row. On the L2 side an exit **is** fully visible as a native tx
  to the canonical exit contract (F7) — alert on that.
* **`l2_withdrawals` holds L1→L2 *entries* (deposits)**, not exits. Do not
  read it as exits.
* **Hyperlane routers** — the nine Igra source routers (iKAS, USDC, USDT,
  WETH, cbBTC, wstETH, SOL, USDS, sUSDS) are in
  `contracts.hyperlane_routers.from_fork_fixture`, supplied by the reviewer
  from `igra-sentinel/test/IgraFork.t.sol` @ `a3029464` (reviewed
  configuration, not a live governance check). F11's router `0xa5b8bf90…`
  is the USDC route. The Mailbox `0x3a867fcf…` is a different role and is
  listed separately.
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
