"""Tests of the read-only endpoint: the policy, the host dialer against a forged relay, and relay plus dialer end to end
against a fake sequencer (all in-process, loopback ports only, no network, no real sequencer).

    python3 -I tools/public_endpoint/test_policy.py                                  (from the repo root)
    python3 -I -m unittest discover -s tools/public_endpoint -p test_policy.py      (the same, through unittest)
"""

import concurrent.futures
import http.client
import json
import os
import sys
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import policy  # noqa: E402
import dialer as dialer_mod  # noqa: E402
import relay as relay_mod  # noqa: E402

TOKEN = "t" * 64


def req(method, params=None, rid=1):
    d = {"jsonrpc": "2.0", "id": rid, "method": method}
    if params is not None:
        d["params"] = params
    return json.dumps(d).encode()


def refused_code(body):
    try:
        policy.check_request(body)
    except policy.Refused as r:
        return r.code
    return None


class PolicyTest(unittest.TestCase):
    def test_allowed_set(self):
        self.assertEqual(len(policy.ALLOWED), 12)
        for m in policy.WRITES + policy.REFUSED_READS:
            self.assertNotIn(m, policy.ALLOWED)

    def test_writes_and_refused_reads(self):
        for m in policy.WRITES + policy.REFUSED_READS + ("rpc.discover", "nope", "getblock", "GETBLOCK"):
            self.assertEqual(refused_code(req(m, [])), policy.METHOD_NOT_FOUND, m)

    def test_allowed_pass_and_canonical(self):
        method, body = policy.check_request(b' {"params":[355],"method":"getBlock","id":7,"jsonrpc":"2.0"} ')
        self.assertEqual(method, "getBlock")
        self.assertEqual(body, b'{"jsonrpc":"2.0","id":7,"method":"getBlock","params":[355]}')
        self.assertEqual(policy.check_request(req("getLastBlockId"))[1],
                         b'{"jsonrpc":"2.0","id":1,"method":"getLastBlockId"}')

    def test_body_size(self):
        pad = "x" * (policy.MAX_BODY + 1)
        self.assertEqual(refused_code(req("getBlock", [pad])), policy.INVALID_REQUEST)

    def test_batch_and_shape(self):
        self.assertEqual(refused_code(b"[" + req("getBlock", [1]) + b"," + req("getBlock", [2]) + b"]"),
                         policy.INVALID_REQUEST)
        self.assertEqual(refused_code(b"[]"), policy.INVALID_REQUEST)
        self.assertEqual(refused_code(b'"getBlock"'), policy.INVALID_REQUEST)
        self.assertEqual(refused_code(b'{"jsonrpc":"2.0","method":"getBlock","params":[1]}'), policy.INVALID_REQUEST)
        self.assertEqual(refused_code(b'{"jsonrpc":"1.0","id":1,"method":"getBlock"}'), policy.INVALID_REQUEST)
        self.assertEqual(refused_code(b'{"jsonrpc":"2.0","id":1,"method":"getBlock","x":1}'), policy.INVALID_REQUEST)
        self.assertEqual(refused_code(b'{"jsonrpc":"2.0","id":1,"method":5}'), policy.INVALID_REQUEST)
        self.assertEqual(refused_code(b'{"jsonrpc":"2.0","id":[1],"method":"getBlock"}'), policy.INVALID_REQUEST)
        self.assertEqual(refused_code(b'{"jsonrpc":"2.0","id":"' + b"i" * 65 + b'","method":"getBlock"}'),
                         policy.INVALID_REQUEST)

    def test_parse_errors(self):
        for b in (b"not json", b"\xff\xfe", b'{"jsonrpc":"2.0","id":1,"method":"getBlock","params":[NaN]}',
                  b'{"jsonrpc":"2.0","jsonrpc":"2.0","id":1,"method":"getBlock"}', b"[" * 1500):
            self.assertEqual(refused_code(b), policy.PARSE_ERROR, b[:40])

    def test_duplicate_method_key_cannot_smuggle(self):
        b = b'{"jsonrpc":"2.0","id":1,"method":"getBlock","method":"sendTransaction"}'
        self.assertEqual(refused_code(b), policy.PARSE_ERROR)

    def test_block_range(self):
        self.assertIsNone(refused_code(req("getBlockRange", [100, 115])))
        self.assertIsNone(refused_code(req("getBlockRange", {"start_block_id": 1, "end_block_id": 16})))
        for p in ([100, 116], [0, 2 ** 63], [5, 4], [-1, 3], [1.0, 2], ["1", "2"], [True, 2], [1], [1, 2, 3],
                  {"start_block_id": 1, "end_block_id": 17}, {"start_block_id": 1}, {"a": 1, "b": 2}, None, 5):
            self.assertEqual(refused_code(req("getBlockRange", p)), policy.INVALID_PARAMS, p)

    def test_nonces(self):
        self.assertIsNone(refused_code(req("getAccountsNonces", [["a"] * 16])))
        self.assertIsNone(refused_code(req("getAccountsNonces", {"account_ids": ["a"]})))
        self.assertEqual(refused_code(req("getAccountsNonces", [["a"] * 17])), policy.INVALID_PARAMS)
        self.assertEqual(refused_code(req("getAccountsNonces", ["a"])), policy.INVALID_PARAMS)
        self.assertEqual(refused_code(req("getAccountsNonces", [])), policy.INVALID_PARAMS)

    def test_non_finite_numbers(self):
        # 1e999 parses as infinity; it has no JSON form, so it must never be re-encoded and sent on.
        for body in (b'{"jsonrpc":"2.0","id":1,"method":"getBlock","params":[1e999]}',
                     b'{"jsonrpc":"2.0","id":1,"method":"getBlock","params":[-1e999]}',
                     b'{"jsonrpc":"2.0","id":1,"method":"getAccountView","params":[{"account_id":1e999}]}'):
            self.assertEqual(refused_code(body), policy.INVALID_PARAMS, body)
        self.assertIsNone(refused_code(b'{"jsonrpc":"2.0","id":1,"method":"getBlock","params":[1e308]}'))

    def test_params_type(self):
        self.assertEqual(refused_code(req("getBlock", 5)), policy.INVALID_PARAMS)
        self.assertEqual(refused_code(req("getBlock", "355")), policy.INVALID_PARAMS)

    def test_rate_limiter(self):
        now = [0.0]
        lim = policy.RateLimiter(rate=5, burst=20, total_rate=20, total_burst=40, max_keys=3, clock=lambda: now[0])
        self.assertEqual(sum(lim.allow("a") for _ in range(30)), 20)  # burst
        now[0] += 1.0
        self.assertEqual(sum(lim.allow("a") for _ in range(30)), 5)   # refill 5 a second
        self.assertTrue(lim.allow("b"))                                 # another visitor has its own bucket
        # The total bucket caps everyone together.
        now[0] += 100.0
        got = sum(lim.allow(k) for k in "bcd" for _ in range(20))
        self.assertLessEqual(got, 40)
        # Bounded keys: a fourth visitor while three are fresh is refused, after they idle it is let in.
        now[0] += 0.01
        self.assertFalse(lim.allow("e"))
        now[0] += 10.0
        self.assertTrue(lim.allow("e"))

    def test_reserve_paces_then_refuses(self):
        now = [0.0]
        lim = policy.RateLimiter(rate=5, burst=20, total_rate=100, total_burst=100, max_delay=1.0,
                                 clock=lambda: now[0])
        waits = [lim.reserve("a") for _ in range(26)]
        self.assertEqual(waits[:20], [0.0] * 20)
        self.assertEqual([round(w, 2) for w in waits[20:25]], [0.2, 0.4, 0.6, 0.8, 1.0])
        self.assertIsNone(waits[25])                                     # more than 1 s ahead: refused
        now[0] += 2.0
        self.assertEqual(lim.reserve("a"), 0.0)
        # A refusal by the total bucket gives the visitor's token back.
        lim = policy.RateLimiter(rate=5, burst=2, total_rate=1, total_burst=1, max_delay=0.0, clock=lambda: now[0])
        self.assertEqual(lim.reserve("a"), 0.0)
        self.assertIsNone(lim.reserve("a"))
        self.assertEqual(lim.keys["a"].tokens, 1.0)


# ---------------------------------------------------------------------------------------------------------------------
# Fakes


class FakeSequencer:
    """Records every request; answers getBlock-style calls with a fixed reply."""

    def __init__(self, reply=b'{"jsonrpc":"2.0","id":1,"result":42}', delay=0.0):
        self.calls = []
        outer = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                outer.calls.append(json.loads(body))
                time.sleep(delay)
                self.send_response(200)
                self.send_header("Content-Length", str(len(reply)))
                self.end_headers()
                self.wfile.write(reply)

        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.srv.handle_error = lambda *a: None  # a client that gave up (timeout test) is expected
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.srv.server_address[1]}"

    def methods(self):
        return [c.get("method") for c in self.calls]

    def close(self):
        self.srv.shutdown()
        self.srv.server_close()


class ForgedRelay:
    """A taken-over relay: hands the host dialer whatever jobs it wants and records the pushes."""

    def __init__(self, jobs):
        self.jobs, self.pushes, self.auth = list(jobs), [], []
        outer = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_GET(self):
                outer.auth.append(self.headers.get("Authorization"))
                if outer.jobs:
                    job_id, body = outer.jobs.pop(0)
                    out = json.dumps({"job": job_id, "body": body}).encode()
                    self.send_response(200)
                else:
                    time.sleep(0.2)
                    out = b""
                    self.send_response(204)
                self.send_header("Content-Length", str(len(out)))
                self.end_headers()
                self.wfile.write(out)

            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                outer.pushes.append((self.headers["X-Job"], int(self.headers["X-Status"]), body))
                self.send_response(200)
                self.send_header("Content-Length", "0")
                self.end_headers()

        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.srv.handle_error = lambda *a: None  # a client that gave up (timeout test) is expected
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.srv.server_address[1]}"

    def close(self):
        self.srv.shutdown()
        self.srv.server_close()


def start_dialer(seq_url, relay_url, stats_path):
    dialer_mod.STOP.clear()
    a = dialer_mod.Dialer(seq_url, "test", TOKEN, relay_url, dialer_mod.Stats(stats_path))
    threads = [threading.Thread(target=a.worker, daemon=True) for _ in range(policy.MAX_CONCURRENT)]
    for t in threads:
        t.start()
    return a, threads


def stop_dialer(threads):
    dialer_mod.STOP.set()
    for t in threads:
        t.join(timeout=45)


def wait_for(cond, timeout=10.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if cond():
            return True
        time.sleep(0.02)
    return False


class DialerAgainstForgedRelay(unittest.TestCase):
    def test_forged_jobs_never_reach_the_sequencer(self):
        seq = FakeSequencer()
        forged = [
            ("j1", '{"jsonrpc":"2.0","id":1,"method":"sendTransaction","params":[{"x":1}]}'),
            ("j2", '{"jsonrpc":"2.0","id":2,"method":"requeueCrossZoneDeadLetter","params":["00"]}'),
            ("j3", '[{"jsonrpc":"2.0","id":3,"method":"getBlock","params":[1]}]'),
            ("j4", '{"jsonrpc":"2.0","id":4,"method":"getBlockRange","params":[0,1000]}'),
            ("j5", '{"jsonrpc":"2.0","id":5,"method":"getBlock","params":["' + "y" * 3000 + '"]}'),
            ("j6", '{"jsonrpc":"2.0","id":6,"method":"getProofsAndRoot","params":[[],[]]}'),
            ("j7", '{"jsonrpc":"2.0","id":7,"method":"getBlock","params":[1],"method2":"sendTransaction"}'),
            ("j8", '{"jsonrpc":"2.0","id":8,"method":"getBlock","params":[355]}'),
        ]
        relay = ForgedRelay(forged)
        stats_path = os.path.join(os.environ.get("TMPDIR", "/tmp"), f"seq-read-test-{os.getpid()}.json")
        a, threads = start_dialer(seq.url, relay.url, stats_path)
        try:
            self.assertTrue(wait_for(lambda: len(relay.pushes) == len(forged)))
        finally:
            stop_dialer(threads)
            relay.close()
            seq.close()
        self.assertEqual(seq.methods(), ["getBlock"])               # only j8 reached the sequencer
        self.assertEqual(seq.calls[0], {"jsonrpc": "2.0", "id": 8, "method": "getBlock", "params": [355]})
        by_job = {j: (s, json.loads(b)) for j, s, b in relay.pushes}
        self.assertEqual(by_job["j1"][1]["error"]["code"], policy.METHOD_NOT_FOUND)
        self.assertEqual(by_job["j2"][1]["error"]["code"], policy.METHOD_NOT_FOUND)
        self.assertEqual(by_job["j3"][1]["error"]["code"], policy.INVALID_REQUEST)
        self.assertEqual(by_job["j4"][1]["error"]["code"], policy.INVALID_PARAMS)
        self.assertEqual(by_job["j5"][1]["error"]["code"], policy.INVALID_REQUEST)
        self.assertEqual(by_job["j6"][1]["error"]["code"], policy.METHOD_NOT_FOUND)
        self.assertEqual(by_job["j7"][1]["error"]["code"], policy.INVALID_REQUEST)
        self.assertEqual(by_job["j8"], (200, {"jsonrpc": "2.0", "id": 1, "result": 42}))
        self.assertTrue(all(h == f"Bearer {TOKEN}" for h in relay.auth))
        snap = a.stats.snapshot()
        self.assertNotIn("sendTransaction", snap["forwarded"])
        self.assertEqual(snap["refused"].get("sendTransaction"), 1)
        self.assertEqual(snap["forwarded"], {"getBlock": 1})
        if os.path.exists(stats_path):
            os.remove(stats_path)

    def test_refusal_counters_have_fixed_keys(self):
        a = dialer_mod.Dialer("http://127.0.0.1:9", "x", TOKEN, "", dialer_mod.Stats(os.devnull))
        for i in range(500):
            a.handle(req(f"madeUpMethod{i}", []))
        a.handle(req("x" * 60, []))
        a.handle(req("sendTransaction", []))
        a.handle(req("getBlockRange", [0, 99]))
        a.handle(b"[1,2]")
        a.handle(b"\xff")
        self.assertEqual(a.stats.snapshot()["refused"],
                         {"<other>": 502, "sendTransaction": 1, "getBlockRange": 1, "<unparsable>": 1})
        self.assertEqual(a.stats.snapshot()["forwarded"], {})

    def test_sequencer_url_must_be_loopback(self):
        with self.assertRaises(SystemExit):
            dialer_mod.Dialer("http://10.0.0.1:3051", "x", TOKEN, "", None)
        with self.assertRaises(SystemExit):
            dialer_mod.Dialer("https://127.0.0.1:3051", "x", TOKEN, "", None)

    def test_dialer_rate_cap(self):
        seq = FakeSequencer()
        a = dialer_mod.Dialer(seq.url, "x", TOKEN, "", dialer_mod.Stats(os.devnull))
        # Frozen clock at the limiter's start: no refill while the requests run, however slow the machine.
        start = a.limiter.total.at
        a.limiter.clock = lambda: start
        try:
            got = [a.handle(req("getLastBlockId"))[0] for _ in range(int(policy.TOTAL_BURST) + 10)]
        finally:
            seq.close()
        self.assertEqual(got.count(200), int(policy.TOTAL_BURST))
        self.assertEqual(got.count(429), 10)

    def test_reply_cap_and_timeout(self):
        big = FakeSequencer(reply=b"x" * (policy.MAX_REPLY + 1))
        a = dialer_mod.Dialer(big.url, "x", TOKEN, "", dialer_mod.Stats(os.devnull))
        try:
            self.assertEqual(a.handle(req("getBlock", [1]))[0], 502)
        finally:
            big.close()
        slow = FakeSequencer(delay=policy.SEQ_TIMEOUT + 1)
        a = dialer_mod.Dialer(slow.url, "x", TOKEN, "", dialer_mod.Stats(os.devnull))
        try:
            self.assertEqual(a.handle(req("getBlock", [1]))[0], 504)
        finally:
            slow.close()


class VisitorKey(unittest.TestCase):
    @staticmethod
    def key(xff, peer="172.31.128.2"):
        h = relay_mod.Public.__new__(relay_mod.Public)
        h.headers = {} if xff is None else {"X-Forwarded-For": xff}
        h.client_address = (peer, 40000)
        return h.visitor()

    def test_last_entry_is_the_key(self):
        self.assertEqual(self.key("198.51.100.7"), "198.51.100.7")
        self.assertEqual(self.key("10.9.9.9, 198.51.100.7"), "198.51.100.7")
        self.assertEqual(self.key(" 198.51.100.7 "), "198.51.100.7")

    def test_not_an_address_falls_back_to_the_peer(self):
        self.assertEqual(self.key("x" * 5000), "172.31.128.2")
        self.assertEqual(self.key("198.51.100.7, not-an-ip"), "172.31.128.2")
        self.assertEqual(self.key(None), "172.31.128.2")
        self.assertEqual(self.key("garbage", peer="also garbage"), "unknown")

    def test_ipv6_keyed_by_its_64(self):
        a = self.key("2001:db8:1:2::5")
        self.assertEqual(a, "2001:db8:1:2::/64")
        self.assertEqual(self.key("2001:db8:1:2:ffff:ffff:ffff:1"), a)
        self.assertNotEqual(self.key("2001:db8:1:3::5"), a)
        self.assertEqual(self.key("::ffff:192.0.2.1"), "192.0.2.1")


class RelayEndToEnd(unittest.TestCase):
    """relay.py's two servers on ephemeral loopback ports, dialer.py between them and a fake sequencer."""

    @classmethod
    def setUpClass(cls):
        relay_mod.TOKEN = TOKEN
        relay_mod.LIMITER = policy.RateLimiter()
        cls.public = relay_mod.Server(("127.0.0.1", 0), relay_mod.Public, 32)
        cls.dialer_srv = relay_mod.Server(("127.0.0.1", 0), relay_mod.Dialer, 8)
        for s in (cls.public, cls.dialer_srv):
            threading.Thread(target=s.serve_forever, daemon=True).start()
        cls.seq = FakeSequencer()
        cls.dialer, cls.threads = start_dialer(cls.seq.url, f"http://127.0.0.1:{cls.dialer_srv.server_address[1]}",
                                             os.devnull)
        wait_for(relay_mod.HUB.dialer_present)

    @classmethod
    def tearDownClass(cls):
        stop_dialer(cls.threads)
        for s in (cls.public, cls.dialer_srv):
            s.shutdown()
            s.server_close()
        cls.seq.close()

    def call(self, method, path, body=None, headers=None, port=None):
        conn = http.client.HTTPConnection("127.0.0.1", port or self.public.server_address[1], timeout=15)
        try:
            h = {"Content-Type": "application/json", "X-Forwarded-For": f"203.0.113.{self.id().__hash__() % 200}"}
            h.update(headers or {})
            conn.request(method, path, body=body, headers=h)
            r = conn.getresponse()
            return r.status, r.read()
        finally:
            conn.close()

    def test_allowed_read_round_trip(self):
        status, body = self.call("POST", "/", req("getBlock", [355]))
        self.assertEqual((status, body), (200, b'{"jsonrpc":"2.0","id":1,"result":42}'))

    def test_refusals(self):
        def code(body):
            status, out = self.call("POST", "/", body)
            self.assertEqual(status, 200)
            return json.loads(out)["error"]["code"]

        n = len(self.seq.calls)
        self.assertEqual(code(req("sendTransaction", [{}])), policy.METHOD_NOT_FOUND)
        self.assertEqual(code(req("getRecoveryBinding", ["x"])), policy.METHOD_NOT_FOUND)
        self.assertEqual(code(b"[" + req("getBlock", [1]) + b"," + req("getBlock", [2]) + b"]"),
                         policy.INVALID_REQUEST)
        self.assertEqual(code(b"hello"), policy.PARSE_ERROR)
        self.assertEqual(code(req("getBlockRange", [1, 17])), policy.INVALID_PARAMS)
        self.assertEqual(self.call("POST", "/", req("getBlock", ["x" * 3000]))[0], 413)
        self.assertEqual(self.call("GET", "/x")[0], 404)
        self.assertEqual(self.call("POST", "/x", req("getBlock", [1]))[0], 404)
        self.assertEqual(self.call("PUT", "/", req("getBlock", [1]))[0], 405)
        self.assertEqual(self.call("GET", "/")[0], 200)
        self.assertEqual(len(self.seq.calls), n)  # none of them reached the sequencer

    def test_dialer_port_needs_token(self):
        port = self.dialer_srv.server_address[1]
        self.assertEqual(self.call("GET", "/pull", port=port)[0], 401)
        self.assertEqual(self.call("GET", "/pull", headers={"Authorization": "Bearer " + "t" * 63}, port=port)[0],
                         401)
        self.assertEqual(self.call("POST", "/push", body=b"{}", headers={"X-Job": "x", "X-Status": "200"},
                                   port=port)[0], 401)

    def test_public_port_has_no_dialer_paths(self):
        auth = {"Authorization": f"Bearer {TOKEN}"}
        self.assertEqual(self.call("GET", "/pull", headers=auth)[0], 404)
        self.assertEqual(self.call("POST", "/push", body=b"{}", headers=auth)[0], 404)

    def test_rate_limit(self):
        # A sequential client over its rate is slowed, not refused: 30 calls, 20 at once, then 5 a second.
        h = {"X-Forwarded-For": "198.51.100.7, 192.0.2.99"}   # the last entry (set by the proxy) is the key
        t0 = time.monotonic()
        got = [self.call("POST", "/", req("getLastBlockId"), headers=h)[0] for _ in range(30)]
        self.assertEqual(got, [200] * 30)
        self.assertGreater(time.monotonic() - t0, 1.5)
        # A flood from another visitor: some 429, and a second later it works again.
        h2 = {"X-Forwarded-For": "192.0.2.50"}
        with concurrent.futures.ThreadPoolExecutor(20) as ex:
            flood = list(ex.map(lambda _: self.call("POST", "/", req("getLastBlockId"), headers=h2)[0], range(60)))
        self.assertIn(429, flood)
        self.assertIn(200, flood)
        time.sleep(1.5)
        self.assertEqual(self.call("POST", "/", req("getLastBlockId"), headers=h2)[0], 200)

    def test_health(self):
        status, body = self.call("GET", "/health")
        self.assertEqual((status, json.loads(body)), (200, {"ok": True, "dialer": True}))


if __name__ == "__main__":
    unittest.main()
