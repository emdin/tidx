#!/usr/bin/env python3
"""Repeatable acceptance run. Executes every fixture and asserts its expectation.

    acceptance.py [--base https://tidx.igralabs.com] [--local-db <psql-url>] [--skip-local]

Production fixtures go through run.py (exact input/output preserved next to
each params file). Local-chain fixtures (cases Igra has no real instance of)
run the SAME sql files verbatim through psql against a tidx test database
prepared by local/setup.sql — pass its URL with --local-db, or --skip-local.

Exit code 0 only if every assertion holds. Each line prints PASS/FAIL with
the evidence (row counts, the tx hashes found or not found).
"""
import json, os, re, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
SQL  = os.path.join(HERE, "..", "sql")

def run(sql, params, base):
    p = subprocess.run([sys.executable, os.path.join(HERE, "run.py"), os.path.join(SQL, sql),
                        os.path.join(HERE, params), "--base", base], capture_output=True, text=True)
    out = re.sub(r"\.params\.json$", "", os.path.join(HERE, params))
    rec = json.load(open(out + ".output.json"))
    r = rec["response"]
    rows = [dict(zip(r.get("columns", []), x)) for x in r.get("rows", [])] if rec["outcome"] == "ok" else None
    return rec["outcome"], rows

def has(rows, prefix, direction=None):
    return any(d["tx_hash"].startswith(prefix) and (direction is None or d.get("direction") == direction) for d in rows)

# name, sql, params, assertion(rows) -> (bool, evidence)
PROD = [
 ("F1  seed→B caught in poll 1",                "q3_erc20.sql", "F1_poll1_seedA.params.json",
   lambda r: (has(r,"0xf210f333","out"), f"{len(r)} rows")),
 ("F1  B→C caught in poll 2 (both legs)",       "q3_erc20.sql", "F1_poll2_A_and_B.params.json",
   lambda r: (has(r,"0xa8fdae0b","out") and has(r,"0x553cc102","out"), f"{len(r)} rows")),
 ("F1b wallet forward INSIDE window: A→B",      "q3_erc20.sql", "F1b_wallet_forward_in_window_seedA.params.json",
   lambda r: (has(r,"0xca275fc2","out"), f"{len(r)} rows")),
 ("F1b wallet forward INSIDE window: B→C on rescan", "q3_erc20.sql", "F1b_wallet_forward_in_window_rescanB.params.json",
   lambda r: (has(r,"0x244884b0","out"), f"{len(r)} rows")),
 ("F2  small transfer (105 raw)",               "q3_erc20.sql", "F2_small_transfer.params.json",
   lambda r: (any(d["amount_raw"]=="105" for d in r), f"{len(r)} rows")),
 ("F3  failed tx excluded",                     "q1_native.sql", "F3_failed_tx_native.params.json",
   lambda r: (len(r)==0, f"{len(r)} rows")),
 ("F3b zero-value Transfer excluded",           "q3_erc20.sql", "F3b_zero_value_transfer.params.json",
   lambda r: (not has(r,"0x0effb1d6"), f"{len(r)} rows")),
 ("F6  swap flagged, called_contract=pool",     "q3_erc20.sql", "F6_swap_user_in.params.json",
   lambda r: (has(r,"0x448cb992") and all(d["called_contract"]=="0xbe3c61c2f57f78cdbaf35f0c03c3c8a680cce7bd" for d in r if d["tx_hash"].startswith("0x448cb992")), f"{len(r)} rows")),
 ("F7  exit: native counted once (q1=1)",       "q1_native.sql", "F7_exit_native.params.json",
   lambda r: (len(r)==1 and r[0]["called_contract"]=="0x4bb88c213d3ed9dc4bae694f1bc1bf745903b2d0", f"{len(r)} rows")),
 ("F7  exit: no internal double count (q2=0)",  "q2_internal.sql", "F7_exit_internal.params.json",
   lambda r: (len(r)==0, f"{len(r)} rows")),
 ("F8  page1 has sentinel row (6 = 5+1)",       "q3_erc20.sql", "F8_page1.params.json",
   lambda r: (len(r)==6, f"{len(r)} rows")),
 ("F8  page2 continues from cursor",            "q3_erc20.sql", "F8_page2.params.json",
   lambda r: (len(r)==6, f"{len(r)} rows")),
 ("F8  single 10-row reference",                "q3_erc20.sql", "F8_single10.params.json",
   lambda r: (len(r)==11, f"{len(r)} rows")),
 ("F9  identical re-run of page1",              "q3_erc20.sql", "F9_restart_page1.params.json",
   lambda r: (len(r)==6, f"{len(r)} rows")),
 ("F10 cutoff hash pinned",                     "q0a_cutoff_hash.sql", "F10_cutoff.params.json",
   lambda r: (len(r)==1 and r[0]["cutoff_hash"].startswith("0x"), f"{r[0]['cutoff']} {r[0]['cutoff_hash'][:12]}…")),
 ("F10 pin check: block 17959343 hash unchanged","q0b_pin_check.sql", "F10_pincheck.params.json",
   lambda r: (r[0]["hash"]=="0x7c6e65d33e9d8da3aa90c624b0607c79ff3b68bf04a42d42e5cb9fe72440b764", r[0]["hash"][:12]+"…")),
 ("F10 no orphaned blocks in F1 range",         "q0c_orphaned_range.sql", "F10_orphaned.params.json",
   lambda r: (len(r)==0, f"{len(r)} rows")),
 ("F11 Hyperlane bridge-out: burn Transfer to 0x0 via token router", "q3_erc20.sql", "F11_hyperlane_bridge_out_erc20.params.json",
   lambda r: (has(r,"0x244c4fdc","out") and r[0]["to_address"]=="0x0000000000000000000000000000000000000000" and r[0]["called_contract"]=="0xa5b8bf902b2844da17d4506cc827f7f1681735e7", f"{len(r)} rows, sel {r[0]['selector'] if r else '-'}")),
 ("F11 Hyperlane bridge-out: native value to router (q1)", "q1_native.sql", "F11_hyperlane_bridge_out_native.params.json",
   lambda r: (has(r,"0x244c4fdc","out") and r[0]["called_contract"]=="0xa5b8bf902b2844da17d4506cc827f7f1681735e7", f"{len(r)} rows")),
 ("F0d internal-coverage signal reports (not assumes) coverage", "q0d_internal_coverage.sql", "F0d_coverage_F1b_window.params.json",
   lambda r: (len(r)==1 and int(r[0]["successful_txs"])==int(r[0]["plain_transfers_certainly_covered"])+int(r[0]["traced_with_frames"])+int(r[0]["possibly_untraced"]), f"{r[0] if r else '-'}")),
]

def keyset(rows): return [(d["block_num"], d["tx_index"], d["log_index"]) for d in rows]

def main():
    a = sys.argv[1:]; base = "https://tidx.igralabs.com"; local = None; skip_local = "--skip-local" in a
    if "--base" in a: base = a[a.index("--base")+1]
    if "--local-db" in a: local = a[a.index("--local-db")+1]
    ok_all = True; results = {}
    for name, sql, params, check in PROD:
        outcome, rows = run(sql, params, base)
        if rows is None:
            ok, ev = False, f"outcome={outcome}"
        else:
            ok, ev = check(rows); results[params] = rows
        ok_all &= ok; print(f"{'PASS' if ok else 'FAIL'}  {name:52s} {ev}")
    # cross-fixture pagination invariants
    p1 = keyset(results["F8_page1.params.json"])[:5]; p2 = keyset(results["F8_page2.params.json"])[:5]
    s10 = keyset(results["F8_single10.params.json"])[:10]; rr = keyset(results["F9_restart_page1.params.json"])[:5]
    for name, ok in [("F8  page1+page2 == single 10-row query", p1+p2==s10), ("F8  no overlap between pages", not set(p1)&set(p2)),
                     ("F9  restart re-run identical to page1", rr==p1)]:
        ok_all &= ok; print(f"{'PASS' if ok else 'FAIL'}  {name}")
    if not skip_local:
        if not local:
            print("SKIP  local-chain fixtures: pass --local-db <url> (see local/setup.sql) or --skip-local");
        else:
            ok_all &= subprocess.run([sys.executable, os.path.join(HERE, "local", "run_local.py"), local]).returncode == 0
    print("\nALL PASS" if ok_all else "\nFAILURES PRESENT"); sys.exit(0 if ok_all else 1)

if __name__ == "__main__":
    main()
