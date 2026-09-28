#!/usr/bin/env python3
"""Run a sentinel-pilot SQL file against tidx /query and preserve input + output.

    run.py <sql-file> <params.json> [--out <name>] [--engine postgres|clickhouse]
           [--timeout-ms N] [--base https://tidx.igralabs.com]

* Every {{PLACEHOLDER}} in the SQL is substituted from params.json.
* Address placeholders are VALIDATED before substitution (this is the bot's
  injection boundary — /query has no bind parameters; tidx's AST validator is
  the second line of defence):
    WATCHED        -> list of 0x + 40 hex   -> quoted, comma-joined
    WATCHED_TOPICS -> same list, left-padded to 32 bytes, lowercased
  Numeric placeholders must be integers; TS_LO must look like a timestamp.

Evidence preservation (the point of this script):
* <out>.input.json is written BEFORE the request is attempted, so an attempt
  that never gets a response still leaves its exact input on disk.
* <out>.output.json records every outcome: HTTP status + JSON body; an HTTP
  error with a JSON body (tidx validator rejections are 4xx); a non-JSON
  body; a network/timeout failure with no response at all. `outcome` is one of
  ok | api_error | http_error | malformed_response | transport_error.
* Exit code 0 only for outcome=ok. A timeout, row cap, validator rejection,
  transport failure or malformed body is never an empty result.
"""
import json, re, sys, time, urllib.error, urllib.request

ADDR = re.compile(r"^0x[0-9a-fA-F]{40}$")
TS   = re.compile(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(\+00:00|Z)?$")

def addr_list(vals):
    out = []
    for a in vals:
        if not ADDR.match(a):
            raise SystemExit(f"invalid address {a!r}")
        out.append(a.lower())
    return out

def render(sql, p):
    subs = {}
    if "WATCHED" in p:
        w = addr_list(p["WATCHED"])
        subs["WATCHED"] = ",".join(f"'{a}'" for a in w)
        subs["WATCHED_TOPICS"] = ",".join(f"'0x{'0'*24}{a[2:]}'" for a in w)
    for k, v in p.items():
        if k == "WATCHED":
            continue
        if k == "TS_LO":
            if not TS.match(str(v)):
                raise SystemExit(f"bad TS_LO {v!r}")
            subs[k] = str(v)
        elif isinstance(v, bool):
            raise SystemExit(f"bool not allowed for {k}")
        elif isinstance(v, int) or (isinstance(v, str) and re.fullmatch(r"-?\d+", v)):
            subs[k] = str(v)
        else:
            raise SystemExit(f"unsupported param {k}={v!r} (ints, TS_LO, WATCHED only)")
    def repl(m):
        key = m.group(1)
        if key not in subs:
            raise SystemExit(f"missing param {key}")
        return subs[key]
    return re.sub(r"\{\{([A-Z_0-9]+)\}\}", repl, sql)

def post(base, q):
    """Returns (outcome, http_status_or_None, body_or_None, raw_text_or_None)."""
    req = urllib.request.Request(f"{base}/query", data=json.dumps(q).encode(),
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            status, raw = r.status, r.read().decode(errors="replace")
    except urllib.error.HTTPError as e:
        status, raw = e.code, e.read().decode(errors="replace")
    except Exception as e:                      # URLError, socket timeout, ...
        return "transport_error", None, {"ok": False, "error": f"{type(e).__name__}: {e}"}, None
    try:
        body = json.loads(raw)
    except ValueError:
        return "malformed_response", status, {"ok": False, "error": f"non-JSON body (HTTP {status})"}, raw[:2000]
    if not isinstance(body, dict) or "ok" not in body:
        return "malformed_response", status, {"ok": False, "error": "JSON without an ok field", "body": body}, None
    if body.get("ok"):
        return "ok", status, body, None
    return ("api_error" if status < 400 else "http_error"), status, body, None

def main():
    args = sys.argv[1:]
    sql_file, params_file = args[0], args[1]
    opt = {"out": None, "engine": "postgres", "timeout-ms": "30000", "base": "https://tidx.igralabs.com"}
    i = 2
    while i < len(args):
        opt[args[i].lstrip("-")] = args[i + 1]; i += 2
    out = opt["out"] or re.sub(r"\.params\.json$", "", params_file)
    sql = open(sql_file).read()
    params = json.load(open(params_file))
    rendered = render(sql, params)
    q = {"sql": rendered, "chainId": 38833, "timeout_ms": int(opt["timeout-ms"]), "engine": opt["engine"]}

    # Input first: an attempt that never returns still leaves its exact evidence.
    attempted_at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    json.dump({"sql_file": sql_file, "params": params, "rendered_sql": rendered, "request": q,
               "attempted_at": attempted_at}, open(f"{out}.input.json", "w"), indent=2)

    t0 = time.time()
    outcome, status, body, raw = post(opt["base"], q)
    wall = round((time.time() - t0) * 1000, 1)
    rec = {"outcome": outcome, "http_status": status, "wall_ms": wall, "attempted_at": attempted_at, "response": body}
    if raw is not None:
        rec["raw_body_prefix"] = raw
    json.dump(rec, open(f"{out}.output.json", "w"), indent=2)

    ok = outcome == "ok"
    print(f"{out}: outcome={outcome} http={status} rows={body.get('row_count') if body else None} "
          f"server_ms={round((body or {}).get('query_time_ms') or 0, 1)} wall_ms={wall}"
          + ("" if ok else f" ERROR={(body or {}).get('error')}"))
    sys.exit(0 if ok else 1)

if __name__ == "__main__":
    main()
