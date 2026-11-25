#!/usr/bin/env bash
# Re-reads the public-chain evidence from a sequencer endpoint and prints PASS or FAIL.
#
#   scripts/verify_public.sh [URL] [RUN_JSON...]
#
#   scripts/verify_public.sh --public [URL] [RUN_JSON...]
#
#   URL       JSON-RPC endpoint (default: the public read-only endpoint, CV_PUBLIC_URL overrides it)
#   RUN_JSON  run files to check (default: every evidence/public-chain/*/run.json)
#   --public  connect to the address public DNS (1.1.1.1, with dig) gives for the host, so the requests take the
#             internet route even from a machine whose own resolver knows a private address for it
#
# For every run file:
#   1. every program of the run (token, vesting_fees, ata, vesting, the fee config) exists on the chain;
#   2. every transaction is found by getTransaction and sits in the block the run recorded;
#   3. every schedule account is read with getAccount, its state decoded from the Borsh layout of SPEC 2 (by this
#      script, independently of the SDK) and compared with what the run recorded (creator, beneficiary, asset, total,
#      claimed, kind and milestones signalled, cancelable, transferable, authorities, refund account, cancellation),
#      and its native escrow balance is checked to be total - claimed - refunded;
#   4. the endpoint is read-only: sendTransaction is refused with -32601 (skipped for a loopback URL, where the
#      sequencer itself answers).
# Standard library only (Python 3.11+). Exit 0 when every check passes, 1 otherwise.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
via=""
if [ "${1:-}" = --public ]; then via=public; shift; fi
url="${1:-${CV_PUBLIC_URL:-https://cliff-veil-seq.tailcdec7a.ts.net}}"
[ $# -gt 0 ] && shift
if [ $# -eq 0 ]; then
  set -- "$repo"/evidence/public-chain/*/run.json
fi
connect_ip=""
if [ "$via" = public ]; then
  host="$(sed -E 's#^[a-z]+://([^/:]+).*#\1#' <<< "$url")"
  connect_ip="$(dig +short @1.1.1.1 "$host" A | grep -E '^[0-9.]+$' | tail -1)"
  [ -n "$connect_ip" ] || { echo "public DNS has no address for $host" >&2; exit 1; }
fi
export CV_CONNECT_IP="$connect_ip"
exec python3 -I - "$url" "$@" <<'PY'
import base64
import http.client
import json
import os
import socket
import ssl
import sys
import time
import urllib.parse

URL, RUNS = sys.argv[1], sys.argv[2:]
CONNECT_IP = os.environ.get("CV_CONNECT_IP", "")


class PinnedHTTPS(http.client.HTTPSConnection):
    """HTTPS to a given address, with the certificate checked for the URL's host name."""

    def connect(self):
        sock = socket.create_connection((CONNECT_IP, self.port or 443), self.timeout)
        self.sock = ssl.create_default_context().wrap_socket(sock, server_hostname=self.host)
NATIVE = "11111111111111111111111111111111"
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58(raw: bytes) -> str:
    n = int.from_bytes(raw, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = B58[r] + s
    return "1" * (len(raw) - len(raw.lstrip(b"\0"))) + s


class Rpc:
    """JSON-RPC over one kept-alive connection; paced under the endpoint's per-visitor rate (5 a second)."""

    def __init__(self, url):
        u = urllib.parse.urlsplit(url)
        self.https, self.host, self.port = u.scheme == "https", u.hostname, u.port
        self.path = u.path or "/"
        self.conn, self.last, self.calls = None, 0.0, 0

    def _connect(self):
        cls = (PinnedHTTPS if CONNECT_IP else http.client.HTTPSConnection) if self.https else http.client.HTTPConnection
        self.conn = cls(self.host, self.port, timeout=30)

    def raw(self, method, params):
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
        for attempt in range(4):
            wait = self.last + 0.25 - time.monotonic()
            if wait > 0:
                time.sleep(wait)
            self.last = time.monotonic()
            try:
                if self.conn is None:
                    self._connect()
                self.conn.request("POST", self.path, body, {"content-type": "application/json"})
                r = self.conn.getresponse()
                data = r.read()
                if r.status == 429 or r.status >= 500:
                    time.sleep(1 + attempt)
                    continue
                self.calls += 1
                return r.status, json.loads(data) if data else None
            except (OSError, http.client.HTTPException, json.JSONDecodeError):
                self.conn = None
                time.sleep(1 + attempt)
        raise RuntimeError(f"{method}: no answer from {URL}")

    def call(self, method, params):
        status, doc = self.raw(method, params)
        if status != 200 or doc is None or "error" in doc:
            raise RuntimeError(f"{method}{params}: HTTP {status} {doc}")
        return doc["result"]


class Reader:
    def __init__(self, b):
        self.b, self.i = bytes(b), 0

    def take(self, n):
        v = self.b[self.i:self.i + n]
        if len(v) != n:
            raise ValueError("schedule state too short")
        self.i += n
        return v

    def u8(self):
        return self.take(1)[0]

    def uint(self, n):
        return int.from_bytes(self.take(n), "little")

    def acc(self):
        return b58(self.take(32))

    def boolean(self):
        v = self.u8()
        if v > 1:
            raise ValueError("bad bool")
        return bool(v)


def decode_schedule(raw):
    """ScheduleState (SPEC 2), Borsh."""
    r = Reader(raw)
    if r.u8() != 0:
        raise ValueError("unknown ScheduleState version")
    s = {"schedule_id": r.take(32).hex(), "creator": r.acc(), "beneficiary": r.acc()}
    tag = r.u8()
    s["asset"], s["token_definition"] = ("native", None) if tag == 0 else ("token", r.acc())
    s["total"], s["claimed"], s["last_time"] = r.uint(16), r.uint(16), r.uint(8)
    tag = r.u8()
    if tag == 0:
        s["kind_detail"] = {"linear": {"start": r.uint(8), "cliff": r.uint(8), "end": r.uint(8)}}
    elif tag == 1:
        amounts = [r.uint(16) for _ in range(r.uint(4))]
        s["kind_detail"] = {"milestones": {"amounts": amounts, "signalled": r.uint(8)}}
    else:
        raise ValueError("unknown Kind")
    s["cancelable"], s["transferable"] = r.boolean(), r.boolean()
    s["cancel_authority"], s["milestone_authority"], s["refund_to"] = r.acc(), r.acc(), r.acc()
    s["cancelled"] = {"at_ms": r.uint(8), "vested_at_cancel": r.uint(16), "refunded": r.uint(16)} if r.u8() else None
    if r.u8():
        if r.u8() == 0:
            r.take(32)
        s["pending"] = True
    else:
        s["pending"] = None
    if r.i != len(r.b):
        raise ValueError(f"{len(r.b) - r.i} trailing bytes")
    return s


def norm(v):
    """Amounts are strings in the run file and integers once decoded."""
    if isinstance(v, str) and v.isdigit():
        return int(v)
    if isinstance(v, dict):
        return {k: norm(x) for k, x in v.items()}
    if isinstance(v, list):
        return [norm(x) for x in v]
    return v


def compare(expect, got):
    bad = []
    for k, want in expect.items():
        if k == "standing":
            continue
        have = got.get(k)
        if k == "cancelled" and want is not None:
            want = {x: want[x] for x in ("at_ms", "vested_at_cancel", "refunded")}
        if norm(want) != norm(have):
            bad.append(f"{k}: recorded {want!r}, chain {have!r}")
    return bad


rpc = Rpc(URL)
failures, counts = [], {"programs": 0, "transactions": 0, "schedules": 0}
loopback = urllib.parse.urlsplit(URL).hostname in ("127.0.0.1", "localhost", "::1")
print(f"endpoint {URL}" + (f" (connected to {CONNECT_IP}, the address public DNS gives)" if CONNECT_IP else ""))
print(f"chain head: block {rpc.call('getLastBlockId', [])}")

for path in RUNS:
    run = json.load(open(path, encoding="utf-8"))
    print(f"\n{run['run']}: {len(run['transactions'])} transactions, {len(run['schedules'])} schedules "
          f"({run['network']}, LEZ {run['lez']}, dev mode {run['dev_mode']})")
    vesting = run["programs"]["vesting"]

    for name, account in list(run["programs"].items()) + list(run.get("accounts", {}).items()):
        a = rpc.call("getAccount", [account])
        ok = a is not None
        counts["programs"] += 1
        if not ok:
            failures.append(f"{run['run']}: {name} {account} not found")
        print(f"  {'ok  ' if ok else 'FAIL'} {name:<13} {account}")

    for tx in run["transactions"]:
        got = rpc.call("getTransaction", [tx["hash"]])
        counts["transactions"] += 1
        if got is None:
            failures.append(f"{run['run']}: tx {tx['hash']} not found")
            print(f"  FAIL tx {tx['hash']} not found ({tx['what']})")
            continue
        body, block = got
        ok = block == tx["block"] and len(base64.b64decode(body)) > 0
        if not ok:
            failures.append(f"{run['run']}: tx {tx['hash']} in block {block}, recorded {tx['block']}")
        print(f"  {'ok  ' if ok else 'FAIL'} tx {tx['hash'][:16]}.. block {block:>6}  {tx['what']}"
              + (f" ({tx['part']})" if "part" in tx else ""))

    for s in run["schedules"]:
        counts["schedules"] += 1
        a = rpc.call("getAccount", [s["account"]])
        states = (a or {}).get("data", {}).get("actor_states", {})
        if vesting not in states:
            failures.append(f"{run['run']}: schedule {s['name']} {s['account']} has no vesting state")
            print(f"  FAIL schedule {s['name']}: no vesting state at {s['account']}")
            continue
        try:
            st = decode_schedule(states[vesting])
        except ValueError as e:
            failures.append(f"{run['run']}: schedule {s['name']}: {e}")
            print(f"  FAIL schedule {s['name']}: {e}")
            continue
        bad = compare(s["expect"], st)
        escrow = int.from_bytes(bytes(states.get(NATIVE, [])), "little")
        refunded = (st["cancelled"] or {}).get("refunded", 0)
        if st["asset"] == "native" and escrow != st["total"] - st["claimed"] - refunded:
            bad.append(f"escrow {escrow} != total {st['total']} - claimed {st['claimed']} - refunded {refunded}")
        if st["pending"] is not None:
            bad.append("pending is set between transactions")
        kind = next(iter(st["kind_detail"]))
        detail = (f"signalled {st['kind_detail']['milestones']['signalled']:#b}" if kind == "milestones" else
                  f"cancelled, refunded {refunded}" if st["cancelled"] else
                  f"cancelable {str(st['cancelable']).lower()}")
        held = (f"escrow {escrow}" if st["asset"] == "native" else
                f"token {st['token_definition'][:8]}.. (escrow in the schedule's token holding)")
        print(f"  {'ok  ' if not bad else 'FAIL'} schedule {s['name']:<11} {s['account']}  {kind}, total {st['total']}, "
              f"claimed {st['claimed']}, {held}, {detail}")
        for b in bad:
            failures.append(f"{run['run']}: schedule {s['name']}: {b}")
            print(f"       {b}")

if loopback:
    print("\nread-only check skipped: loopback URL (the sequencer itself answers)")
else:
    status, doc = rpc.raw("sendTransaction", [])
    code = ((doc or {}).get("error") or {}).get("code")
    ok = status == 200 and code == -32601
    print(f"\n{'ok  ' if ok else 'FAIL'} sendTransaction refused by the endpoint: HTTP {status}, code {code}")
    if not ok:
        failures.append("sendTransaction was not refused with -32601")

print(f"\nchecked {counts['programs']} program and config accounts, {counts['transactions']} transactions, "
      f"{counts['schedules']} schedules with {rpc.calls} requests")
if failures:
    print(f"FAIL ({len(failures)})")
    for f in failures:
        print(f"  {f}")
    sys.exit(1)
print("PASS")
PY
