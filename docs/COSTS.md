# Cost of each operation

Written by `cargo run -p vesting_cost` from the files in `evidence/cost/`; every number below is copied from those files. Do not edit by hand.

- **Executor cycles**: the transaction run through LEZ v0.4.0-rc3's public execution (`ValidatedStateDiff`, the code a sequencer runs), whole transaction including the clock read where the operation has one. The chain's cap is 10,000,000 cycles per transaction and per block.
- **Sequencer fee**: native units charged by a standalone sequencer, from the payer's balance change.
- A row without a number says why: `not built` (the program or message does not exist yet) or the refusal.

## executor run (LEZ v0.4.0-rc3)

- ids: `deploy ids file`
- vesting.bin sha256: `de7e1c0da03961c724683c0b70fa44ca069e4f96d38056361957661ee235f1ee`
- vesting_fees.bin sha256: `797931e66ee47da3dde0bd712442ee53028595f87c617ab8b163b6293c416f69`

| Operation | Asset | Schedules | Cycles | Fee | Status |
|---|---|---|---|---|---|
| native_transfer | - | 1 | 0 | - | ok |
| clock_tick | - | 1 | 7745 | - | ok |
| create | native | 1 | 92929 | - | ok |
| claim | native | 1 | 99583 | - | ok |
| claim_as_of | native | 1 | 57142 | - | ok |
| transfer_beneficiary | native | 1 | 50679 | - | ok |
| cancel | native | 1 | 96644 | - | ok |
| make_non_cancelable | native | 1 | 48182 | - | ok |
| signal_milestone | native | 1 | 49137 | - | ok |
| create_batch | native | 1 | 95508 | - | ok |
| create_batch | native | 8 | 457501 | - | ok |
| create_batch | native | 32 | 1704154 | - | ok |
| create_batch (program cap) | native | 164 | 8515387 | - | ok |
| create_batch (cap + 1, must be refused) | native | 165 | - | - | refused: Failed to execute program: Guest panicked: cliff-veil error 25: too many schedules in one batch |
| create | token | 1 | - | - | not measured yet (token setup: definition and holdings) |
| claim | token | 1 | - | - | not measured yet (token setup: definition and holdings) |
| cancel | token | 1 | - | - | not measured yet (token setup: definition and holdings) |
| check_fee | - | 1 | - | - | not built (vesting_fees; build with --features fees) |
| set_fee | - | 1 | - | - | not built (vesting_fees; build with --features fees) |

## sequencer run (LEZ v0.4.0-rc3)

- rpc: `http://127.0.0.1:3050`
- RISC0_DEV_MODE of this client: `1`

| Operation | Asset | Schedules | Cycles | Fee | Status |
|---|---|---|---|---|---|
| native_transfer | - | 1 | - | 4088 | ok |
| create | native | 1 | - | 759544 | ok |
| claim | native | 1 | - | 795888 | ok |
| make_non_cancelable | native | 1 | - | 389416 | ok |
| cancel | native | 1 | - | 778248 | ok |
| signal_milestone | native | 1 | - | 394512 | ok |
| transfer_beneficiary | native | 1 | - | 409648 | ok |
