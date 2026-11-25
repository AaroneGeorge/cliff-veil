# evidence/public-chain

Runs on `local-pub`: our standalone LEZ v0.4.0-rc3 sequencer with `RISC0_DEV_MODE` unset, readable by anyone,
read-only, at `https://cliff-veil-seq.tailcdec7a.ts.net` (relay: `tools/public_endpoint/`).

| File | What |
|---|---|
| `20261125T100306Z/run.json` | the native lifecycle: 40 transactions (deployment, account funding, every vesting operation) with block and description, 10 refusals with their error codes, 13 queries, the recorded state of 24 schedules. Written by `scripts/public_lifecycle.sh` |
| `20261125T100306Z/run.log` | every command of that run and its JSON output |
| `20261125T100306Z/steps/` | one JSON document per command (`NN.json`), the step index, funding transfers and schedule accounts |
| `20261125T100306Z/deploy.log` | the output of `scripts/deploy_local.sh pub --build release` (commit `91db156`, pinned builder image) |
| `20261125T100306Z/batch.csv` | the 20 recipients of the batch |
| `20261125T100631Z-token/run.json` | token schedules: 20 transactions of the two token end-to-end tests run against `local-pub` (`scripts/public_token_run.sh`), and the 2 token schedule accounts |
| `20261125T100631Z-token/run.log`, `rpc.jsonl` | the test output, and the recording relay's log of the requests the tests sent |
| `20261125T113302Z-private-real-proof/` | one private claim with a real proof (GitHub run 45598084620, `RISC0_DEV_MODE` unset, 1,572 s, about 9.17 GiB), proved against this endpoint and submitted by the sequencer's host: `run.json` (4 transactions, the claim is block 498, hash `660a13a03d5533d571a0d680beb56fcce043ca28de0146c403183b7a3477490a`), `submit.json`, `scan.txt` (destination 0 times, one-time address 8), `claim.time`, `export-summary.json`, `exported-tx.json`, `private-claim-block.json`, `private-claim-transaction.json`, `claim.json`, `methods.tsv` (every request the runner made, by method) and `memory.tsv` |
| `verify-20261125T1013Z.txt` | `scripts/verify_public.sh --public` over the internet route: 60 of 60 transactions in their recorded blocks, 26 of 26 schedules decoded and equal to the record, 10 of 10 program and config accounts present, `sendTransaction` refused; PASS |
| `verify-20261125T1042Z.txt` | the same command after the relay was redeployed with `getPublications` and `getProofsAndRoot` allowed: PASS, same counts |
| `endpoint-checks-20261125T1044Z.txt` | the probes after that change: the two new methods answered in the wallet's shapes (`getPublications` `[0,256]` and the empty page that ends a scan, `getProofsAndRoot` `[[],[]]`, one entry, 16 entries at their longest encoding), and refused with `-32602` over the caps (limit 257 or 0, a 17th commitment or position) and in every malformed shape tried (31-byte or base64 commitment, byte 256, float or negative numbers, a missing or extra param); writes and the remaining reads off the allow-list still `-32601`; the 4 KiB body cap; the dialer's counters (only allow-listed reads reached the sequencer) |
| `endpoint-checks-20261125T1014Z.txt` | the endpoint's answers to reads, writes, methods off the allow-list, a batch, a duplicate key, a 17-block range, a 3 KiB body (then over the 2 KiB cap) and a PUT, with the first allow-list; and the host dialer's counters after them (only allow-listed reads reached the sequencer) |

| `verify-20261125T1135Z.txt` | the same command with the real-proof claim added to the record: 64 of 64 transactions, 27 of 27 schedules, 15 of 15 program and config accounts, `sendTransaction` refused; PASS |

Dev mode is off on this sequencer. Every transaction here is a public one, executed by the sequencer, and carries no
proof, except the private claim of `20261125T113302Z-private-real-proof/`, whose real proof the sequencer checked.
The other private claims with real proofs are in `evidence/real-proof/`.
