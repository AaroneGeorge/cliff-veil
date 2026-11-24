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
     sequencer must refuse it; if it were accepted, the sequencer would not be checking proofs and the run stops there;
   - **the private claim**: `cliff-veil --json claim <schedule> --as <beneficiary> --private --yes` with dev mode unset.
     The SDK builds a `ClaimAsOf` to a one-time blinded address of a fresh private account of the wallet; the wallet
     proves it locally with `r0vm` and sends it. Wall time and peak memory are taken with `/usr/bin/time -v`, and every
     5 s a sampler writes the available memory, swap in use and the resident memory of `r0vm` and of the CLI;
   - fetches `getTransaction` and `getBlock` for that transaction as any client sees them, and runs
     `tools/scan_rpc/scan_rpc.py` on them: the canonical private account must appear 0 times;
   - a public claim of what vested meanwhile, then a cancel by the creator (the unvested part goes back);
   - writes `summary.json`.

## Run it

On GitHub: Actions, `real-proof`, "Run workflow" (input `swap_gb`, default 8: a swap file added as a margin), or

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
| `control-dev-receipt.json` | the refused dev-mode attempt |
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
- Only the claim to a private account is proved. Public transactions (deploys, create, public claim, cancel) carry no
  proof on LEZ: the sequencer executes them itself.
