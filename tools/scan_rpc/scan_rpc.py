"""Scans the raw JSON-RPC answers of a LEZ sequencer for account ids in every form (privacy check of a claim).

Usage: python3 -I tools/scan_rpc/scan_rpc.py <rpc url> <block id> <tx hash> <id base58> [<id base58> ...]
Fetches getBlock and getTransaction exactly as a client sees them, then counts each id as base58
text, hex text and, inside every base64 string of the answer (the RPC carries blocks and
transactions as base64 of their borsh bytes), as raw 32 bytes.
"""
import base64
import binascii
import json
import re
import sys
import urllib.request

ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58decode(text):
    n = 0
    for ch in text:
        n = n * 58 + ALPHABET.index(ch)
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big")
    pad = len(text) - len(text.lstrip("1"))
    return b"\0" * pad + raw


def rpc(url, method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(url, body, {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode()


def base64_blobs(text):
    out = []
    for s in re.findall(r'"([A-Za-z0-9+/=]{64,})"', text):
        try:
            out.append(base64.b64decode(s, validate=True))
        except (binascii.Error, ValueError):
            pass
    return out


def main():
    url, block_id, tx_hash, *ids = sys.argv[1:]
    answers = {
        "getBlock": rpc(url, "getBlock", [int(block_id)]),
        "getTransaction": rpc(url, "getTransaction", [tx_hash]),
    }
    total = {}
    for name, text in answers.items():
        blobs = base64_blobs(text)
        print(f"  rpc {name}: {len(text)} chars of JSON, {len(blobs)} base64 blob(s) of "
              f"{sum(len(b) for b in blobs)} bytes; error={'error' in json.loads(text)}")
        for ident in ids:
            raw = b58decode(ident)
            assert len(raw) == 32, ident
            c58 = text.count(ident)
            chex = text.lower().count(raw.hex())
            craw = sum(b.count(raw) for b in blobs)
            total[ident] = total.get(ident, 0) + c58 + chex + craw
            print(f"    {ident}: base58 {c58}, hex {chex}, raw bytes in decoded base64 {craw}")
    for ident, n in total.items():
        print(f"  RPC_TOTAL {ident} {n}")


if __name__ == "__main__":
    main()
