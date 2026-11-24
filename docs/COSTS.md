# Cost of each operation

Written by `cargo run -p vesting_cost` from the files in `evidence/cost/`, `evidence/batch/max.txt` and `evidence/real-proof/*/` (`summary.json`, `private-claim.time`); every number below is copied from those files. Do not edit by hand.

All runs are on LEZ v0.4.0-rc3 (a local standalone sequencer and its executor). LEZ testnet 0.4 has no public endpoint yet; the same commands will be run there when it has.

- **Cycles**: the transaction run through LEZ v0.4.0-rc3's public execution (`ValidatedStateDiff`, the code a sequencer runs), whole transaction: every program it calls (`vesting`, `vesting_fees`, the token or native program, the clock read where the operation has one). The chain's cap is 10,000,000 cycles per transaction and per block.
- **Bytes**: the size of the transaction as a sequencer receives it (`LeeTransaction::Public`, Borsh). A sequencer accepts a transaction up to its block size less 2,048 bytes (1,048,576 - 2,048 by default).
- **Fee**: native units charged by a standalone sequencer, from the payer's balance change.
- `init`, `set_fee`, `check_fee`: messages of `vesting_fees` sent alone, to a deployment of the same binary whose admin key the run holds. Inside a creation `vesting` sends `CheckFee` itself; that cost is part of every `create` row.
- A row without a number says why: `not built` (a guest was missing from the run) or the refusal.

## executor run (LEZ v0.4.0-rc3)

- ids: `deploy ids file`
- vesting.bin sha256: `de7e1c0da03961c724683c0b70fa44ca069e4f96d38056361957661ee235f1ee`
- vesting_fees.bin sha256: `797931e66ee47da3dde0bd712442ee53028595f87c617ab8b163b6293c416f69`
- token.bin sha256: `7b0178907ac0cfc61542d7d5585481dbbbfa88f594df37cdb3ebb2b88c9db1b5`

| Operation | Asset | Schedules | Cycles | Bytes | Fee | Status |
|---|---|---|---|---|---|---|
| native_transfer | - | 1 | 0 | 480 | - | ok |
| clock_tick | - | 1 | 7745 | 175 | - | ok |
| create | native | 1 | 92929 | 782 | - | ok |
| claim | native | 1 | 99583 | 655 | - | ok |
| claim_as_of | native | 1 | 57142 | 599 | - | ok |
| transfer_beneficiary | native | 1 | 50679 | 463 | - | ok |
| cancel | native | 1 | 96644 | 623 | - | ok |
| make_non_cancelable | native | 1 | 48182 | 431 | - | ok |
| signal_milestone | native | 1 | 49137 | 432 | - | ok |
| create_batch | native | 1 | 95508 | 786 | - | ok |
| create_batch | native | 8 | 457501 | 2242 | - | ok |
| create_batch | native | 32 | 1704154 | 7234 | - | ok |
| create_batch (program cap) | native | 164 | 8515387 | 34690 | - | ok |
| create_batch (cap + 1, must be refused) | native | 165 | - | - | - | refused: Failed to execute program: Guest panicked: cliff-veil error 25: too many schedules in one batch |
| create | token | 1 | 190078 | 878 | - | ok |
| claim | token | 1 | 142004 | 655 | - | ok |
| cancel | token | 1 | 139063 | 623 | - | ok |
| create_batch | token | 8 | 1222369 | 2338 | - | ok |
| create_batch (program cap) | token | 57 | 8433456 | 12530 | - | ok |
| create_batch (cap + 1, must be refused) | token | 58 | - | - | - | refused: Failed to execute program: Guest panicked: cliff-veil error 25: too many schedules in one batch |
| create_with_fee (50 bps) | native | 1 | 95542 | 846 | - | ok (fee 5000 received by the treasury) |
| create_with_fee (50 bps) | token | 1 | 248245 | 942 | - | ok (fee 5000 received by the treasury's associated token account) |
| init | - | 1 | 37851 | 475 | - | ok |
| set_fee | - | 1 | 52078 | 498 | - | ok |
| check_fee | - | 1 | 19900 | 465 | - | ok |

## private claim run (LEZ v0.4.0-rc3)

- source: `evidence/cli/local-dev-lifecycle.log`
- transaction: `8ea2818816b3eab753f2f194c00a54e529a2d30ba1fcabc682e07dba63dff345 in block 856`
- RISC0_DEV_MODE: `1`

| Operation | Asset | Schedules | Cycles | Bytes | Fee | Status |
|---|---|---|---|---|---|---|
| claim_private | native | 1 | - | - | 5528416 | ok (dev mode: the proof is a dev-mode receipt, so no proving cost is shown; a real proof is in the section below; the fresh private account received 100006667 and holds 94478251 after paying the fee) |

## sequencer run (LEZ v0.4.0-rc3)

- rpc: `http://127.0.0.1:3050`
- RISC0_DEV_MODE of this client: `1`

| Operation | Asset | Schedules | Cycles | Bytes | Fee | Status |
|---|---|---|---|---|---|---|
| native_transfer | - | 1 | - | - | 4088 | ok |
| create | native | 1 | - | - | 759544 | ok |
| claim | native | 1 | - | - | 795888 | ok |
| make_non_cancelable | native | 1 | - | - | 389416 | ok |
| cancel | native | 1 | - | - | 778248 | ok |
| signal_milestone | native | 1 | - | - | 394512 | ok |
| transfer_beneficiary | native | 1 | - | - | 409648 | ok |

## Largest batch the chain accepts

From `evidence/batch/max.txt` (executor, budget of one transaction). A batch over the budget is refused whole (out of gas) and changes nothing. The program caps a batch at 85% of the linear maximum: 164 native and 57 token schedules (`vesting_core::MAX_BATCH_NATIVE`, `MAX_BATCH_TOKEN`); the SDK splits a longer list into several transactions, each filled to an estimated 85% of the cycle budget and never above the cap. The byte size of a batch at the cap is in the executor table above.

| Item shape | Largest accepted | Smallest refused | Cycle budget |
|---|---|---|---|
| native | 193 | 194 | 10000000 |
| token | 67 | 68 | 10000000 |
| native-64-milestones-with-fee | 61 | 62 | 10000000 |
| token-64-milestones-with-fee | 38 | 39 | 10000000 |

## private claim with a real proof (LEZ v0.4.0-rc3)

Every record under `evidence/real-proof/`, newest first: one run. The figures come from each record's `summary.json` and `private-claim.time` (and `token.time` for the token part); the memory samples are in `memory.tsv`, the privacy scan in `scan.txt`, the sequencer's log in `sequencer.log`. The method and its limits are in [tools/real_proof/README.md](../tools/real_proof/README.md).

### 20261124T155037Z: one run on a hosted runner

- source: `evidence/real-proof/20261124T155037Z/`, repository commit `5b1101d1dd66225df17af089a4a69a82a547ea82`, workflow run [45422127636](https://github.com/AaroneGeorge/cliff-veil/actions/runs/45422127636)
- transaction: `66d6e06d68ae4b76eca4d21996809286a1aebae4e94abdf98a286d1c05200d43` in block 193
- RISC0_DEV_MODE: unset in the client and in the sequencer's environment (`risc0_dev_mode_in_its_environment: false`)
- sequencer: standalone LEZ v0.4.0-rc3 that verifies proofs, started for this run on the same machine and discarded with it
- machine: GitHub-hosted runner, 4 CPUs (AMD EPYC 9V45 96-Core Processor), 15,983 MiB of memory, 11,263 MiB of swap in total (including any swap file the workflow added as a margin), image `ubuntu24 20261116.376.1`, prover `risc0-r0vm 3.0.5`

| Measure | Value |
|---|---|
| Claimed | 305,555,555 (the private balance after the claim: 300,026,771) |
| Network fee estimate | 5,528,784 |
| Cycles (the CLI's estimate before sending) | 57,271 |
| Wall time of the whole CLI command | 1,229 s (20:29.49 by `/usr/bin/time -v`) |
| Time `r0vm` was seen running | about 1,200 s (sampled every 5 s) |
| CPU use | 370% (user 4,541.95 s, system 15.95 s) |
| Peak memory of the command (maximum resident set size) | 9,612,576 KiB (9.17 GiB) |
| Peak sampled memory of `r0vm` / of the CLI | 9,445,464 KiB / 37,352 KiB |
| Peak swap in use (sampled) | 1,737,444 KiB |
| Lowest available memory (sampled) | 5,874,316 KiB |
| Size of the transaction (`getTransaction`) | 251,270 bytes |
| Privacy scan of `getBlock` and `getTransaction` | canonical private account found 0 times, one-time address 8 times |

The wall time covers the whole command: local checks, the dry run, the proof, sending, and waiting for the transaction to be included. Public transactions carry no proof on LEZ, so the public claim (`ee6ac767...`, block 194, 1,138,893,519) and the cancel (`7246a352...`, block 195) of the same run were not proved. The same claim made with a dev-mode receipt was not included, and the sequencer's log names the reason: `State transition failed at transaction 0: Invalid privacy preserving execution circuit proof, skipping it`. The privacy scan of this transaction is in [PRIVACY.md](PRIVACY.md).

This run predates the token part of `run.sh`.

What the run does not show: how the time scales on other machines (a different runner, a laptop), or a second run on the same kind of machine. Nor a token claim with real proofs (which needs three more proofs): a token private claim is so far measured in dev mode on a sequencer only.
