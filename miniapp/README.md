# cliff-veil mini-app (Logos Basecamp)

A Basecamp mini-app for cliff-veil vesting schedules on LEZ v0.4.0-rc3.

- **Recipient view**: every schedule paying a beneficiary, with totals per asset; per schedule the position (total,
  vested, claimed), total locked, amount claimable now, the next unlock event, the terms, and the claim action.
- **Claim**: public or private destination; a pre-claim summary with the claimable amount and the estimated network
  fee from an SDK dry run and, for a private claim, the fee reserve fixed in its proof (the most its fee payer is
  charged); a gas check (the fee payer holds the fee reserve) with an actionable error when it does not; for a claim to a private account, the SDK's privacy disclosure, which must be acknowledged
  before that claim is sent. Every claim gets its own summary, disclosure and acknowledgement.
- **Creator view**: create a schedule with every parameter (beneficiary, native or token asset, total, linear start,
  cliff and end or milestone amounts, cancelable, transferable, cancel authority, milestone authority, refund
  account, label for the schedule id); list the creator's active schedules; cancel one; signal a milestone. Each is
  reviewed (dry run, estimated fee) before it is sent.

## Layout

| Path | What |
|---|---|
| `rust-lib/logic/` | Crate `cliff_veil_miniapp`: view models, create form, claim flow, disclosure check, backend protocol, the module's JSON API. Plain Rust, no Qt, no Nix. |
| `rust-lib/` | Crate `cliff_veil_module`: the Logos glue (`src/glue.rs`) (trait `CliffVeilModule`); the module builder generates the C ABI and `.lidl` from it. |
| `flake.nix`, `metadata.json`, `CMakeLists.txt` | The core module `cliff_veil_module` (`type: core`, `interface: cdylib`). |
| `ui/` | The QML view `cliff_veil_ui` (`type: ui_qml`): `Main.qml`, `metadata.json`, `flake.nix`, icon. |

Where the rules live: the QML file lays out what the core module answers and disables buttons accordingly, but the
rules are enforced in `rust-lib/logic/` (`claim::ClaimFlow`): the module refuses to send a claim to a private account whose
disclosure was not acknowledged, whose disclosure does not name every item the RFP requires (claim amount,
beneficiary address, vesting schedule address as visible; destination private account and later movements as not
recorded), or whose fee payer cannot pay the fee.

## Tests

```bash
cargo test --manifest-path miniapp/rust-lib/logic/Cargo.toml
```

Test names map to the RFP rows (`tools/reqmap`): `u2_*` (recipient and creator views, create form), `u4_*`
(pre-claim summary, gas check), `u5_*` (disclosure gating: `claim::tests::u5_blocks_until_ack`), `pr2_*` (the
disclosure text). The `pr2_*` tests read the SDK's `PRIVACY_DISCLOSURE` from `sdk/src/disclosure.rs`, so they check
the text users see, not a copy.

## How the mini-app reaches the chain

The SDK links the LEZ wallet and the RISC Zero host crates, which do not belong inside a Qt plugin. The core module
runs the `cliff-veil` command line (the SDK's own front end, `cli/`) once per request and reads one JSON document
from its standard output. Core modules run outside the QML sandbox, in their own `logos_host` process.

A module can also link the SDK as a C library instead: `sdk/ffi` builds `libcliff_veil` with the header
`sdk/ffi/include/cliff_veil.h`, whose calls answer the same JSON as the command line (both print the shapes of
`cliff_veil_sdk::json`), and which keeps the disclosure gate of a private claim in the library itself
([docs/SDK.md](../docs/SDK.md)). This module stays on the command line: the SDK needs the repository's Rust toolchain
and LEZ's git dependencies, which the module builder's crate build does not provide, and a separate process keeps
the wallet and the prover out of the Basecamp host.

### What the module runs (`cliff-veil --json ...`)

Arguments are built in one place, `rust-lib/logic/src/backend.rs` (`argv`); the JSON the command line prints is read
in `rust-lib/logic/src/cli.rs`. Every operation runs twice: `--dry-run` for the review (checked against current state,
priced, nothing sent), then `--yes` after the person confirms. For a claim to a private account the module passes
`--yes` only once that claim's privacy disclosure was acknowledged in the mini-app.

| View action | Command |
|---|---|
| Show a schedule | `show <schedule>` |
| Review / send a claim | `claim <schedule> --as <beneficiary> [--to <account>]`, or `... --private [--private-to <p>] [--fee-payer <p>]`, with `--dry-run` / `--yes` |
| Review / send a creation | `create --from <creator> --beneficiary <b> [--token <def>] (--total <n> --start <ms> --cliff <ms> --end <ms> \| --milestones <a,b,...>) [--non-cancelable] [--transferable] [--cancel-authority <a>] [--milestone-authority <a>] [--refund-to <a>] (--label <s> \| --id <hex>)`, with `--dry-run` / `--yes` |
| Review / send a cancel | `cancel <schedule> --as <cancel authority>`, with `--dry-run` / `--yes` |
| Review / send a signal | `signal <schedule> --index <i> --as <milestone authority>`, with `--dry-run` / `--yes` |
| Settings check | `disclosure` |
| List by account | `list --creator <a>` / `list --beneficiary <a>` (block scan with a local cache); schedule accounts can also be typed in, one per line |

Two details keep the send equal to the review: a claim reviewed with a new private account is sent to that same
account (`--private-to`), and a creation without a label carries a schedule id the mini-app picks (`--id`), so the
dry run and the send name the same schedule account. Wallet and program ids come from the command line's
environment, set in the Settings tab: `LEE_WALLET_HOME_DIR`, `LEE_WALLET_PASSWORD`, and `CV_IDS` (ids file) or
`CV_VESTING` (program id).

## Build and load in Basecamp from git

Requirements: Nix with flakes (`experimental-features = nix-command flakes`), Linux x86-64, Basecamp 0.3.2 or later
(the released AppImage), and the Rust toolchain of `rust-toolchain.toml` for the command line. Nix evaluation of the
builder takes a few GiB of memory.

1. Build the two packages straight from the repository (portable variants, which released Basecamp builds install):

   ```bash
   nix build 'github:AaroneGeorge/cliff-veil?dir=miniapp#lgx-portable' -o cliff-veil-core
   nix build 'github:AaroneGeorge/cliff-veil?dir=miniapp/ui#lgx-portable' -o cliff-veil-ui
   ls cliff-veil-core/*.lgx cliff-veil-ui/*.lgx
   ```

   From a local checkout use `path:` so files not yet committed are included:
   `nix build path:./miniapp#lgx-portable` and `nix build path:./miniapp/ui#lgx-portable`.

2. Build the command line the core module runs:

   ```bash
   cargo build --profile host -p vesting_cli      # target/host/cliff-veil
   ```

3. In Basecamp: **Package Manager > Install Local Package**, select the core package (`cliff-veil-core/*.lgx`), click
   **Install**; then the same for the view (`cliff-veil-ui/*.lgx`). Core first: the view calls it.

4. **Settings > Module Inspector**: **Load** `cliff_veil_module`. **Settings > Apps Inspector**: **Load**
   `cliff_veil_ui`, then open it from the sidebar.

5. In the mini-app's **Settings** tab, enter the path of `target/host/cliff-veil` and its environment as JSON, for
   example `{"LEE_WALLET_HOME_DIR": "/path/to/wallet-home", "RISC0_DEV_MODE": "1"}`, and click **Apply**. (Or start
   Basecamp with `CLIFF_VEIL_CLI=/path/to/cliff-veil` in its environment.) Creating, cancelling, signalling and
   claiming run the program locally first, with RISC Zero's `r0vm`: when the host process does not find it (no rzup
   install in its `HOME`, or not on its `PATH`), add `"RISC0_SERVER_PATH": "/path/to/r0vm"`. Without it these steps
   fail with `Failed to execute program: No such file or directory`, while `show` and `list` still work.

### Without Basecamp: the core module under `logosctl`

`logosctl` 0.3.2 (the headless Logos Core runtime) installs, loads and calls the core module. It does not load the
QML view (a `ui_qml` module needs Basecamp's QML host).

```bash
S=/tmp/cliff-veil-session                       # any empty directory
logosctl --config-dir $S daemon start --detach  # with CLIFF_VEIL_CLI=/path/to/cliff-veil in the environment
logosctl --config-dir $S install cliff-veil-core/*.lgx -y
logosctl --config-dir $S module load cliff_veil_module
logosctl --config-dir $S call cliff_veil_module configure 'str:{"cli":"/path/to/cliff-veil","env":{"LEE_WALLET_HOME_DIR":"/path/to/wallet-home","CV_IDS":"/path/to/deploy/ids-local-dev.toml","RISC0_DEV_MODE":"1","RISC0_SERVER_PATH":"/path/to/r0vm"}}'
logosctl --config-dir $S call cliff_veil_module recipient_view <beneficiary> 'str:<schedule account>' 
logosctl --config-dir $S call cliff_veil_module prepare_claim 'str:{"schedule":"<s>","beneficiary":"<b>","path":"private"}'
logosctl --config-dir $S call cliff_veil_module acknowledge_disclosure <flow> str:<digest>
logosctl --config-dir $S call cliff_veil_module confirm_claim <flow>
```

To regenerate `rust-lib/Cargo.lock` after a dependency change, stage the Logos Rust SDK the path dependency points
at, then resolve:

```bash
nix build 'github:logos-co/logos-module-builder/a57dc7fd1fe92d3ac7b545c5559b9c581576f2c9#rust-sdk-src' -o miniapp/logos-rust-sdk-src
(cd miniapp/rust-lib && cargo generate-lockfile)
```

## Status

| Item | State |
|---|---|
| Logic and its tests (`u2_*`, `u4_*`, `u5_*`, `pr2_*`) | Done; `cargo test` passes |
| Nix build of both packages (`lgx-portable`, linux-amd64) | Builds from a local checkout (`path:`); log in `evidence/miniapp/build.log` |
| Core module installed, loaded and called under `logosctl` 0.3.2 | Done, over the real `cliff-veil` command line against the live local-dev sequencer (next row). The earlier run over a stand-in command line printing sample answers is kept in `evidence/miniapp/load-logosctl.log` |
| Against a live sequencer (headless) | Done on local-dev (LEZ v0.4.0-rc3, RISC0_DEV_MODE=1), every step through the core module under `logosctl` with a fresh funded wallet: `evidence/miniapp/live-local-dev/20261124T183901Z.log` (summary `.json`, 31 of 31 checks). Creator view: three creations (linear with cliff, linear, milestones), `list --creator` finding them, a cancel and a milestone signal, each reviewed with an estimated fee and then sent (transaction hashes, blocks 1966 to 1970). Recipient view: position, claimable, next unlock, totals; a public claim with its pre-claim summary and fee; a private claim whose fee payer is an empty private account refused with `private_fee_shortfall` and what to do; a private claim (dev mode) showing the disclosure, refused before the acknowledgement and with a wrong digest, sent after it, the destination's private balance going from 0 to 19,469,781; a second private claim (signalled milestone) to the same account, again needing its own acknowledgement (balance 302,881,749) |
| Found by the live run | A private claim's output starts with the wallet's sync lines; the module read that as bad output although the claim was sent (`evidence/miniapp/live-local-dev/20261124T183204Z.log`, 29 of 31). Fixed in `backend.rs` (`backend::tests::reads_a_private_claim_after_the_wallet_sync_lines`); the run above uses the fixed module |
| QML view rendered | Offscreen (Qt 6.9, software renderer): with sample answers (`evidence/miniapp/screenshots/miniapp-*.png`) and with the answers the core module gave in the live run, replayed (`evidence/miniapp/screenshots/live-*.png`). The view itself did not talk to the chain |
| Loaded in Basecamp | Done in the Basecamp 0.3.2 GUI (released AppImage, linux x86-64) against local-dev (LEZ v0.4.0-rc3, RISC0_DEV_MODE=1): both packages installed with **Package Manager > Install Local Package** (unsigned local packages) and reinstalled after changes, `cliff_veil_module` loaded in the Module Inspector, `cliff_veil_ui` loaded from the Apps Inspector, Settings applied and checked. Recipient view (block scan, totals, position, next unlock), public claims and private claims made from the view's screens: summary, gas check, the disclosure with **Confirm claim** disabled until it is acknowledged, then sent (transaction hashes and balances in `evidence/miniapp/basecamp/20261125T101712Z/`). Creator view: a linear schedule with a cliff created with every parameter, the active schedules listed, a schedule cancelled, a milestone signalled and then claimed; the private claim's fee payer charged exactly the fee reserve the summary showed (`evidence/miniapp/basecamp/20261125T104537Z/`, full run; `20261125T103042Z/` for the first creator-tab run) |
| Found in Basecamp | (1) The view bridge hands a module's string answer to QML JSON-encoded once more; the view ignored every answer until `Main.qml` (`call`) parsed it a second time. (2) The bridge refused `prepare_signal`, the module's only three-argument call ("Call refused"); the view now calls `prepare_signal_request` with one JSON string, as it does for create and claims, and the signal goes through in Basecamp. (3) A private claim's fee payer is charged the fee reserve fixed in the proof (16,588,032 against an estimated fee of 5,529,344 in the first Basecamp run); the summary now shows that reserve next to the estimated fee and the gas check requires it (`claim::tests::u4_private_summary_shows_the_fee_reserve_next_to_the_estimated_fee`, `u4_gas_check_needs_the_reserve_not_only_the_fee`) |
| Wired to the `cliff-veil` command line | Every argument list is accepted by the built binary (`backend::tests::u2_every_argument_list_is_accepted_by_the_command_line`, run with `CLIFF_VEIL_CLI=<binary> cargo test -- --ignored`); its JSON is read by `cli.rs` (unit-tested on the shapes `cli/src/output.rs` prints) |
| Built from the GitHub URL (`github:...?dir=miniapp`) | Not verified (local `path:` builds only) |
