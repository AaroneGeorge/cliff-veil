#!/usr/bin/env bash
# Token schedules on the public-chain sequencer local-pub, recorded: runs the two token end-to-end tests (a token
# schedule created and claimed publicly in two parts; a token schedule cancelled with its refund, then the vested
# part claimed) against local-pub through a loopback recording relay, and writes evidence/public-chain/<utc>-token/.
#
#   scripts/public_token_run.sh
#
# The command line has no way to mint a token, so the tests' harness (e2e/src/token.rs) makes the definition and
# the holdings. The relay (127.0.0.1:3061 -> local-pub's 127.0.0.1:3051, for this run only) writes every request
# and reply to rpc.jsonl; run.json takes from it the hash and block of every transaction the tests sent and the
# schedule accounts they read, then reads each schedule's final state from the chain. scripts/verify_public.sh
# re-reads all of it. CV_E2E_BIN runs an already built test binary instead of `cargo test`.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/env.sh
source "$here/env.sh"
unset RISC0_DEV_MODE
"$here/seq.sh" pub status >/dev/null || { echo "local-pub is not running: scripts/seq.sh pub start" >&2; exit 1; }
IDS="$CV_REPO/deploy/ids-local-pub.toml"
[ -f "$IDS" ] || { echo "no $IDS: scripts/deploy_local.sh pub --build release" >&2; exit 1; }

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
out="$CV_REPO/evidence/public-chain/$stamp-token"
mkdir -p "$out"
port=3061
TESTS=(f2_token_claim_public_pays_the_destination_holding f3_token_cancel_refunds_the_unvested_part_to_the_refund_holding)

python3 -I - "$port" http://127.0.0.1:3051 "$out/rpc.jsonl" <<'PY' &
import http.client, json, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
port, upstream, path = int(sys.argv[1]), sys.argv[2], sys.argv[3]
host, uport = upstream.split("//")[1].split(":")
lock = threading.Lock()
log = open(path, "a", encoding="utf-8")

class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        c = http.client.HTTPConnection(host, int(uport), timeout=60)
        c.request("POST", "/", body, {"content-type": "application/json"})
        r = c.getresponse()
        data = r.read()
        try:
            req, rep = json.loads(body), json.loads(data)
            if isinstance(req, dict) and req.get("method") in ("sendTransaction", "getTransaction", "getAccountView",
                                                                "getAccount"):
                entry = {"t": time.time(), "method": req["method"],
                         "params": req.get("params") if req["method"] != "sendTransaction" else None}
                res = rep.get("result") if isinstance(rep, dict) else None
                entry["result"] = res[1] if req["method"] == "getTransaction" and res else (
                    res if req["method"] == "sendTransaction" else res is not None)
                if isinstance(rep, dict) and "error" in rep:
                    entry["error"] = rep["error"]
                with lock:
                    log.write(json.dumps(entry) + "\n")
                    log.flush()
        except (ValueError, TypeError, IndexError, KeyError):
            pass
        self.send_response(r.status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
PY
relay=$!
trap 'kill $relay 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do curl -fs -m 1 -o /dev/null -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getLastBlockId","params":[]}' "http://127.0.0.1:$port" && break; sleep 0.2; done

export CV_SEQ_RPC="http://127.0.0.1:$port" CV_IDS="$IDS" CV_E2E_RECORD="$out/record.tsv"
{
  echo "# token schedules on local-pub (LEZ v0.4.0-rc3 standalone, RISC0_DEV_MODE unset), $(date -u +%FT%TZ)"
  echo "# repository commit $(git -C "$CV_REPO" rev-parse --short=12 HEAD); tests run one at a time: ${TESTS[*]}"
  if [ -n "${CV_E2E_BIN:-}" ]; then
    (cd "$CV_REPO/e2e" && "$CV_E2E_BIN" --ignored --exact --test-threads 1 --nocapture "${TESTS[@]}")
  else
    (cd "$CV_REPO" && cargo test --locked -p vesting_e2e --test requirements -- --ignored --exact --test-threads 1 \
      --nocapture "${TESTS[@]}")
  fi
} 2>&1 | tee "$out/run.log"
kill $relay 2>/dev/null || true

python3 -I - "$out" "$IDS" <<'PY'
import json, os, sys, tomllib, urllib.request
out, ids_path = sys.argv[1], sys.argv[2]
ids = tomllib.load(open(ids_path, "rb"))
vesting = ids["programs"]["vesting"]
entries = [json.loads(l) for l in open(os.path.join(out, "rpc.jsonl"), encoding="utf-8")]

def rpc(method, params):
    req = urllib.request.Request("http://127.0.0.1:3051", json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(), {"content-type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=30))["result"]

# Every transaction the tests sent, in order, with the block getTransaction found it in.
sent = [e["result"] for e in entries if e["method"] == "sendTransaction" and e.get("result")]
labels = (["native transfer (test account funding, test f2)"] * 4
          + ["token: new definition, whole supply to the creator", "token: initialise the destination holding",
             "vesting: create a linear token schedule (no cliff)", "vesting: public claim (part, mid-vesting)",
             "vesting: public claim (the rest, after the end)"]
          + ["native transfer (test account funding, test f3)"] * 5
          + ["token: new definition, whole supply to the creator", "token: initialise the refund holding",
             "token: initialise the beneficiary holding",
             "vesting: create a linear token schedule, cancel authority and refund holding set",
             "vesting: cancel (unvested part refunded to the refund holding)",
             "vesting: public claim of the vested part after the cancel"])
if len(sent) != len(labels):
    labels = [f"transaction {i + 1} of the token tests" for i in range(len(sent))]
txs = []
for i, h in enumerate(sent):
    got = rpc("getTransaction", [h])
    txs.append({"step": i + 1, "what": labels[i], "hash": h, "block": got[1] if got else None})

# Schedule accounts: every account read with a vesting schedule state at the end.
def strings(x):
    if isinstance(x, str):
        yield x
    elif isinstance(x, dict):
        for v in x.values():
            yield from strings(v)
    elif isinstance(x, list):
        for v in x:
            yield from strings(v)
seen = []
for e in entries:
    if e["method"] in ("getAccountView", "getAccount"):
        for s in strings(e.get("params")):
            if 32 <= len(s) <= 44 and s not in seen and s not in ids["programs"].values():
                seen.append(s)
schedules = []
for a in seen:
    acc = rpc("getAccount", [a])
    if acc and vesting in acc.get("data", {}).get("actor_states", {}):
        schedules.append({"name": f"token-{len(schedules) + 1}", "account": a, "expect": {"asset": "token"}, "expect_from":
                          "read from the chain at the end of the run (decoded by scripts/verify_public.sh)"})
run = {"what": "token schedules on local-pub: the token end-to-end tests, recorded through a loopback relay",
       "run": os.path.basename(out), "network": ids["deploy"]["network"], "lez": ids["deploy"]["lez"],
       "dev_mode": ids["deploy"]["dev_mode"], "programs": ids["programs"], "accounts": ids.get("accounts", {}),
       "transactions": txs, "refusals": [], "queries": [], "schedules": schedules}
json.dump(run, open(os.path.join(out, "run.json"), "w", encoding="utf-8"), indent=2)
print(f"run.json: {len(txs)} transactions, {len(schedules)} token schedules")
PY
