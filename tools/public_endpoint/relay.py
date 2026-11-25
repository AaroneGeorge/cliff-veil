#!/usr/bin/env python3
"""The public half of the read-only sequencer endpoint: a queue, not a proxy.

    python3 -I relay.py          (in the app container; env RELAY_TOKEN, at least 32 characters)

Port 8080 (public, behind the HTTPS sidecar): `POST /` one JSON-RPC request; `GET /` help text; `GET /health`.
Each request is checked with policy.check_request, then queued. Port 8081 (only the host dialer, bearer token):
`GET /pull` hands out the next queued request, `POST /push` returns its reply. The relay never opens a connection
itself: the host dialer on the sequencer's host dials in, checks every request again, and calls the sequencer.
Standard library only.
"""

import collections
import hmac
import ipaddress
import json
import os
import secrets
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import policy  # noqa: E402

PUBLIC_PORT, DIALER_PORT = 8080, 8081
PULL_WAIT = 25.0          # seconds a /pull waits for a job
DIALER_GRACE = 5.0         # seconds after the last /pull the host dialer still counts as present
MAX_PUBLIC_CONNECTIONS, MAX_DIALER_CONNECTIONS = 128, 8

HELP = f"""Read-only JSON-RPC endpoint of a LEZ sequencer (local-pub, LEZ v0.4.0-rc3, a standalone sequencer).

POST / with one JSON-RPC 2.0 request, content-type application/json. Forwarded methods:
  {", ".join(sorted(policy.ALLOWED))}
Everything else is refused with -32601, including sendTransaction.
Limits: {policy.MAX_BODY} bytes per request, one request per body (no batches), getBlockRange at most
{policy.MAX_RANGE} blocks, getAccountsNonces at most {policy.MAX_NONCES} ids, getProofsAndRoot at most {policy.MAX_PROOFS} entries per
list, getPublications a limit of 1 to {policy.MAX_PUBLICATIONS}, replies at most {policy.MAX_REPLY} bytes,
{policy.PER_VISITOR_RATE:g} requests a second per visitor (burst {policy.PER_VISITOR_BURST:g}; a little over is slowed by up to
{policy.MAX_DELAY:g} s, more gets HTTP 429), {policy.TOTAL_TIMEOUT:g} s per request.

Example:
  curl -s -H 'content-type: application/json' -d '{{"jsonrpc":"2.0","id":1,"method":"getLastBlockId","params":[]}}' URL
""".encode()


def log(*a):
    print(time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), *a, flush=True)


class Job:
    __slots__ = ("id", "method", "body", "done", "status", "reply", "taken")

    def __init__(self, method, body):
        self.id, self.method, self.body = secrets.token_hex(8), method, body
        self.done, self.status, self.reply, self.taken = threading.Event(), 0, b"", False


class Hub:
    """The queue between visitors and the host dialer."""

    def __init__(self):
        self.cond = threading.Condition()
        self.waiting = collections.deque()
        self.jobs = {}
        self.pollers = 0
        self.last_poll = 0.0

    def dialer_present(self):
        with self.cond:
            return self.pollers > 0 or time.monotonic() - self.last_poll < DIALER_GRACE

    def submit(self, job):
        with self.cond:
            if len(self.waiting) >= policy.MAX_WAITING:
                return False
            self.waiting.append(job)
            self.jobs[job.id] = job
            self.cond.notify()
            return True

    def forget(self, job):
        with self.cond:
            self.jobs.pop(job.id, None)
            try:
                self.waiting.remove(job)
            except ValueError:
                pass

    def take(self, wait):
        deadline = time.monotonic() + wait
        with self.cond:
            self.pollers += 1
            try:
                while True:
                    while self.waiting:
                        job = self.waiting.popleft()
                        if job.id in self.jobs:
                            job.taken = True
                            return job
                    left = deadline - time.monotonic()
                    if left <= 0:
                        return None
                    self.cond.wait(left)
            finally:
                self.pollers -= 1
                self.last_poll = time.monotonic()

    def finish(self, job_id, status, reply):
        with self.cond:
            job = self.jobs.pop(job_id, None)
        if job is None:
            return False
        job.status, job.reply = status, reply
        job.done.set()
        return True


HUB = Hub()
LIMITER = policy.RateLimiter()
TOKEN = os.environ.get("RELAY_TOKEN", "").strip()


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 64

    def __init__(self, addr, handler, max_connections):
        self.slots = threading.BoundedSemaphore(max_connections)
        super().__init__(addr, handler)

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        super().process_request(request, client_address)

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()


class Base(BaseHTTPRequestHandler):
    server_version = "seq-read"
    sys_version = ""
    timeout = 15  # socket timeout: a slow sender is dropped

    def log_message(self, *a):  # no per-request lines with addresses
        pass

    def send(self, status, body, ctype="application/json"):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def length(self):
        """Content-Length as int, or None when missing or not a number (chunked bodies are not accepted)."""
        if self.headers.get("Transfer-Encoding"):
            return None
        try:
            n = int(self.headers.get("Content-Length", ""))
        except ValueError:
            return None
        return n if n >= 0 else None


class Public(Base):
    def visitor(self):
        """The rate-limit key: the visitor's address as the sidecar's proxy gives it (the last X-Forwarded-For entry;
        earlier entries come from the visitor), else the peer address. IPv6 is keyed by its /64, which one visitor
        usually holds whole."""
        last = self.headers.get("X-Forwarded-For", "").rsplit(",", 1)[-1].strip()
        for text in (last, self.client_address[0]):
            try:
                ip = ipaddress.ip_address(text)
            except ValueError:
                continue
            if ip.version == 6:
                if ip.ipv4_mapped:
                    return str(ip.ipv4_mapped)
                return str(ipaddress.ip_network(f"{ip}/64", strict=False))
            return str(ip)
        return "unknown"

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path == "/":
            self.send(200, HELP, "text/plain; charset=utf-8")
        elif path == "/health":
            self.send(200, json.dumps({"ok": True, "dialer": HUB.dialer_present()}).encode())
        else:
            self.send(404, b'{"error":"not found"}')

    do_HEAD = do_GET

    def do_OPTIONS(self):
        self.send_response(204)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "POST, GET")
        self.send_header("Access-Control-Allow-Headers", "content-type")
        self.send_header("Content-Length", "0")
        self.end_headers()

    def not_allowed(self):
        self.send(405, b'{"error":"method not allowed"}')

    do_PUT = do_DELETE = do_PATCH = not_allowed

    def do_POST(self):
        if self.path.split("?", 1)[0] != "/":
            self.send(404, b'{"error":"not found"}')
            return
        wait = LIMITER.reserve(self.visitor())
        if wait is None:
            self.send(429, policy.error_body(-32000, "too many requests"))
            return
        if wait:
            time.sleep(wait)
        n = self.length()
        if n is None:
            self.send(411, policy.error_body(policy.INVALID_REQUEST, "Content-Length required"))
            return
        if n > policy.MAX_BODY:
            self.send(413, policy.error_body(policy.INVALID_REQUEST, f"request over {policy.MAX_BODY} bytes"))
            return
        body = self.rfile.read(n)
        try:
            method, canonical = policy.check_request(body)
        except policy.Refused as r:
            log("refused", r.code)
            self.send(200, policy.error_body(r.code, r.message, r.req_id))
            return
        if not HUB.dialer_present():
            self.send(503, policy.error_body(-32000, "the sequencer side is away, try again later"))
            return
        job = Job(method, canonical)
        if not HUB.submit(job):
            self.send(503, policy.error_body(-32000, "busy, try again"))
            return
        if not job.done.wait(policy.TOTAL_TIMEOUT):
            HUB.forget(job)
            self.send(504, policy.error_body(-32000, "the sequencer did not answer in time"))
            return
        self.send(job.status, job.reply)


class Dialer(Base):
    def authorised(self):
        got = self.headers.get("Authorization", "")
        ok = len(TOKEN) >= 32 and hmac.compare_digest(got.encode(), f"Bearer {TOKEN}".encode())
        if not ok:
            self.send(401, b'{"error":"unauthorised"}')
        return ok

    def do_GET(self):
        if self.path.split("?", 1)[0] != "/pull":
            self.send(404, b'{"error":"not found"}')
            return
        if not self.authorised():
            return
        job = HUB.take(PULL_WAIT)
        if job is None:
            self.send(204, b"")
            return
        self.send(200, json.dumps({"job": job.id, "body": job.body.decode()}).encode())

    def do_POST(self):
        if self.path.split("?", 1)[0] != "/push":
            self.send(404, b'{"error":"not found"}')
            return
        if not self.authorised():
            return
        n = self.length()
        try:
            status = int(self.headers.get("X-Status", ""))
        except ValueError:
            status = 0
        job_id = self.headers.get("X-Job", "")
        if n is None or n > policy.MAX_REPLY or not 200 <= status <= 599:
            if job_id:
                HUB.finish(job_id, 502, policy.error_body(-32000, "reply refused by the relay"))
            self.send(400, b'{"error":"bad push"}')
            return
        reply = self.rfile.read(n)
        self.send(200 if HUB.finish(job_id, status, reply) else 410, b"{}")


def main():
    if len(TOKEN) < 32:
        sys.exit("RELAY_TOKEN must be set (at least 32 characters)")
    public = Server(("0.0.0.0", PUBLIC_PORT), Public, MAX_PUBLIC_CONNECTIONS)
    dialer = Server(("0.0.0.0", DIALER_PORT), Dialer, MAX_DIALER_CONNECTIONS)
    threading.Thread(target=dialer.serve_forever, daemon=True).start()
    log(f"relay up: public :{PUBLIC_PORT}, dialer :{DIALER_PORT}, {len(policy.ALLOWED)} methods allowed")
    public.serve_forever()


if __name__ == "__main__":
    main()
