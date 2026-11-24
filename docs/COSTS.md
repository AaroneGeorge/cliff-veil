# Cost of each operation

Written by `cargo run -p vesting_cost` from the files in `evidence/cost/` and `evidence/batch/max.txt`; every number below is copied from those files. Do not edit by hand.

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
| claim_private | native | 1 | - | - | 5528416 | ok (dev mode: the proof is a dev-mode receipt, a real proof is not measured; the fresh private account received 100006667 and holds 94478251 after paying the fee) |

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
