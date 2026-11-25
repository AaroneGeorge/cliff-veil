# tools/public_endpoint: a public read-only endpoint for the local-pub sequencer

`local-pub` (`scripts/seq.sh pub`: LEZ v0.4.0-rc3 standalone, `RISC0_DEV_MODE` unset) holds every transaction of
`evidence/public-chain/`. Its JSON-RPC listens on `127.0.0.1:3051` only and has no authentication of its own,
including for `sendTransaction`. This folder makes 12 of its read methods reachable from the internet and nothing
else.

```
 visitor --HTTPS--> TLS front --> relay.py (container, port 8080: a queue)
                                     ^ port 8081: GET /pull, POST /push, bearer token (never published)
                                     | the sequencer's host opens this connection
            dialer.py on the host ---+--> http://127.0.0.1:3051 (allow-listed reads only)
```

The container cannot open connections to the host, so the relay never calls the sequencer; the host dialer calls
out to the relay instead. Nothing new listens on the host.

| File | What |
|---|---|
| `policy.py` | the allow-list and every limit; used by both halves |
| `relay.py` | the public half: checks each request, queues it, hands it to the host dialer, returns the reply |
| `dialer.py` | the host half: long-polls the relay, checks each request again, calls the sequencer, returns the reply |
| `Dockerfile` | the relay image (`python:3.12-alpine` pinned by digest, user 65534, standard library only) |
| `test_policy.py` | unit and in-process tests (no network, no sequencer) |

## Policy

- Forwarded: `getTransaction`, `getBlock`, `getBlockRange` (at most 16 blocks), `getLastBlockId`, `getAccount`,
  `getAccountView`, `getAccountBalance`, `getAccountsNonces` (at most 16 ids), `getProgramIds`, `getFeeState`,
  `getChannelId`, `checkHealth`.
- Refused with `-32601` "not available on this read-only endpoint": `sendTransaction`,
  `requeueCrossZoneDeadLetter`, `getPublications`, `getMessagePath`, `getProofsAndRoot`, `getRecoveryBinding`,
  `getCrossZoneDeadLetters`, `rpc.*`, any other name.
- One JSON object per request (a batch is `-32600`), only the keys `jsonrpc`, `id`, `method`, `params`, no duplicate
  keys, no notifications. The request is re-encoded before it reaches the sequencer, so the sequencer sees only what
  the policy checked.
- `POST /` only (plus `GET /` help and `GET /health`); 2 KiB per request (HTTP 413 above); replies at most 2 MiB.
- 5 requests a second per visitor, burst 20, 20 a second in total. A visitor a little over the rate is slowed by up
  to 1 s; more gets HTTP 429.
- At most 64 requests waiting, 4 sequencer calls at once, 5 s per sequencer call, 10 s per request (HTTP 504);
  HTTP 503 while the host dialer is away.

The host dialer checks every request again with the same `policy.py`, refuses to talk to a sequencer that is not on
loopback, and keeps per-method counters (`stats.json` next to the token file): a relay that is broken or taken over
can still ask only for allow-listed reads.

## Run

Run `relay.py` as a container (this `Dockerfile`) behind a TLS front that forwards HTTPS to its port 8080 and sets
`X-Forwarded-For`; give it `RELAY_TOKEN` (at least 32 characters, for example
`head -c 32 /dev/urandom | xxd -p -c 64`) in its environment. Port 8081 must stay unpublished: only the
sequencer's host opens connections to it. Then run `dialer.py` on the sequencer's host, with the same token in a file
of mode 600, best in its own cgroup:

```
env -i PATH=/usr/bin:/bin SEQ_URL=http://127.0.0.1:3051 RELAY_APP=<container app name> \
  RELAY_TOKEN_FILE=<token file> python3 -I tools/public_endpoint/dialer.py
```

`RELAY_APP` names the container (`<app name>.web.1`, address read with `docker inspect` at each reconnect);
`RELAY_URL=http://HOST:8081` gives the relay's address directly instead. Stop the dialer with SIGTERM (it writes its
counters and exits). The relay answers 503 while the dialer is away; the dialer reconnects by itself when the relay
restarts.

## Test

```
python3 -I tools/public_endpoint/test_policy.py      # 27 unit and in-process tests
scripts/verify_public.sh                             # re-reads evidence/public-chain/ through the public URL
```
