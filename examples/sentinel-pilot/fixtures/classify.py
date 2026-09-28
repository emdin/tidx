#!/usr/bin/env python3
"""Reference watchlist-expansion classifier for q1/q2/q3 rows (README step 4).

    classify.py --selftest                       offline: stubbed code lookup
    classify.py <config.json> <fixture>...       classify saved fixture outputs with a
                                                 block-pinned eth_getCode via config.rpc_url;
                                                 writes fixtures/C_classify.output.json

Address classification, in order, first match wins:
    zero_address      0x000…0
    exit              config.contracts.canonical_exit
    hyperlane_router  config.contracts.hyperlane_routers.from_fork_fixture
    known_pool        config.contracts.known_dex_or_pool
    token             config.tokens, or the row's own token_address
    contract          eth_getCode(addr, block) != 0x   (pinned to the row's block)
    eoa               eth_getCode(addr, block) == 0x
    unknown           lookup failed -> retained for review, never treated as EOA

Appearing in `called_contract` does NOT make an address a contract, and absence
from config/run does NOT make it an EOA: only the code lookup decides those two.

Decision per row:
    report   direction != 'out', or the sender is not yet watched at that position
    review   recipient is anything but eoa, or the callee is a contract other than
             the two allowed cases below, or any lookup is unknown
    expand   recipient eoa AND callee allowed. Allowed callees:
               * callee == recipient        (q1: plain native payment to a wallet)
               * callee == row.token_address (q3: ordinary ERC-20 transfer(); the
                                              token contract is the callee, the
                                              recipient may still be a wallet)
"""
import json, os, sys, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ZERO = "0x" + "0" * 40

def rpc_get_code(rpc_url):
    cache = {}
    def get_code(addr, block):
        key = (addr, block)
        if key in cache: return cache[key]
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "eth_getCode", "params": [addr, hex(block)]}).encode()
        try:
            req = urllib.request.Request(rpc_url, data=body, headers={"content-type": "application/json"})
            r = json.loads(urllib.request.urlopen(req, timeout=15).read())
            code = r["result"]
            cache[key] = "eoa" if code == "0x" else "contract"
        except Exception as e:                      # any failure -> unknown, never eoa
            cache[key] = "unknown"
        return cache[key]
    return get_code

def classify(addr, block, cfg, get_code, token=None):
    if addr is None: return "none"
    a = addr.lower(); c = cfg["contracts"]
    if a == ZERO: return "zero_address"
    if a == c["canonical_exit"].lower(): return "exit"
    if a in [r.lower() for r in c["hyperlane_routers"]["from_fork_fixture"]]: return "hyperlane_router"
    if a in [k.lower() for k in c["known_dex_or_pool"] if k.startswith("0x")]: return "known_pool"
    if a in [k.lower() for k in cfg["tokens"] if k.startswith("0x")] or (token and a == token.lower()): return "token"
    return get_code(a, block)                       # contract | eoa | unknown

def position(row):
    return (row["block_num"], row["tx_index"], row.get("log_index") if row.get("log_index") is not None else row.get("trace_path") or -1)

def decide(row, cfg, get_code, watch_from):
    """watch_from: {address: (block, tx_index, sub)} — position from which each address is watched."""
    if row["direction"] != "out":
        return "report", "inbound row; the payer is never added"
    sender = row["from_address"].lower()
    if sender not in watch_from or position(row) < tuple(watch_from[sender]):
        return "report", "sender not watched at this position (history)"
    if int(row["amount_raw"]) <= 0:
        return "report", "non-positive amount"
    to, callee, token = row["to_address"], row.get("called_contract"), row.get("token_address")
    rc = classify(to, row["block_num"], cfg, get_code, token)
    if rc != "eoa":
        return "review", f"recipient {to} is {rc}"
    if callee is None or callee.lower() == to.lower():
        return "expand", "native payment to a wallet (callee == recipient, eoa by code lookup)"
    if token and callee.lower() == token.lower():
        return "expand", "ordinary ERC-20 transfer (callee == token contract), recipient eoa by code lookup"
    cc = classify(callee, row["block_num"], cfg, get_code, token)
    return "review", f"callee {callee} is {cc}"

# ---------------------------------------------------------------------------
def selftest():
    cfg = {"tokens": {"0x17ec7e1768c813e2a3a9b0f94a35605ca520c242": {}},
           "contracts": {"canonical_exit": "0x4bb88c213d3ed9dc4bae694f1bc1bf745903b2d0",
                         "hyperlane_routers": {"from_fork_fixture": ["0xa5b8bf902b2844da17d4506cc827f7f1681735e7"]},
                         "known_dex_or_pool": {"0xbe3c61c2f57f78cdbaf35f0c03c3c8a680cce7bd": ""}}}
    code = {"0xb0b0000000000000000000000000000000000000": "eoa", "0xc0de000000000000000000000000000000000000": "contract"}
    get_code = lambda a, b: code.get(a, "unknown")
    W = "0xa000000000000000000000000000000000000000"; wf = {W: (100, 0, -1)}
    base = dict(block_num=200, tx_index=0, log_index=None, trace_path=None, direction="out", from_address=W, amount_raw="5", token_address=None)
    cases = [
        ("native to wallet -> expand",              dict(base, to_address="0xb0b0000000000000000000000000000000000000", called_contract="0xb0b0000000000000000000000000000000000000"), "expand"),
        ("erc20 transfer(), wallet recipient -> expand", dict(base, to_address="0xb0b0000000000000000000000000000000000000", called_contract="0x17ec7e1768c813e2a3a9b0f94a35605ca520c242", token_address="0x17ec7e1768c813e2a3a9b0f94a35605ca520c242"), "expand"),
        ("erc20 via router callee -> review",       dict(base, to_address="0xb0b0000000000000000000000000000000000000", called_contract="0xc0de000000000000000000000000000000000000", token_address="0x17ec7e1768c813e2a3a9b0f94a35605ca520c242"), "review"),
        ("recipient is a pool -> review",           dict(base, to_address="0xbe3c61c2f57f78cdbaf35f0c03c3c8a680cce7bd", called_contract="0xbe3c61c2f57f78cdbaf35f0c03c3c8a680cce7bd"), "review"),
        ("recipient is exit contract -> review",    dict(base, to_address="0x4bb88c213d3ed9dc4bae694f1bc1bf745903b2d0", called_contract="0x4bb88c213d3ed9dc4bae694f1bc1bf745903b2d0"), "review"),
        ("burn to zero address -> review",          dict(base, to_address=ZERO, called_contract="0xa5b8bf902b2844da17d4506cc827f7f1681735e7", token_address="0xa5b8bf902b2844da17d4506cc827f7f1681735e7"), "review"),
        ("native to unlisted contract -> review",   dict(base, to_address="0xc0de000000000000000000000000000000000000", called_contract="0xc0de000000000000000000000000000000000000"), "review"),
        ("code lookup failed -> review, not eoa",   dict(base, to_address="0x9999000000000000000000000000000000000000", called_contract="0x9999000000000000000000000000000000000000"), "review"),
        ("inbound -> report",                       dict(base, direction="in", from_address="0xb0b0000000000000000000000000000000000000", to_address=W, called_contract=W), "report"),
        ("before watch start -> report",            dict(base, block_num=50, to_address="0xb0b0000000000000000000000000000000000000", called_contract="0xb0b0000000000000000000000000000000000000"), "report"),
        ("same block, before watch tx -> report",   dict(base, block_num=100, tx_index=0, to_address="0xb0b0000000000000000000000000000000000000", called_contract="0xb0b0000000000000000000000000000000000000"), "report"),
    ]
    wf2 = {W: (100, 1, -1)}
    ok = True
    for name, row, want in cases:
        got, why = decide(row, cfg, get_code, wf2 if name.startswith("same block") else wf)
        ok &= got == want; print(f"{'PASS' if got == want else 'FAIL'}  {name:48s} {got:7s} {why}")
    print("\nSELFTEST PASS" if ok else "\nSELFTEST FAIL"); return ok

def main():
    if sys.argv[1:] == ["--selftest"]:
        sys.exit(0 if selftest() else 1)
    cfg = json.load(open(sys.argv[1])); get_code = rpc_get_code(cfg["rpc_url"]); out = []
    for f in sys.argv[2:]:
        rec = json.load(open(f)); r = rec["response"]
        params = json.load(open(f.replace(".output.json", ".params.json")))
        wf = {a.lower(): (params["BLOCK_LO"], -1, -1) for a in params["WATCHED"]}
        for x in r["rows"]:
            row = dict(zip(r["columns"], x)); d, why = decide(row, cfg, get_code, wf)
            out.append({"fixture": os.path.basename(f), "tx_hash": row["tx_hash"], "block_num": row["block_num"],
                        "direction": row["direction"], "to_address": row["to_address"], "called_contract": row.get("called_contract"),
                        "decision": d, "reason": why})
            print(f"{os.path.basename(f):48s} {row['tx_hash'][:12]} {d:7s} {why}")
    json.dump(out, open(os.path.join(HERE, "C_classify.output.json"), "w"), indent=1)

if __name__ == "__main__":
    main()
