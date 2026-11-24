# Private claim with a real proof

`tools/real_proof/run.sh` makes one private claim with a real RISC Zero proof against a LEZ v0.4.0-rc3 standalone
sequencer that verifies proofs, and records every step. The workflow `.github/workflows/real-proof.yml` runs it on a
GitHub-hosted runner (4 cores, 16 GiB) and uploads the record as an artifact. Everywhere else in this repository private
claims run in dev mode (`RISC0_DEV_MODE=1`, no real proof) and are labelled so; this is the one place where they do not.

## What a run does

1. Bootstraps the pinned toolchain and LEZ binaries (`scripts/bootstrap.sh`; the LEZ binaries are cached by commit).
2. Starts the `local-pub` sequencer (`scripts/seq.sh pub start`): standalone, mock Bedrock, LEZ's local genesis,
   `RISC0_DEV_MODE` unset, so it checks the proof of every privacy-preserving transaction.
3. Builds the four guests in the pinned image and deploys them (`scripts/deploy_local.sh pub --build release`), which
   writes `deploy/ids-local-pub.toml` with the program and image ids.
4. `run.sh`:
   - checks that dev mode is off: not in its own environment, not in the sequencer process's environment
     (`/proc/<pid>/environ`), and `dev_mode = false` in the ids file;
   - opens a fresh creator and beneficiary (public), funded from LEZ's local genesis test account;
   - creates a native linear schedule (cliff after 30 s, end after 3 h) and waits for the cliff;
   - **control**: sends the private claim once with a dev-mode receipt (`RISC0_DEV_MODE=1` for that one command). The
     sequencer must refuse it; if it were accepted, the sequencer would not be checking proofs and the run stops there.
     A client only sees that the transaction is never included, so the run also finds the sequencer's own log line for
     that transaction hash and requires it to name the proof as the reason (`control_dev_mode_receipt.reason` in
     `summary.json`, the line in `control-sequencer-log.txt`);
   - **the private claim**: `cliff-veil --json claim <schedule> --as <beneficiary> --private --yes` with dev mode unset.
     The SDK builds a `ClaimAsOf` to a one-time blinded address of a fresh private account of the wallet; the wallet
     proves it locally with `r0vm` and sends it. Wall time and peak memory are taken with `/usr/bin/time -v`, and every
     5 s a sampler writes the available memory, swap in use and the resident memory of `r0vm` and of the CLI;
   - fetches `getTransaction` and `getBlock` for that transaction as any client sees them, and runs
     `tools/scan_rpc/scan_rpc.py` on them: the canonical private account must appear 0 times;
   - a public claim of what vested meanwhile, then a cancel by the creator (the unvested part goes back);
   - writes `summary.json`;
   - **token claim to a private holding** (on by default; `CV_REAL_PROOF_TOKEN=0`, or the workflow input `token: 0`,
     skips it). It runs only after the native record is written, so its failure cannot hide the native result; the run
     then still exits non-zero. `e2e/examples/real_proof_token.rs` takes the steps of the end-to-end test
     `f2_token_claim_private_to_an_owned_private_holding` with real proofs: a fixed-supply token and a schedule of it
     (public); a private fee payer funded from the beneficiary's public balance (proved), since a token claim brings
     no native balance to pay its fee with; the destination private account initialises its own holding of the token,
     fee paid by that payer (proved); the SDK's private claim to the holding, fee paid by the payer (proved; the SDK
     names `vesting` and the token program to the prover, `Tx::prover_programs`). The claim's RPC answers are scanned
     for the destination and the fee payer, which must both appear 0 times. The result is `token_private_claim` in
     `summary.json`, with each step's hash, block and seconds from send to inclusion (proof included).

## Run it

On GitHub: Actions, `real-proof`, "Run workflow" (inputs `swap_gb`, default 8: a swap file added as a margin; `token`,
default 1), or a push to the `proof-runs` branch, or

```sh
gh workflow run real-proof.yml -R AaroneGeorge/cliff-veil
gh run watch -R AaroneGeorge/cliff-veil <run id>
gh run download -R AaroneGeorge/cliff-veil <run id> -n real-proof-<run id>
```

On your own machine (at least 10 GiB of free memory, 4 or more cores), after `scripts/bootstrap.sh`:

```sh
scripts/seq.sh pub start
scripts/deploy_local.sh pub --build release
tools/real_proof/run.sh /tmp/real-proof
```

## The record

| File | What it holds |
|---|---|
| `summary.json` | machine, sequencer, program ids, every transaction (step, hash, block), the control's answer, the private claim (amount, accounts, wall time, peak memory, transaction size, scan result), the public claim, the cancel, the final schedule and balances |
| `run.log` | every command and its full output, in order |
| `private-claim.json`, `private-claim.time` | the CLI's answer and `/usr/bin/time -v` for the proved claim |
| `memory.tsv` | the 5 s memory samples during the proved claim |
| `private-claim-transaction.json`, `private-claim-block.json` | the RPC answers for the proved claim; the transaction bytes carry the proof |
| `scan.txt` | the privacy scan of those answers |
| `control-dev-receipt.json`, `control-sequencer-log.txt` | the refused dev-mode attempt, and the sequencer's log line giving the reason |
| `token-summary.json`, `token-steps.json`, `token.time`, `memory-token.tsv`, `token-claim-transaction.json`, `token-claim-block.json`, `token-scan.txt` | the token part, as for the native claim |
| `sequencer.log`, `ids-local-pub.toml` | the sequencer's log and the deployed ids |

Local paths in the record are replaced by `<build>` and `~`. A run kept in this repository is under
`evidence/real-proof/<UTC time>/`.

## What it shows, and what it does not

- It shows that a private claim of this program can be proved on ordinary hardware (a hosted runner), how long that
  takes and how much memory it needs, and that a sequencer that checks proofs includes it, while it refuses the same
  claim with a dev-mode receipt.
- The proof is made on a GitHub-hosted runner, not on the claimant's own machine; timings depend on that machine
  (`machine` in `summary.json`).
- The sequencer is ephemeral: a standalone LEZ sequencer started inside the runner, with mock Bedrock and LEZ's local
  genesis, and discarded with it. It is not a public Logos network, and nobody can query it after the run. What stays
  is the record: every transaction hash and block, the RPC answers for the proved claim, and the sequencer's log.
  Anyone can run the workflow again on the same commit and compare.
- The wall time covers the whole CLI command: local checks, the dry run, the proof, sending, and waiting for inclusion
  (one 15 s block or so). The memory samples show how much of it the prover ran.
- The token part's memory and time cover its whole program (three proofs one after the other); each step's own
  seconds are in `token_private_claim.steps`.
- Only privacy-preserving transactions are proved. Public transactions (deploys, create, public claim, cancel) carry no
  proof on LEZ: the sequencer executes them itself.
