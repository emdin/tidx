#!/usr/bin/env python3
r"""Run the local-chain fixtures: apply setup.sql, then execute the SAME sql
files verbatim through psql and assert each expectation.

    run_local.py <db>      where <db> is a libpq URL for `psql`, or
                           docker:<container>:<user>:<dbname> to exec inside a container

The only rendering difference from run.py: /query's hex rewriter turns a
'0x…' literal into bytea, so here the '\x…'::bytea form is substituted
directly. The SQL text is otherwise byte-identical to what production runs.
Outputs are saved as <fixture>.local.output.csv next to this file.
"""
import csv, io, os, re, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
SQL  = os.path.join(HERE, "..", "..", "sql")
W    = "1111111111111111111111111111111111111111"
A    = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
B    = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

def psql_cmd(db):
    if db.startswith("docker:"):
        _, c, u, d = db.split(":")
        return ["docker", "exec", "-i", c, "psql", "-U", u, "-d", d]
    return ["psql", db]

def run_sql(db, sql, csv_out=True):
    cmd = psql_cmd(db) + (["--csv", "-v", "ON_ERROR_STOP=1"] if csv_out else ["-v", "ON_ERROR_STOP=1"])
    p = subprocess.run(cmd, input=sql, capture_output=True, text=True)
    if p.returncode != 0:
        raise SystemExit(f"psql failed:\n{p.stderr}")
    return p.stdout

def render(name, watched, **num):
    sql = open(os.path.join(SQL, name)).read()
    subs = {"WATCHED": ",".join(f"'\\x{a}'::bytea" for a in watched),
            "WATCHED_TOPICS": ",".join(f"'\\x{'0'*24}{a}'::bytea" for a in watched),
            "TS_LO": "2026-09-28T00:00:00+00:00", "CUR_BLOCK": "-1", "CUR_TX": "-1", "CUR_PATH": "-1",
            "CUR_LOG": "-1", "PAGE_PLUS_1": "101", **{k: str(v) for k, v in num.items()}}
    sql = re.sub(r"\{\{([A-Z_0-9]+)\}\}", lambda m: subs[m.group(1)], sql)
    # mirror /query's hex rewriter for literals baked into the sql (e.g. the Transfer topic0)
    return re.sub(r"'0x([0-9a-fA-F]{40,})'", r"'\\x\1'::bytea", sql)

def rows(db, fixture, sqlfile, watched, **num):
    out = run_sql(db, render(sqlfile, watched, **num))
    open(os.path.join(HERE, f"{fixture}.local.output.csv"), "w").write(out)
    return list(csv.DictReader(io.StringIO(out)))

def check(name, ok, evidence):
    print(f"{'PASS' if ok else 'FAIL'}  {name:58s} {evidence}"); return ok

def main():
    db = sys.argv[1]
    run_sql(db, open(os.path.join(HERE, "setup.sql")).read(), csv_out=False)
    ok = True
    # F4 + F4c
    r = rows(db, "F4_reverted_subtree_and_create2", "q2_internal.sql", [W], BLOCK_LO=999000, BLOCK_HI=999002)
    got = sorted((int(d["trace_path"]), d["amount_raw"], d["call_type"]) for d in r)
    ok &= check("F4  only frames outside reverted subtrees, incl. CREATE2", got == [(5, "7", "CALL"), (9, "13", "CREATE2")], f"got {got}")
    ctrl = run_sql(db, re.sub(r"AND NOT EXISTS \(\s*SELECT 1 FROM reverted rv.*?\)\n", "", render("q2_internal.sql", [W], BLOCK_LO=999000, BLOCK_HI=999002), flags=re.S))
    hidden = sorted(int(d["trace_path"]) for d in csv.DictReader(io.StringIO(ctrl)))
    ok &= check("F4  control without the CTE also returns hidden p2, p8, p11", hidden == [2, 5, 8, 9, 11], f"got {hidden}")
    # F1c same-block forwarding
    r = rows(db, "F1c_sameblock_seedA", "q3_erc20.sql", [A], BLOCK_LO=999002, BLOCK_HI=999002)
    ok &= check("F1c poll with seed A: A→B (tx#0) out", [(d["tx_hash"][-2:], d["direction"], d["amount_raw"]) for d in r] == [("02", "out", "500")], f"got {[(d['tx_hash'][-4:], d['direction']) for d in r]}")
    r = rows(db, "F1c_sameblock_rescanB", "q3_erc20.sql", [B], BLOCK_LO=999002, BLOCK_HI=999002)
    outs = [(d["tx_hash"][-2:], d["amount_raw"]) for d in r if d["direction"] == "out"]
    ok &= check("F1c rescan B, SAME window: B→C (tx#1, later in same block) out", outs == [("03", "200")], f"got {outs}")
    # F10b reorg: orphaned observations with their ORIGINAL block hash
    r = rows(db, "F10b_reorg_q1o", "q1o_native_orphaned.sql", [A], REORG_ID=999, BLOCK_LO=999003, BLOCK_HI=999003)
    ok &= check("F10b q1o returns the orphaned native movements w/ orphaned hash", len(r) == 2 and r[0]["tx_hash"].endswith("04") and all(d["block_hash"].startswith("\\xdead") for d in r), f"got {len(r)} rows, hash {r[0]['block_hash'][:8] if r else '-'}")
    r = rows(db, "F10b_reorg_q3o", "q3o_erc20_orphaned.sql", [A], REORG_ID=999, BLOCK_LO=999003, BLOCK_HI=999003)
    ok &= check("F10b q3o returns the orphaned token movements (900..905) w/ orphaned hash", sorted(d["amount_raw"] for d in r) == [str(n) for n in range(900, 906)] and all(d["block_hash"].startswith("\\xdead") for d in r), f"got {len(r)} rows")
    r = rows(db, "F10b_canonical_after_reorg", "q3_erc20.sql", [A], BLOCK_LO=999003, BLOCK_HI=999003)
    ok &= check("F10b canonical q3 over the same range now finds nothing", len(r) == 0, f"got {len(r)} rows")
    # F10c: more than a page of orphaned rows inside ONE block — keyset pagination of q1o/q2o/q3o
    for q, key, size, want in [("q1o_native_orphaned.sql", ("CUR_BLOCK", "CUR_TX"), 1, 2),
                               ("q2o_internal_orphaned.sql", ("CUR_BLOCK", "CUR_TX", "CUR_PATH"), 2, 3),
                               ("q3o_erc20_orphaned.sql", ("CUR_BLOCK", "CUR_TX", "CUR_LOG"), 4, 6)]:
        col = {"CUR_BLOCK": "block_num", "CUR_TX": "tx_index", "CUR_PATH": "trace_path", "CUR_LOG": "log_index"}
        full = rows(db, f"F10c_{q[:3]}_single", q, [A], REORG_ID=999, BLOCK_LO=999003, BLOCK_HI=999003, PAGE_PLUS_1=101)
        pages, cur, n = [], {k: -1 for k in key}, 0
        while True:
            pg = rows(db, f"F10c_{q[:3]}_page{n+1}", q, [A], REORG_ID=999, BLOCK_LO=999003, BLOCK_HI=999003, PAGE_PLUS_1=size + 1, **cur)
            pages.append(pg[:size]); n += 1
            if len(pg) <= size: break
            cur = {k: pg[size - 1][col[k]] for k in key}
        keys = lambda rs: [tuple(d[col[k]] for k in key) for d in rs]
        flat = [d for p in pages for d in p]
        ok &= check(f"F10c {q[:3]} paginates {want} rows in one block over {n} pages == single query, no overlap",
                    len(full) == want and keys(flat) == keys(full) and len(set(keys(flat))) == len(flat) and all(d["block_hash"].startswith("\\xdead") for d in flat),
                    f"got {len(full)} rows, {n} pages, keys {keys(flat)}")
    print("\nLOCAL ALL PASS" if ok else "\nLOCAL FAILURES"); sys.exit(0 if ok else 1)

if __name__ == "__main__":
    main()
