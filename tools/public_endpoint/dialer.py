#!/usr/bin/env python3
"""The host half of the read-only sequencer endpoint: dials into the relay app, calls the sequencer.

    env -i PATH=/usr/bin:/bin SEQ_URL=http://127.0.0.1:3051 RELAY_APP=<app name> \\
        RELAY_TOKEN_FILE=<token file> python3 -I tools/public_endpoint/dialer.py

Nothing listens on the host. Four workers long-poll the relay's dialer port (`GET /pull`, bearer token), check each
request again with policy.check_request (a request the relay should have refused is answered with the refusal and
never reaches the sequencer), send the canonical request to SEQ_URL (loopback only), and return the reply
(`POST /push`). The relay's address is read with `docker inspect APP.web.1` at each reconnect (RELAY_URL overrides it,
for tests). Counters per method go to STATS_FILE (default: next to the token file, stats.json).
Standard library only.
"""

import http.client
import json
import os
import signal
import subprocess
import sys
import threading
import time
import urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import policy  # noqa: E402

PULL_WAIT_TIMEOUT = 40.0   # socket timeout of one /pull (the relay holds it up to 25 s)
STOP = threading.Event()


def log(*a):
    print(time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), *a, flush=True)


class Stats:
    def __init__(self, path):
        self.path, self.lock = path, threading.Lock()
        self.forwarded, self.refused, self.errors = {}, {}, {}
        self.started = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        self.dirty = True

    def add(self, table, key):
        with self.lock:
            table[key] = table.get(key, 0) + 1
            self.dirty = True

    def snapshot(self):
        with self.lock:
            return {"started": self.started, "pid": os.getpid(), "forwarded": dict(self.forwarded),
                    "refused": dict(self.refused), "errors": dict(self.errors)}

    def write(self):
        with self.lock:
            if not self.dirty:
                return
            self.dirty = False
        doc = self.snapshot()
        doc["written"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        tmp = self.path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(doc, f, indent=1, sort_keys=True)
            f.write("\n")
        os.replace(tmp, self.path)


class Dialer:
    def __init__(self, seq_url, app, token, relay_url, stats):
        u = urllib.parse.urlsplit(seq_url)
        if u.scheme != "http" or u.hostname not in ("127.0.0.1", "localhost", "::1") or not u.port:
            raise SystemExit("SEQ_URL must be http://127.0.0.1:PORT (the sequencer on this host)")
        self.seq = (u.hostname, u.port)
        self.app, self.token, self.fixed_relay, self.stats = app, token, relay_url, stats
        self.limiter = policy.RateLimiter(rate=policy.TOTAL_RATE, burst=policy.TOTAL_BURST,
                                          total_rate=policy.TOTAL_RATE, total_burst=policy.TOTAL_BURST)
        self.relay_lock = threading.Lock()
        self.relay = None
        self.connected = None

    # -- the relay ------------------------------------------------------------------------------------------------

    def relay_addr(self):
        with self.relay_lock:
            if self.relay is None:
                if self.fixed_relay:
                    u = urllib.parse.urlsplit(self.fixed_relay)
                    self.relay = (u.hostname, u.port or 80)
                else:
                    self.relay = (self.inspect(), 8081)
            return self.relay

    def inspect(self):
        out = subprocess.run(["docker", "inspect", "-f", "{{json .NetworkSettings.Networks}}", f"{self.app}.web.1"],
                             capture_output=True, text=True, timeout=10, check=False)
        if out.returncode != 0:
            raise OSError(f"docker inspect {self.app}.web.1: {out.stderr.strip()[:200]}")
        nets = json.loads(out.stdout or "{}") or {}
        own = nets.get(f"hosting-{self.app}") or {}
        ips = [ip for ip in [own.get("IPAddress")] + [n.get("IPAddress") for n in nets.values()] if ip]
        if not ips:
            raise OSError(f"{self.app}.web.1 has no address")
        return ips[0]

    def forget_relay(self):
        with self.relay_lock:
            self.relay = None

    def relay_call(self, method, path, body=None, headers=None, timeout=10.0):
        host, port = self.relay_addr()
        conn = http.client.HTTPConnection(host, port, timeout=timeout)
        try:
            h = {"Authorization": f"Bearer {self.token}"}
            h.update(headers or {})
            conn.request(method, path, body=body, headers=h)
            r = conn.getresponse()
            return r.status, r.read(64 * 1024)
        finally:
            conn.close()

    def mark(self, up, why=""):
        if self.connected != up:
            self.connected = up
            log("relay connected" if up else f"relay away: {why}")

    # -- one job --------------------------------------------------------------------------------------------------

    def handle(self, raw):
        try:
            method, canonical = policy.check_request(raw)
        except policy.Refused as r:
            self.stats.add(self.stats.refused, self.method_of(raw))
            return 200, policy.error_body(r.code, r.message, r.req_id)
        if not self.limiter.allow("all"):
            self.stats.add(self.stats.refused, "<rate>")
            return 429, policy.error_body(-32000, "too many requests")
        conn = http.client.HTTPConnection(*self.seq, timeout=policy.SEQ_TIMEOUT)
        try:
            conn.request("POST", "/", body=canonical, headers={"Content-Type": "application/json"})
            r = conn.getresponse()
            reply = r.read(policy.MAX_REPLY + 1)
            status = r.status
        except (OSError, http.client.HTTPException) as e:
            self.stats.add(self.stats.errors, method)
            timeout = isinstance(e, TimeoutError)
            return (504 if timeout else 502), policy.error_body(-32000, "the sequencer did not answer")
        finally:
            conn.close()
        if len(reply) > policy.MAX_REPLY:
            self.stats.add(self.stats.errors, method)
            return 502, policy.error_body(-32000, f"reply over {policy.MAX_REPLY} bytes")
        self.stats.add(self.stats.forwarded, method)
        return status, reply

    KNOWN = frozenset(policy.ALLOWED | set(policy.WRITES) | set(policy.REFUSED_READS))

    @classmethod
    def method_of(cls, raw):
        """A counter key from a fixed set, so a relay sending made-up names cannot grow the counters."""
        try:
            doc = json.loads(raw)
        except (ValueError, RecursionError):
            return "<unparsable>"
        m = doc.get("method") if isinstance(doc, dict) else None
        return m if isinstance(m, str) and m in cls.KNOWN else "<other>"

    def worker(self):
        backoff = 1.0
        while not STOP.is_set():
            try:
                status, data = self.relay_call("GET", "/pull", timeout=PULL_WAIT_TIMEOUT)
                if status == 401:
                    raise OSError("token refused by the relay")
                if status == 204:
                    self.mark(True)
                    backoff = 1.0
                    continue
                if status != 200:
                    raise OSError(f"/pull answered {status}")
                self.mark(True)
                backoff = 1.0
                job = json.loads(data)
                job_id, raw = str(job["job"]), str(job["body"]).encode()
                status, reply = self.handle(raw)
                self.relay_call("POST", "/push", body=reply,
                                headers={"X-Job": job_id, "X-Status": str(status),
                                         "Content-Type": "application/json"})
            except (OSError, http.client.HTTPException, ValueError, KeyError, TypeError) as e:
                self.mark(False, str(e)[:200])
                self.forget_relay()
                STOP.wait(backoff)
                backoff = min(backoff * 2, 15.0)


def main():
    token_file = os.environ.get("RELAY_TOKEN_FILE", "")
    try:
        with open(token_file, encoding="utf-8") as f:
            token = f.read().strip()
    except OSError as e:
        sys.exit(f"RELAY_TOKEN_FILE: {e.strerror}")
    if len(token) < 32:
        sys.exit("RELAY_TOKEN_FILE holds no token (at least 32 characters)")
    stats = Stats(os.environ.get("STATS_FILE") or os.path.join(os.path.dirname(os.path.abspath(token_file)),
                                                                "stats.json"))
    dialer = Dialer(os.environ.get("SEQ_URL", "http://127.0.0.1:3051"),
                    os.environ.get("RELAY_APP", "cliff-veil-seq"),
                    token, os.environ.get("RELAY_URL", ""), stats)
    del token

    def stop(*_):
        STOP.set()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    workers = [threading.Thread(target=dialer.worker, daemon=True) for _ in range(policy.MAX_CONCURRENT)]
    for w in workers:
        w.start()
    log(f"dialer up: {policy.MAX_CONCURRENT} workers, sequencer {dialer.seq[0]}:{dialer.seq[1]}, app {dialer.app}")
    last_summary = ""
    while not STOP.wait(5.0):
        stats.write()
        s = json.dumps(stats.snapshot()["forwarded"], sort_keys=True)
        if s != last_summary and int(time.time()) % 60 < 5:
            log("forwarded", s)
            last_summary = s
    stats.write()
    log("dialer stopped", json.dumps(stats.snapshot(), sort_keys=True))


if __name__ == "__main__":
    main()
