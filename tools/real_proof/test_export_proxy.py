"""Tests of export_proxy.py against an in-process upstream (no network, no sequencer).

    python3 -I tools/real_proof/test_export_proxy.py
"""

import http.server
import json
import os
import pathlib
import sys
import tempfile
import threading
import unittest
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import export_proxy  # noqa: E402

export_proxy.PACE = 0.0


class Upstream(http.server.BaseHTTPRequestHandler):
    """Answers every method with its name; refuses getProofsAndRoot like the read-only endpoint."""

    seen = []

    def do_POST(self):  # noqa: N802
        req = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        Upstream.seen.append(req["method"])
        if req["method"] == "getProofsAndRoot":
            reply = {"jsonrpc": "2.0", "id": req["id"],
                     "error": {"code": -32601, "message": "not available on this read-only endpoint"}}
        else:
            reply = {"jsonrpc": "2.0", "id": req["id"], "result": req["method"]}
        data = json.dumps(reply).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


def serve(handler):
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def post(port, body):
    req = urllib.request.Request(f"http://127.0.0.1:{port}/", body, {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


class ExportProxyTest(unittest.TestCase):
    def setUp(self):
        Upstream.seen = []
        self.out = tempfile.mkdtemp()
        self.up = serve(Upstream)
        url = f"http://127.0.0.1:{self.up.server_address[1]}/"
        self.proxy = serve(export_proxy.make_handler(url, self.out))
        self.port = self.proxy.server_address[1]

    def tearDown(self):
        self.proxy.shutdown()
        self.up.shutdown()

    def methods_log(self):
        return pathlib.Path(self.out, "methods.tsv").read_text().splitlines()

    def test_reads_are_forwarded(self):
        reply = post(self.port, b'{"jsonrpc":"2.0","id":7,"method":"getLastBlockId","params":[]}')
        self.assertEqual(reply, {"jsonrpc": "2.0", "id": 7, "result": "getLastBlockId"})
        self.assertEqual(Upstream.seen, ["getLastBlockId"])
        self.assertTrue(self.methods_log()[0].endswith("\tgetLastBlockId\tforwarded"))

    def test_send_transaction_is_kept_byte_for_byte_and_never_forwarded(self):
        body = b'{"jsonrpc": "2.0", "id": 28, "method": "sendTransaction", "params": ["AAEC"]}'
        reply = post(self.port, body)
        self.assertEqual(reply["id"], 28)
        self.assertEqual(reply["error"]["code"], export_proxy.EXPORTED_CODE)
        self.assertEqual(pathlib.Path(self.out, "send-1.json").read_bytes(), body)
        self.assertEqual(Upstream.seen, [])
        post(self.port, body)
        self.assertTrue(os.path.exists(os.path.join(self.out, "send-2.json")), "each send is kept in its own file")

    def test_upstream_refusals_are_passed_on_and_logged(self):
        reply = post(self.port, b'{"jsonrpc":"2.0","id":1,"method":"getProofsAndRoot","params":[[],[]]}')
        self.assertEqual(reply["error"]["code"], -32601)
        self.assertIn("\tgetProofsAndRoot\trefused upstream -32601", self.methods_log()[0])

    def test_batches_are_split(self):
        batch = [{"jsonrpc": "2.0", "id": 1, "method": "getFeeState", "params": []},
                 {"jsonrpc": "2.0", "id": 2, "method": "sendTransaction", "params": ["AA"]}]
        reply = post(self.port, json.dumps(batch).encode())
        self.assertEqual([r["id"] for r in reply], [1, 2])
        self.assertEqual(reply[0]["result"], "getFeeState")
        self.assertIn("error", reply[1])
        self.assertEqual(Upstream.seen, ["getFeeState"])


if __name__ == "__main__":
    unittest.main()
