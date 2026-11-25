"""The allow-list and the limits of the public read-only sequencer endpoint (one file, used by both parts).

relay.py (the public app) checks every request with `check_request` before it queues it; dialer.py (on the sequencer's
host) checks it again before it calls the sequencer, so a broken or taken-over relay can ask only for these reads.
Standard library only.
"""

import json
import threading
import time

# Read methods forwarded to the sequencer. Everything else is refused, the two writes included
# (`sendTransaction`, `requeueCrossZoneDeadLetter`).
ALLOWED = frozenset({
    "getTransaction", "getBlock", "getBlockRange", "getLastBlockId",
    "getAccount", "getAccountView", "getAccountBalance", "getAccountsNonces",
    "getProgramIds", "getFeeState", "getChannelId", "checkHealth",
})
# Named here so the tests and the help text can show them; refused like any unknown method.
WRITES = ("sendTransaction", "requeueCrossZoneDeadLetter")
REFUSED_READS = ("getPublications", "getMessagePath", "getProofsAndRoot", "getRecoveryBinding",
                 "getCrossZoneDeadLetters")

MAX_BODY = 2048               # bytes per request
MAX_REPLY = 2 * 1024 * 1024   # bytes per reply
MAX_RANGE = 16                # blocks per getBlockRange (inclusive span)
MAX_NONCES = 16               # account ids per getAccountsNonces
MAX_ID_LEN = 64               # characters of a string request id
PER_VISITOR_RATE, PER_VISITOR_BURST = 5.0, 20.0
TOTAL_RATE, TOTAL_BURST = 20.0, 40.0
MAX_WAITING = 64              # jobs queued in the relay
SEQ_TIMEOUT = 5.0             # seconds, host dialer to sequencer
TOTAL_TIMEOUT = 10.0          # seconds, visitor to reply
MAX_CONCURRENT = 4            # sequencer calls at once (the host dialer's workers)
MAX_DELAY = 1.0               # seconds a visitor a little over its rate is slowed before HTTP 429

# JSON-RPC error codes.
PARSE_ERROR, INVALID_REQUEST, METHOD_NOT_FOUND, INVALID_PARAMS = -32700, -32600, -32601, -32602


class Refused(Exception):
    """A request the endpoint will not forward: `code` is the JSON-RPC error code."""

    def __init__(self, code, message, req_id=None):
        super().__init__(message)
        self.code, self.message, self.req_id = code, message, req_id


def error_body(code, message, req_id=None):
    return json.dumps({"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}},
                      separators=(",", ":")).encode()


def _no_duplicate_keys(pairs):
    out = {}
    for k, v in pairs:
        if k in out:
            raise ValueError("duplicate key")
        out[k] = v
    return out


def _no_constant(name):
    raise ValueError(f"{name} is not JSON")


def _uint(v):
    return isinstance(v, int) and not isinstance(v, bool) and 0 <= v < 2 ** 64


def _positional(method, params, names):
    """The params as a list in `names` order (array or object form), or Refused."""
    if isinstance(params, list):
        if len(params) != len(names):
            raise Refused(INVALID_PARAMS, f"{method} takes {len(names)} params")
        return params
    if isinstance(params, dict):
        if set(params) != set(names):
            raise Refused(INVALID_PARAMS, f"{method} takes params {', '.join(names)}")
        return [params[n] for n in names]
    raise Refused(INVALID_PARAMS, f"{method} takes params {', '.join(names)}")


def _check_params(method, params):
    if params is not None and not isinstance(params, (list, dict)):
        raise Refused(INVALID_PARAMS, "params must be an array or an object")
    if method == "getBlockRange":
        start, end = _positional(method, params, ("start_block_id", "end_block_id"))
        if not (_uint(start) and _uint(end)):
            raise Refused(INVALID_PARAMS, "block ids must be unsigned integers")
        if end < start or end - start + 1 > MAX_RANGE:
            raise Refused(INVALID_PARAMS, f"getBlockRange spans at most {MAX_RANGE} blocks on this endpoint")
    elif method == "getAccountsNonces":
        (ids,) = _positional(method, params, ("account_ids",))
        if not isinstance(ids, list) or len(ids) > MAX_NONCES:
            raise Refused(INVALID_PARAMS, f"getAccountsNonces takes at most {MAX_NONCES} account ids")


def check_request(body):
    """Checks one request body (bytes). Returns (method, canonical body bytes) or raises Refused.

    The canonical body is what the host dialer sends to the sequencer: only `jsonrpc`, `id`, `method` and `params`, one JSON
    object, re-encoded, so nothing the policy did not see reaches the sequencer.
    """
    if len(body) > MAX_BODY:
        raise Refused(INVALID_REQUEST, f"request over {MAX_BODY} bytes")
    try:
        req = json.loads(body.decode("utf-8"), object_pairs_hook=_no_duplicate_keys, parse_constant=_no_constant)
    except (UnicodeDecodeError, ValueError, RecursionError):
        raise Refused(PARSE_ERROR, "parse error") from None
    if isinstance(req, list):
        raise Refused(INVALID_REQUEST, "batches are not accepted on this endpoint")
    if not isinstance(req, dict):
        raise Refused(INVALID_REQUEST, "the request must be one JSON object")
    req_id = req.get("id")
    if not (req_id is None or _uint(req_id) or (isinstance(req_id, str) and len(req_id) <= MAX_ID_LEN)):
        raise Refused(INVALID_REQUEST, "id must be an unsigned integer or a short string")
    if "id" not in req:
        raise Refused(INVALID_REQUEST, "notifications (no id) are not accepted", None)
    if set(req) - {"jsonrpc", "id", "method", "params"} or req.get("jsonrpc") != "2.0":
        raise Refused(INVALID_REQUEST, "expected jsonrpc 2.0 with id, method and params only", req_id)
    method = req.get("method")
    if not isinstance(method, str):
        raise Refused(INVALID_REQUEST, "method must be a string", req_id)
    if method not in ALLOWED:
        raise Refused(METHOD_NOT_FOUND, "not available on this read-only endpoint", req_id)
    params = req.get("params")
    try:
        _check_params(method, params)
    except Refused as r:
        r.req_id = req_id
        raise
    out = {"jsonrpc": "2.0", "id": req_id, "method": method}
    if "params" in req:
        out["params"] = params
    try:
        canonical = json.dumps(out, separators=(",", ":"), allow_nan=False)
    except ValueError:  # 1e999 parses as infinity, which has no JSON form
        raise Refused(INVALID_PARAMS, "numbers must be finite", req_id) from None
    return method, canonical.encode()


class TokenBucket:
    """Token bucket (refills `rate` per second up to `burst`) that can lend up to `max_delay` seconds of tokens: a
    caller slightly over the rate is told to wait, one far over it is refused."""

    __slots__ = ("rate", "burst", "tokens", "at")

    def __init__(self, rate, burst, now):
        self.rate, self.burst, self.tokens, self.at = rate, burst, burst, now

    def reserve(self, now, max_delay):
        """Seconds to wait before going ahead, or None (refused, nothing taken)."""
        self.tokens = min(self.burst, self.tokens + (now - self.at) * self.rate)
        self.at = now
        wait = max(0.0, (1.0 - self.tokens) / self.rate)
        if wait > max_delay + 1e-9:
            return None
        self.tokens -= 1.0
        return wait

    def refund(self):
        self.tokens += 1.0


class RateLimiter:
    """Per-key buckets plus one total bucket. Thread-safe; idle keys are dropped so memory stays bounded.

    `reserve` paces a client that goes a little over its rate (it waits up to `max_delay` seconds, as a sequential
    client re-reading many transactions does) and refuses one that floods (the wait would be longer)."""

    def __init__(self, rate=PER_VISITOR_RATE, burst=PER_VISITOR_BURST, total_rate=TOTAL_RATE,
                 total_burst=TOTAL_BURST, max_keys=10000, max_delay=MAX_DELAY, clock=time.monotonic):
        self.rate, self.burst, self.max_keys, self.max_delay, self.clock = rate, burst, max_keys, max_delay, clock
        self.total = TokenBucket(total_rate, total_burst, clock())
        self.keys = {}
        self.lock = threading.Lock()

    def reserve(self, key, max_delay=None):
        """Seconds the caller must wait before going ahead, or None when refused (HTTP 429)."""
        max_delay = self.max_delay if max_delay is None else max_delay
        now = self.clock()
        with self.lock:
            b = self.keys.get(key)
            if b is None:
                if len(self.keys) >= self.max_keys:
                    self._sweep(now)
                    if len(self.keys) >= self.max_keys:
                        return None
                b = self.keys[key] = TokenBucket(self.rate, self.burst, now)
            mine = b.reserve(now, max_delay)
            if mine is None:
                return None
            total = self.total.reserve(now, max_delay)
            if total is None:
                b.refund()
                return None
            return max(mine, total)

    def allow(self, key):
        """No waiting: True when a token is there now."""
        return self.reserve(key, max_delay=0.0) == 0.0

    def _sweep(self, now):
        full = self.burst / self.rate + self.max_delay  # a bucket idle this long is full again: forgetting it is safe
        for k in [k for k, b in self.keys.items() if now - b.at >= full]:
            del self.keys[k]
