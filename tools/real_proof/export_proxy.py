"""A local JSON-RPC proxy that lets the wallet build and prove a transaction against a read-only endpoint, and keeps
the transaction instead of sending it.

    python3 -I tools/real_proof/export_proxy.py <listen port> <upstream url> <out dir>

The wallet (and the cliff-veil CLI over it) is pointed at http://127.0.0.1:<listen port>. Every request except
`sendTransaction` is forwarded to <upstream url> unchanged, one request at a time, paced to 4 a second (the public
endpoint allows 5 a second per visitor), retried on HTTP 429/503/504. A `sendTransaction` request is written as it
came to <out dir>/send-<n>.json, byte for byte, and answered with a JSON-RPC error, so nothing reaches any sequencer.
The saved file is exactly what the wallet would have posted; posting it to the sequencer later submits that
transaction (tools/real_proof/submit_exported.sh).

<out dir>/methods.tsv logs every request: time, method, outcome (forwarded, refused upstream with its code, exported).
Batches are split into single requests (the public endpoint takes one request per call). Standard library only.
"""

import http.server
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request

PACE = 0.25          # seconds between upstream requests
RETRIES = 6
EXPORTED_CODE = -32099
lock = threading.Lock()
state = {"last": 0.0}


def log(out, method, outcome):
    with open(os.path.join(out, "methods.tsv"), "a", encoding="utf-8") as f:
        f.write(f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\t{method}\t{outcome}\n")


def forward(upstream, body):
    """POSTs one request upstream, paced and retried. Returns the reply bytes."""
    delay = 1.0
    for attempt in range(RETRIES):
        with lock:
            wait = state["last"] + PACE - time.monotonic()
            if wait > 0:
                time.sleep(wait)
            state["last"] = time.monotonic()
        req = urllib.request.Request(upstream, body, {"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return r.read()
        except urllib.error.HTTPError as e:
            if e.code in (429, 503, 504) and attempt + 1 < RETRIES:
                time.sleep(delay)
                delay *= 2
                continue
            raise
        except (urllib.error.URLError, TimeoutError):
            if attempt + 1 < RETRIES:
                time.sleep(delay)
                delay *= 2
                continue
            raise
    raise RuntimeError("unreachable")


def handle_one(req, upstream, out, sent, raw=None):
    """One JSON-RPC request object -> reply object. `raw`: the request's own bytes when it came alone."""
    method = req.get("method") if isinstance(req, dict) else None
    req_id = req.get("id") if isinstance(req, dict) else None
    if method == "sendTransaction":
        with lock:
            sent["n"] += 1
            n = sent["n"]
        path = os.path.join(out, f"send-{n}.json")
        with open(path, "wb") as f:
            f.write(raw if raw is not None else json.dumps(req, separators=(",", ":")).encode())
        log(out, method, f"exported to send-{n}.json")
        return {"jsonrpc": "2.0", "id": req_id,
                "error": {"code": EXPORTED_CODE, "message": f"exported to send-{n}.json, not sent (export proxy)"}}
    try:
        reply = json.loads(forward(upstream, json.dumps(req, separators=(",", ":")).encode()))
    except Exception as e:  # noqa: BLE001 - every failure becomes a JSON-RPC error the wallet can report
        log(out, method, f"upstream failure: {e}")
        return {"jsonrpc": "2.0", "id": req_id, "error": {"code": -32603, "message": f"export proxy upstream: {e}"}}
    err = reply.get("error") if isinstance(reply, dict) else None
    log(out, method, f"refused upstream {err.get('code')}: {err.get('message')}" if err else "forwarded")
    return reply


def make_handler(upstream, out):
    sent = {"n": 0}  # sendTransaction requests kept by this server

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_POST(self):  # noqa: N802 - http.server naming
            body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            try:
                req = json.loads(body)
            except ValueError:
                reply = {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "parse error"}}
            else:
                if isinstance(req, list):
                    reply = [handle_one(r, upstream, out, sent) for r in req]
                else:
                    reply = handle_one(req, upstream, out, sent, body)
            data = json.dumps(reply, separators=(",", ":")).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *args):
            pass

    return Handler


def main():
    port, upstream, out = int(sys.argv[1]), sys.argv[2], sys.argv[3]
    os.makedirs(out, exist_ok=True)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), make_handler(upstream, out))
    print(f"export proxy on 127.0.0.1:{port} -> {upstream}; sendTransaction kept in {out}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
