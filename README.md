# cliff-veil

Privacy-preserving token vesting for the Logos Execution Zone (LEZ), written for RFP-017 and built against LEZ
`v0.4.0-rc3`.

A creator locks native LEZ or a LEZ token in a per-schedule escrow account and releases it to a beneficiary on a cliff
plus linear curve, a fully linear curve, or creator-signalled milestones. The beneficiary claims to a public account or
to a private account. A claim to a private account is a privacy-preserving transaction: the destination private account
and the later movements of the claimed tokens are not recorded in public on-chain state. The beneficiary address, the
schedule and every claim amount are public. [docs/PRIVACY.md](docs/PRIVACY.md) states exactly what is visible.

The repository holds the vesting program and its fee-switch program, a Rust SDK, a command-line client, the SPEL IDL, a
Logos mini-app, the end-to-end tests, and the evidence files the numbers in these documents come from.

## Status

| Item | State |
|---|---|
| Create (cliff + linear, fully linear, milestones, batch), claim to a public account, cancel, make non-cancelable, signal a milestone, transfer the beneficiary, creation-fee switch | Runs on a LEZ v0.4.0-rc3 standalone sequencer in dev mode. The end-to-end suite passed 34 of 34 tests on 2026-11-24 ([evidence/e2e/local-dev/20261124T140628Z.json](evidence/e2e/local-dev/20261124T140628Z.json)). |
| Native and token schedules | Both are covered by tests. Cost rows for token operations are not measured yet ([docs/COSTS.md](docs/COSTS.md)). |
| Claim to a private account | Runs in RISC Zero dev mode only (`RISC0_DEV_MODE=1`: the transaction carries no real proof). Every private-claim figure and log in this repository is a dev-mode figure. A claim with a real proof has not been run. |
| LEZ testnet 0.4 | Not yet live. Nothing is deployed there. The suite runs on a local standalone sequencer instead; program ids of that network are in [deploy/ids-local-dev.toml](deploy/ids-local-dev.toml). |
| Mini-app | Both packages build with Nix, the core module installs, loads and answers under `logosctl` 0.3.2, and the view renders offscreen. Loading in Basecamp is not verified, and neither is a run of the mini-app against a live sequencer ([evidence/miniapp/README.md](evidence/miniapp/README.md)). |
| Guest builds | Reproducible: independent builds in the pinned RISC Zero builder image give identical binaries and image ids ([evidence/guests/reproducible-local-dev.log](evidence/guests/reproducible-local-dev.log)). |
| Cost measurements | Executor cycles and standalone-sequencer fees ([docs/COSTS.md](docs/COSTS.md)). They are not testnet 0.4 measurements. |

## Quick start (local network)

Everything below runs on one Linux x86-64 machine against a standalone LEZ v0.4.0-rc3 sequencer in dev mode.

1. **Set up the machine.** `scripts/bootstrap.sh --with-sudo` installs the system packages, the RISC Zero toolchain
   (`rzup`), fetches LEZ at the pinned commit (`scripts/pins.txt`) next to the checkout, and builds the LEZ standalone
   sequencer and wallet (about 20 minutes on 4 cores). `scripts/bootstrap.sh --check` only reports what is missing.
   Docker is needed only for the reproducible guest build. Paths (build directory, run directory, wallet homes) are
   described in the header of `scripts/env.sh`.
2. **Start a sequencer.**
   ```bash
   scripts/seq.sh dev start     # dev mode, RPC http://127.0.0.1:3050, 15 s blocks, LEZ's local genesis
   scripts/seq.sh dev wallet-check
   ```
   `start` prints the `LEE_WALLET_HOME_DIR=...` line to use for the wallet of this sequencer. `scripts/seq.sh dev stop`
   stops it. `scripts/seq.sh pub start` starts a second one with dev mode unset (RPC 3051).
3. **Deploy the programs.**
   ```bash
   make deploy-local            # scripts/deploy_local.sh dev
   ```
   This builds the guests with the local RISC Zero toolchain, deploys `token`, `vesting_fees` and `vesting` as immutable
   programs (in that order, so `vesting` can embed the other ids), sends the `Init` of `vesting_fees` from the deployer,
   and writes every id to `deploy/ids-local-dev.toml`. `scripts/deploy_local.sh dev --build release` builds the guests in
   the pinned Docker image instead, so the image ids can be rebuilt and checked by anyone (`scripts/check_image_ids.sh`),
   and also deploys `ata`, which only the release build produces; without it a token creation fee stays at rate 0.
4. **Build the command line.**
   ```bash
   source scripts/env.sh
   cargo build --profile host -p vesting_cli    # $CARGO_TARGET_DIR/host/cliff-veil
   ```
5. **Make accounts.** The wallet is LEZ's own. The deploy step imports LEZ's published local test account under the
   label `cv-payer`; fund fresh accounts from it:
   ```bash
   export LEE_WALLET_HOME_DIR=<the directory seq.sh printed> LEE_WALLET_PASSWORD=
   wallet account new public --label creator
   wallet account new public --label alice
   wallet auth-transfer send --from cv-payer --to creator --amount 5000000000
   wallet auth-transfer send --from cv-payer --to alice   --amount 100000000
   ```
   The CLI accepts wallet labels, `Public/<id>`, `Private/<id>` or bare base58 ids wherever it takes an account.

### Program ids

The ids are the program-loader header accounts. These are the ids of the local-dev deployment recorded in
[deploy/ids-local-dev.toml](deploy/ids-local-dev.toml), which belongs to that one local chain: a new local chain gets
new ids, and `make deploy-local` rewrites the file.

| Program | Id on local-dev |
|---|---|
| `vesting` | `5nCVf47kVHKC9RwgfyokEgYM1n2wdjEhS3D9z7RkJFsZ` |
| `vesting_fees` | `D19cmwnGteSuS1i5aBtuvEdpRS9yeFM3UYk9pN87Uex5` |
| token (lez-programs, see [docs/REUSE.md](docs/REUSE.md)) | `3uSMsxaS8RE6eLbLg3xwvLedXWmRp88LnJUhJ9xLKGtY` |
| associated token account | `4AwKPBQecPM5pFpGQ1DNny8U7vefMgTpJZQrpTacrLDU` |
| `vesting_fees` config account | `5aH6Z3GiECyNPUrUJTYQ1sgh67z8MpA4LQsNUfxa84jb` |

There are no testnet 0.4 ids yet. A deployment there is recorded in a file of the same shape next to the local one.
`vesting` has the ids of the token, fee and ATA programs compiled in, so its image id is fixed per network; the ids file
holds the image id built for each program, and `[build]` records how.

## Command line

`cliff-veil` is a thin layer over the SDK. It reads the program ids from `--ids` (default: `deploy/ids-local-dev.toml`
in the working directory, else the file named by `CV_IDS`), opens the wallet from `LEE_WALLET_HOME_DIR`, and talks to
the sequencer the wallet is configured for. The examples assume it is on `PATH` and run from the repository root.

Every command that changes state first checks the operation against current state, prints what it will do and the
estimated network fee, and then sends. `--dry-run` stops after the check, `--json` prints one JSON object, and `--yes`
skips the confirmation (only private claims ask). The SDK runs every transaction locally before sending it, so a
refusal comes back with its program error code and an action to take, and nothing is sent ([docs/PLATFORM-FINDINGS.md](docs/PLATFORM-FINDINGS.md),
finding 7). The error codes are in [SPEC.md](SPEC.md) section 5.

Times are milliseconds since the Unix epoch, `now`, or an offset from the chain clock (`+30s`, `+10m`, `+365d`; units
`ms s m h d`). A schedule is named by its account; `create` prints it, and
`cliff-veil address --creator <account> --label <label>` recomputes it from the creator and the label the schedule id
was derived from.

### Create

```bash
# Cliff + linear: 1,000,000,000 units, a 1 year cliff and a 4 year end. The share vested at the cliff
# (365/1460 of the total) unlocks at once, then the rest accrues linearly to the end.
cliff-veil create --from creator --beneficiary alice --total 1000000000 \
    --start now --cliff +365d --end +1460d --label team-alice

# Fully linear: no cliff, accrual from the start.
cliff-veil create --from creator --beneficiary alice --total 1000000000 --start now --end +90d

# Milestones: three tranches that the milestone authority (default: the creator) unlocks one by one.
cliff-veil create --from creator --beneficiary alice --milestones 300,300,400

# A token schedule, cancelable by a separate authority, transferable by the beneficiary.
cliff-veil create --from creator --beneficiary alice --token <token definition account> --total 5000 \
    --start now --end +30d --cancel-authority <multisig account> --transferable

# Created non-cancelable (the default is cancelable).
cliff-veil create --from creator --beneficiary alice --total 1000 --start now --end +10d --non-cancelable
```

Options not shown: `--milestone-authority`, `--refund-to` (default: the account that funded the escrow), `--id` (a
schedule id as 64 hex characters; default: random, or derived from `--label`). The creator signs, funds the escrow with
exactly `--total` and pays the creation fee (0 today, see [Fees](#fees)).

**Batch.** One schedule per recipient of a CSV file (`beneficiary,total[,label]`, optional header line, `#` comments) or
a JSON array (`[{"beneficiary": "...", "total": "1000", "label": "..."}]`):

```bash
cliff-veil create-batch --from creator --file recipients.csv --start now --cliff +365d --end +1460d
cliff-veil create-batch --from creator --file recipients.csv --milestone-bps 2500,7500     # milestone weights in basis points
cliff-veil create-batch --from creator --file recipients.csv --end +90d --dry-run          # how many transactions, what fee
```

One transaction holds at most 164 native schedules or 57 token schedules (`MAX_BATCH_NATIVE`, `MAX_BATCH_TOKEN` in
`core/src/create.rs`); a longer list is split into several transactions by an estimate of the cycles each needs. The
caps and the measurement behind them (the largest batch the chain accepts, and the next size refused) are in
[evidence/batch/max.txt](evidence/batch/max.txt) and [docs/DECISIONS.md](docs/DECISIONS.md) (decision 8); the 164-schedule
batch costs 8,515,387 cycles in [docs/COSTS.md](docs/COSTS.md).

### Show, claimable, list

```bash
cliff-veil show <schedule>                       # terms, vested, claimed, claimable now, locked, next unlock, escrow
cliff-veil claimable <schedule>                  # the amount claimable now and the next unlock
cliff-veil list --creator creator                # schedules a creator opened (a scan of the chain's blocks)
cliff-veil list --beneficiary alice --active     # schedules that currently pay alice, still active
```

`list` scans blocks for creation transactions, because a standalone sequencer serves no events (finding 5), and keeps a
cache file in the wallet home (`--no-cache` skips it). "Claimable now" is computed at the timestamp of the last block,
the time the clock reports to a transaction in the next block.

An excerpt of `show` on a live local schedule, from [evidence/cli/local-dev-lifecycle.log](evidence/cli/local-dev-lifecycle.log):

```text
schedule            9n9eEzUE6zENnQyPoynXaka7EMxDREqWquyd7iMSWv16
standing            active
kind                linear: start 2026-11-24T14:01:12.294Z, cliff 2026-11-24T14:01:42.294Z, end 2026-11-24T14:03:42.294Z
total               1000000000
vested              500000000
claimed             399993333
claimable now       100006667
locked              500000000
next unlock         2026-11-24T14:02:27.295Z
escrow              600006667
```

A claim with nothing to claim is refused before anything is sent, with the next unlock time:

```text
error: cliff-veil error 14: nothing claimable yet; next unlock at 2026-11-24T14:01:42.294Z (1795528902294 ms)
```

### Claim to a public account

```bash
cliff-veil claim <schedule> --as alice                     # to alice's own account
cliff-veil claim <schedule> --as alice --to <account>      # to another public account
```

The beneficiary signs. The claim pays everything vested and not yet claimed, computed at the clock time of the
including block, in one transaction. A token claim needs the destination to hold a token holding of the definition
already (the SDK refuses with the action to take when it does not). The summary the command prints, from the same log:

```text
Claim summary (public claim)
  schedule:       9n9eEzUE6zENnQyPoynXaka7EMxDREqWquyd7iMSWv16
  beneficiary:    H75zu1PGkQmek6WesxQ4LPomWrVKXztcK1xz4EuYXTCz
  claimable now:  at least 399993333 (computed at the clock time of the including block)
  destination:    H75zu1PGkQmek6WesxQ4LPomWrVKXztcK1xz4EuYXTCz (public account)
  claimed after:  399993333
  still to come:  600006667
  network fee:    ~803320 (estimated), paid by H75zu1PGkQmek6WesxQ4LPomWrVKXztcK1xz4EuYXTCz which will hold 100000000
```

### Claim to a private account

```bash
cliff-veil claim <schedule> --as alice --private                          # to a fresh private account of this wallet
cliff-veil claim <schedule> --as alice --private --private-to <private account> --yes
cliff-veil claim <schedule> --as alice --private --fee-payer <private account>   # a token claim needs one
```

The claim is a privacy-preserving transaction proved by the claimant's wallet. Before it is sent the command prints the
summary, the privacy disclosure below and, on a terminal, asks for confirmation; without a terminal it refuses to send
unless `--yes` is given, so a script has to say so on purpose. The mini-app shows the same text and does not send until
it is acknowledged.

```text
Privacy of this claim (claim to a private account)
  Visible on-chain, to everyone:
    - the claim amount
    - the beneficiary address
    - the vesting schedule address
    - the time of the claim
  Not recorded in public on-chain state:
    - the destination private account
    - later movements of the claimed tokens
  Limitation: the beneficiary address is publicly linked to this schedule from its creation, and every claim
  from it is linked to that address. Privacy covers where the claimed tokens go, not who claimed them.
```

What the SDK checks first: the destination is a private account of this wallet (a public account is refused with an
explicit error before anything is sent), the claim is larger than the network fee of a privacy-preserving transaction
when it has to pay its own fee, and a token claim has a private fee payer holding native balance and a private holding of
the definition. A native claim into a fresh private account pays its own network fee out of the claimed amount. In a
dev-mode run this fee was ~5528416 against ~803320 for the public claim above, and the private balance after the claim
was 94478251 from a claim of 100006667. In dev mode the command also prints `dev mode: RISC0_DEV_MODE=1, ...` and the
transaction carries no real proof; on a network that verifies proofs the claim is proved locally first, which takes
minutes.

### Cancel, make non-cancelable, signal, transfer

```bash
cliff-veil cancel <schedule> --as creator                 # unvested part to the refund account; vested part stays claimable
cliff-veil make-non-cancelable <schedule> --as creator    # one way; the creator or the cancel authority may do it
cliff-veil signal <schedule> --index 0 --as creator       # milestone authority; unlocks that milestone's amount
cliff-veil transfer <schedule> --to bob --as alice        # hand a transferable schedule to a new beneficiary
```

`cancel` is allowed only for a cancelable schedule and only for the cancel authority (default: the creator). It freezes
vesting at the effective time of the cancel, pays `total - vested` to the refund account, and leaves the vested and
unclaimed part claimable by the beneficiary. A second cancel is refused with code 16. Signalling the same milestone
twice is refused with code 18 and unlocks nothing. `transfer` is refused with code 19 on a schedule that was not created
transferable; transferability cannot be changed after creation.

A complete local run of create, show, a refused claim, a public claim, a private claim with the scan of its transaction,
a refused private claim to a public account, a cancel and a refused second cancel is in
[evidence/cli/local-dev-lifecycle.log](evidence/cli/local-dev-lifecycle.log).

### Fees

```bash
cliff-veil fee-show
```

```text
creation fee rate   0 bps (hard cap 100 bps = 1%)
treasury            not set
charged             at creation only, from the funding account, in the schedule's asset
config account      5aH6Z3GiECyNPUrUJTYQ1sgh67z8MpA4LQsNUfxa84jb
```

## Mini-app (Logos Basecamp)

The mini-app has a recipient view (position, total locked, claimable now, next unlock, claim with a pre-claim summary,
fee check and the privacy disclosure) and a creator view (create with every parameter, list active schedules, cancel,
signal a milestone). It is two Nix packages built from this repository, a core module and a QML view, and the core
module runs the `cliff-veil` command above.

```bash
nix build 'github:AaroneGeorge/cliff-veil?dir=miniapp#lgx-portable' -o cliff-veil-core
nix build 'github:AaroneGeorge/cliff-veil?dir=miniapp/ui#lgx-portable' -o cliff-veil-ui
cargo build --profile host -p vesting_cli          # the command line the core module runs
```

In Basecamp (0.3.2 or later): Package Manager, Install Local Package, the core package and then the view; Settings,
Module Inspector, load `cliff_veil_module`; Apps Inspector, load `cliff_veil_ui`; in the mini-app's Settings tab enter
the path of `cliff-veil` and its environment (`LEE_WALLET_HOME_DIR`, `LEE_WALLET_PASSWORD`, `CV_IDS`). The full steps,
the command-line arguments the module sends, and the headless `logosctl` route are in [miniapp/README.md](miniapp/README.md).
Creating a schedule, claiming and cancelling in the mini-app are the same operations as the CLI commands above, each
reviewed with a dry run before it is sent.

Not verified: loading in Basecamp, and building from the GitHub URL (only local `path:` builds were run). The evidence
that exists is in [evidence/miniapp/](evidence/miniapp/README.md).

## Tests and CI

| What | Command | Notes |
|---|---|---|
| Format, clippy | `make lint` | clippy with warnings as errors on every crate and target |
| Unit and property tests | `make test` | runs with the guest build skipped; tests that execute the real guests (`program/tests`, `program/fees/tests`: events, batch caps, authorisation attacks, the fee program) report that they are skipped. To run them: `make test-executor`, which builds the guests and fails if they are missing (`CV_REQUIRE_GUESTS=1`) |
| End-to-end tests | `scripts/seq.sh dev start && make deploy-local && make e2e` | `e2e/run.sh` runs the `vesting_e2e` suite against the running sequencer (`CV_SEQ_RPC` moves it); every test opens its own funded accounts |
| Requirement coverage | `cargo run -p reqmap -- --check` | fails when a hard requirement has no test or when [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) is out of date |
| Reproducible guests | `make guests-release` | builds every guest in the pinned image and compares the image ids with `program/IMAGE_IDS.expected` |
| Mini-app logic | `cargo test --manifest-path miniapp/rust-lib/logic/Cargo.toml` | |
| IDL | `cargo run -p vesting_idl -- --check` | fails when `idl/*.json` differs from a fresh generation |

`.github/workflows/ci.yml` has four jobs: `rust` (format, clippy, tests, mini-app logic, `reqmap --check --strict`),
`executor` (builds the guests and runs every executor test against them; a missing guest fails the job), `guests` (guest
build in the pinned image and the image-id check) and `e2e` (builds the standalone sequencer at the pinned LEZ commit,
deploys, runs `e2e/run.sh`).

## Requirement coverage and costs

[docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) maps every hard requirement of RFP-017 to the tests that exercise it:
27 of its 29 rows are covered by tests and 2 by named evidence. [docs/COSTS.md](docs/COSTS.md) holds the cost of each
operation, generated from `evidence/cost/*.json`. Executor cycles on LEZ v0.4.0-rc3, native schedules:

| Operation | Cycles |
|---|---|
| create | 92,929 |
| claim (reads the clock) | 99,583 |
| cancel | 96,644 |
| signal milestone | 49,137 |
| transfer beneficiary | 50,679 |
| make non-cancelable | 48,182 |

The chain's execution cap is 10,000,000 cycles per transaction and per block. The fees a standalone sequencer charged for
the same operations are in the same file (for example 759,544 for a create and 795,888 for a claim). These are cycles and
fees measured on a standalone sequencer, not on testnet 0.4.

## Privacy model

Public: every schedule parameter (asset, total, curve, times or milestone amounts, flags, authorities, creator,
beneficiary, refund account), the amount claimed so far, the time of the last claim, cancellation and milestone state,
the escrow balance, and every event. For each claim that is its amount, its time, the schedule and the beneficiary.

Not recorded in public on-chain state when the claim goes to a private account: the destination private account and the
later movements of the claimed tokens.

Limitation: the beneficiary address is permanently and publicly associated with the schedule from the moment it is
created, and every claim is linked to that address. The privacy property covers post-claim token movements only.
Keeping the beneficiary itself off the chain would need commitment-based beneficiary registration, which depends on
LP-0003 and is outside this RFP and this version.

A private claim names a one-time blinded address, not the destination's canonical id, because LEZ publishes the root
call of a privacy-preserving transaction. [docs/PRIVACY.md](docs/PRIVACY.md) explains the mechanism, the scan that checks
a real transaction for the canonical id (it found none; it found the one-time address), and what the scan does not show.

## Fees

| Question | Answer |
|---|---|
| Who pays | The schedule creator, from the funding account, in the schedule's asset. Beneficiaries pay no protocol fee on claims. |
| When | At creation only (`create`, `create-batch`). Claim, cancel, make non-cancelable, signal and transfer carry no protocol fee; they pay the network's own transaction fee. |
| Rate | `fee = floor(total * rate_bps / 10,000)` per creation message (a batch: on the sum of its totals). The rate is 0 after deployment. Hard cap 100 basis points (1%), a constant in `core/` that no message can raise; it is enforced by `vesting` itself and again by `vesting_fees`. |
| Fee switch | The `vesting_fees` program holds the rate and the treasury. Its admin is managed with the RFP-001 admin-authority library (`lez-admin-authority` v0.2.0: single admin, two-step handover, renounceable). The deployer sends `Init`; the admin sets the rate with `SetFee`. Both programs are deployed immutable. |
| Where | The treasury named in the creation message and checked by `vesting_fees`: its native balance for a native fee, its associated token account for a token fee. |
| Broker fee | None. |

Changing the rate is done with the admin client:

```bash
cargo run -p vesting_fees_admin -- --program <vesting_fees id> --payer <account> show
cargo run -p vesting_fees_admin -- --program <vesting_fees id> --payer <account> set-fee --admin <admin account> --rate-bps 50 --treasury <treasury account>
```

The creation message carries the fee and treasury that the SDK quoted, and `vesting_fees` refuses a mismatch, so a rate
change between quote and execution makes the creation fail cleanly (error 27) instead of charging an unexpected amount.
A creation with a zero fee passes whatever the state of the fee configuration. Details: [SPEC.md](SPEC.md) section 6.

## Repository layout

| Path | What |
|---|---|
| `SPEC.md` | The interface specification (version 1): programs and accounts, messages, state, arithmetic, error codes, fees, events, privacy, reading state. |
| `core/` | `vesting_core`: the vesting rules, state, messages, events and error table. Shared by the programs and the SDK; property-tested. |
| `program/` | `vesting_methods` (embeds the guest ELFs) and `program/guest`, the `vesting` program. `program/fees/` is the `vesting_fees` program, its guest and its admin client. `program/token-guest/` builds the token program from lez-programs. `program/ata-guest/` is the upstream associated-token-account program, vendored. |
| `sdk/` | `cliff_veil_sdk`: the whole lifecycle, public and private claims, batch splitting, block scan, in-memory backend for tests. |
| `cli/` | The `cliff-veil` binary. |
| `idl/` | `idl/vesting.json` and `idl/vesting_fees.json` in SPEL's IDL format, and the generator that keeps them equal to `core/`. See [docs/IDL.md](docs/IDL.md). |
| `miniapp/` | The Logos mini-app: core module, QML view, and its logic crate. |
| `e2e/` | End-to-end tests against a standalone sequencer, and `e2e/run.sh`. |
| `tools/` | `cost` (writes docs/COSTS.md), `reqmap` (writes docs/REQUIREMENTS.md), `image_id` (image id of a guest binary), `scan_rpc` (privacy scan of RPC answers). |
| `scripts/` | Bootstrap, local sequencers, deployment, reproducible guest build and image-id check. |
| `deploy/` | Program ids and image ids per network. |
| `evidence/` | Logs and measurement files that the documents quote. |
| `docs/` | [REQUIREMENTS](docs/REQUIREMENTS.md), [COSTS](docs/COSTS.md), [IDL](docs/IDL.md), [PLATFORM-FINDINGS](docs/PLATFORM-FINDINGS.md), [DECISIONS](docs/DECISIONS.md), [PRIVACY](docs/PRIVACY.md), [EVENTS](docs/EVENTS.md), [REUSE](docs/REUSE.md). |

## Licence

Our code is licensed under either of the [MIT licence](LICENSE-MIT) or the [Apache licence, version 2.0](LICENSE-APACHE),
at your option. `program/ata-guest/` is the associated-token-account program of Logos Blockchain's lez-programs,
vendored with unchanged sources under its own MIT licence (`program/ata-guest/LICENSE`). Other upstream code we depend
on, and its licences, are listed in [docs/REUSE.md](docs/REUSE.md).
