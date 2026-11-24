# SDK: Rust API and C ABI

The SDK covers the whole lifecycle of a vesting schedule (RFP Usability 1): create (one, or a batch split into
transactions that fit), query (vested, claimable now, locked, next unlock), claim to a public account or to a private
account, cancel, make non-cancelable, signal a milestone, transfer the beneficiary, and read the fee configuration.

It comes in two forms over the same code:

| Form | Crate | For |
|---|---|---|
| Rust API | `sdk/` (package `vesting_sdk`, library `cliff_veil_sdk`) | Rust programs and Rust Logos modules; the `cliff-veil` command line is built on it |
| C ABI | `sdk/ffi/` (package `vesting_sdk_ffi`, library `libcliff_veil.so` / `libcliff_veil.a`, header `sdk/ffi/include/cliff_veil.h`) | Logos core modules in C++ (Qt), and any host that can call C |

Both run every check before anything is signed: each operation is **prepared** (current state is read, the program's
own rules from `vesting_core` run on a copy, the transaction is built and dry-run on the chain to price it, and the
payer's balance is checked), then **sent**. A refusal comes back before signing, with the program's stable error code
(SPEC 5), a message and an action.

| Operation | Rust (`Client`) | C ABI | Command line |
|---|---|---|---|
| Create one schedule | `prepare_create` | `cv_prepare_create` | `create` |
| Create a batch | `prepare_create_batch` | `cv_prepare_create_batch` | `create-batch` |
| Query a schedule | `status`, `claimable` | `cv_show`, `cv_claimable` | `show`, `claimable` |
| List by creator or beneficiary | `list` | `cv_list` | `list` |
| Claim (public or private) | `prepare_claim` | `cv_prepare_claim` | `claim [--private]` |
| Cancel | `prepare_cancel` | `cv_prepare_cancel` | `cancel` |
| Make non-cancelable | `prepare_make_non_cancelable` | `cv_prepare_make_non_cancelable` | `make-non-cancelable` |
| Signal a milestone | `prepare_signal` | `cv_prepare_signal` | `signal` |
| Transfer the beneficiary | `prepare_transfer` | `cv_prepare_transfer` | `transfer` |
| Fee configuration | `fee_config`, `quote_fee` | `cv_fee_show` | `fee-show` |
| Schedule address | `Programs::schedule_account` | `cv_address` | `address` |
| Send a prepared transaction | `send` | `cv_send_prepared` | (after the summary) |
| Privacy disclosure | `privacy_disclosure()` | `cv_disclosure` | `disclosure` |

## Rust API

```toml
[dependencies]
vesting_sdk = { git = "https://github.com/AaroneGeorge/cliff-veil" }   # library name: cliff_veil_sdk
```

A [`Client`](../sdk/src/client.rs) runs over a `Backend`: `SequencerBackend` (LEZ's wallet against a sequencer's RPC)
or `MemoryBackend` (an in-memory chain that runs the same `vesting_core` rules, for tests and offline work). The
example below is the unit test `doc_example_runs` (`sdk/src/tests.rs`):

```rust
use cliff_veil_sdk::{Client, ClaimTarget, MemoryBackend, Programs, linear_terms};
use lee_core::account::AccountId;
use vesting_core::Asset;

let (creator, alice) = (AccountId::new([1; 32]), AccountId::new([2; 32]));
let mut chain = MemoryBackend::new(Programs::new(AccountId::new([9; 32])), 0);
chain.add_public(creator, 10_000);
chain.add_public(alice, 100);
let mut client = Client::new(chain);

// 1 000 units over 1 000 ms, 250 ms cliff.
let create = client.prepare_create(creator, [7; 32], linear_terms(alice, Asset::Native, 1_000, 0, 250, 1_000)).await?;
client.send(&create).await?;
let schedule = client.programs().schedule_account(creator, &[7; 32]);

client.backend_mut().now = 500;
assert_eq!(client.claimable(schedule).await?, 500);
let claim = client.prepare_claim(alice, schedule, ClaimTarget::Public(alice)).await?;
client.send(&claim).await?;
```

Against a sequencer:

```rust
let programs = Programs::from_ids_file(std::path::Path::new("deploy/ids-local-dev.toml"))?;
let mut client = Client::new(SequencerBackend::from_env(programs)?);   // LEE_WALLET_HOME_DIR, LEE_WALLET_PASSWORD
```

A claim to a private account is `ClaimTarget::Private { account, fee_payer }` (`account: None` creates a fresh
private account of the wallet). Its `Prepared` carries the pre-claim summary (`Outcome::Claim`); the caller shows
that summary and `privacy_disclosure()` and asks for confirmation before `send`. With feature `json`,
`cliff_veil_sdk::json` renders every answer in the shapes below.

## C ABI

### Build

```bash
cargo build -p vesting_sdk_ffi                  # debug: $CARGO_TARGET_DIR/debug/libcliff_veil.{so,a}
cargo build -p vesting_sdk_ffi --profile host   # optimised: $CARGO_TARGET_DIR/host/libcliff_veil.{so,a}
```

The header is `sdk/ffi/include/cliff_veil.h` (written by hand; the test `u1_c_abi_header_declares_every_exported_function`
fails when a function of `sdk/ffi/src/abi.rs` is missing from it, or the header names one the library does not export).
`make c-example` builds the library, compiles `sdk/ffi/examples/c/claim.c` with `-std=c11 -Wall -Wextra -Werror
-pedantic`, checks that the header compiles and links as C++17, and runs the example against the in-memory chain. Its
output is in `evidence/sdk-ffi/c-example.log`.

### Conventions

- **JSON in, JSON out.** Every function that returns `char *` answers one JSON object in the shapes `cliff-veil
  --json` prints; both come from `cliff_veil_sdk::json`, so a module, the command line and the mini-app read the
  same fields. Success is `{"ok": true, ...}`. Failure is
  `{"ok": false, "error": {"code": N | null, "name": "...", "message": "...", "action": "..." | null}}`; an account
  that cannot pay adds `payer`, `need`, `have` and `short`.
- **Values.** Amounts are decimal strings in answers (they are 128-bit) and a number or a decimal string in
  requests. Times are milliseconds since the Unix epoch (number or decimal string), or `"now"` for the chain clock;
  answers also give them as ISO-8601 UTC. Accounts are base58 ids or labels: wallet labels (and `Public/<id>`,
  `Private/<id>`) on a sequencer, the configured labels on the in-memory chain.
- **Memory.** Every returned `char *` belongs to the caller: free it with `cv_string_free`. Input strings are
  borrowed for the call only.
- **Threads.** A client may be used from any thread; calls on one client are serialised by a lock inside it, and
  separate clients are independent (`u1_c_abi_one_handle_serves_many_threads` calls one client from 8 threads). A
  call blocks until the SDK answers; a send waits for inclusion. In a Qt module, call from a worker thread, not the
  UI thread. Do not call from a thread that runs an async (tokio) runtime. Close a client only after every call on it
  has returned.
- **Panics** never cross the boundary: every function catches them and answers `{"ok": false, "error": {"name":
  "Panic", ...}}`; the client it happened on then answers `ClientPoisoned` until it is closed.
- **Version.** `cv_abi_version()` equals `CV_ABI_VERSION` of the matching header (1).

### Opening a client

`char *cv_client_open(const char *config_json, CvClient **out_client)`; close with `cv_client_close`.

| Backend | Configuration |
|---|---|
| Sequencer | `{"backend": "sequencer", "ids_file": "<path of deploy/ids-*.toml>"}` or `{"backend": "sequencer", "vesting": "<id>", "vesting_fees"?: "<id>", "token_program"?: "<id>", "ata_program"?: "<id>"}`; plus `"wallet_home"?: "<dir>"`, `"wallet_password"?: "..."` (defaults: `LEE_WALLET_HOME_DIR`, else `~/.lee/wallet`, and `LEE_WALLET_PASSWORD`, else none, as LEZ's `wallet` finds them; the library never prompts on a terminal: a wrong or missing password is an error), `"fee_headroom_blocks"?: n` |
| In memory | `{"backend": "memory", "now_ms"?: n, "public"?: [{"label": "creator", "balance": "1000000"}], "private"?: [{"label": "vault", "balance": "0"}], "fee_rate_bps"?: n, "treasury"?: "<label>"}`; an entry may give `"account": "<id>"`, else its id is derived from the label. The answer lists `accounts` (label to id). The clock moves only with `cv_memory_set_clock`. |

The answer also gives `backend`, `programs` (the program ids in use) and `dev_mode` (`true` when `RISC0_DEV_MODE` is
set in the process: private claims then carry no real proof, and every prepared private claim says so).

### Requests

| Function | Request | Answer (besides `"ok": true`) |
|---|---|---|
| `cv_show(client, schedule)` | a schedule account | `schedule`: terms and position (`total`, `vested`, `claimed`, `claimable_now`, `locked`, `next_unlock`, `next_unlock_kind`, `next_unlock_ms`, `kind_detail`, `standing`, authorities, `clock_ms`, ...) |
| `cv_claimable(client, schedule)` | a schedule account | `claimable`: `schedule`, `claimable_now`, `next_unlock`, `clock` |
| `cv_list` | `{"creator" \| "beneficiary", "from_block"?, "active"?}` | `schedules` (as `cv_show`), `scanned_to_block`; the block scan is cached in the client |
| `cv_fee_show(client)` | none | `fees`: `deployed`, `program`, `config_account`, `rate_bps`, `max_fee_bps`, `treasury` |
| `cv_address` | `{"creator", "label" \| "id"}` | `schedule` |
| `cv_prepare_create` | `{"from", "beneficiary", "total", "start"?, "cliff"?, "end"}` (linear; `start` defaults to now, `cliff` to `start`) or `{"from", "beneficiary", "milestones": [...]}`; `"token"?`, `"cancelable"?` (true), `"transferable"?` (false), `"cancel_authority"?`, `"milestone_authority"?`, `"refund_to"?`, `"label"?` or `"id"?` (64 hex; default random) | `transactions`: one entry with `schedules`, `total`, `creation_fee`, `treasury`, `schedule_id` |
| `cv_prepare_create_batch` | `{"from", "items": [{"beneficiary", "total", "label"?}], "start"?, "cliff"?, "end"}` or `{..., "milestone_bps": [...]}`, the terms above, `"per_tx"?` | `transactions`: one entry per transaction that fits the execution budget |
| `cv_prepare_claim` | public: `{"schedule", "as", "to"?}`; private: `{"schedule", "as", "private": true, "private_to"?, "fee_payer"?}` | `transactions`: the pre-claim summary (`amount`, `at`, `claimed_after`, `still_to_come`, `destination`, `fee_payer`, `fee_payer_holds`, `fee_payer_choice`, `fee_payer_reason`, `reserve`, `headroom_*`); private: `privacy_disclosure`, `dev_mode`, `disclosure_ack` |
| `cv_prepare_cancel` | `{"schedule", "as"}` | `at`, `vested_at_cancel`, `refunded`, `refund_to`, `still_claimable` |
| `cv_prepare_make_non_cancelable` | `{"schedule", "as"}` | (the estimate only) |
| `cv_prepare_signal` | `{"schedule", "as", "index"}` | `index`, `unlocks` |
| `cv_prepare_transfer` | `{"schedule", "as", "to"}` | `from`, `to` |
| `cv_send_prepared(client, id, disclosure_ack)` | a prepared id; `disclosure_ack` or NULL | `prepared`, `sent`: `[{"hash", "block", "declared_fee"}]`; a private claim adds `private_balance_after` |
| `cv_discard_prepared(client, id)` | a prepared id | `prepared`, `discarded` |
| `cv_memory_set_clock(client, ms)` | in-memory chains only | `clock_ms`, `clock` |
| `cv_disclosure()` | none (no client) | `privacy_disclosure`, `dev_mode` |

Every `transactions` entry also carries `operation`, `prepared` (its id) and `estimate` (`network_fee`, `reserve`,
`payer`, `private`, `cycles`, `headroom_blocks`, `headroom_reserve`).

### The disclosure gate of a private claim

A prepared claim to a private account answers the privacy disclosure (`privacy_disclosure`, the same text the command
line and the mini-app show, `sdk/src/disclosure.rs`) and a token, `disclosure_ack`. The library sends that claim only
when `cv_send_prepared` is given exactly that token; otherwise it answers `DisclosureNotAcknowledged`, sends nothing
and keeps the claim prepared. The token is a SHA-256 over the prepared id, the claim's one-time address and the
disclosure text, so the acknowledgement of one claim does not send another (`u1_c_abi_prepared_transactions_are_used_once`).
A module shows the disclosure, waits for the person to accept it, then passes the token: the gate stays in the
library even if a module's own check is missing. The token proves the call path carried the disclosure; it cannot
prove a person read it, which is the module's part.

A prepared transaction is used once: after a send (included, refused or dropped) its id is gone and the operation is
prepared again. A client keeps the 64 most recent prepared transactions.

### Errors of the ABI itself

Besides the SDK's errors (program refusals with their `code`, `NotPrivateAccount`, `PrivateFeeShortfall`,
`InsufficientBalance`, `NoHolding`, `Chain`, `Reverted`, `Dropped`, ...), the ABI answers these, with `code` null:

| `name` | When |
|---|---|
| `Input` | The request is not a JSON object, a field is missing or of the wrong type, an argument is null or not UTF-8, or an account is neither a label nor an id |
| `DisclosureNotAcknowledged` | A claim to a private account was sent without its own `disclosure_ack` |
| `UnknownPrepared` | No prepared transaction with this id (sent, discarded or dropped as old) |
| `WrongBackend` | `cv_memory_set_clock` on a sequencer client |
| `Backend` | The wallet does not open, or the runtime does not start |
| `Panic`, `ClientPoisoned` | A failure inside the SDK (see Conventions) |

### Example

`sdk/ffi/examples/c/claim.c`, shortened (error handling and JSON reading left out):

```c
#include "cliff_veil.h"

CvClient *client = NULL;
char *r = cv_client_open("{\"backend\":\"sequencer\",\"ids_file\":\"deploy/ids-local-dev.toml\"}", &client);
cv_string_free(r);

r = cv_prepare_claim(client, "{\"schedule\":\"<schedule>\",\"as\":\"<beneficiary>\",\"private\":true}");
/* show "transactions"[0] (the summary) and its "privacy_disclosure"; read "prepared" and "disclosure_ack" */
cv_string_free(r);

/* once the person accepts the disclosure: */
r = cv_send_prepared(client, prepared, disclosure_ack);
cv_string_free(r);
cv_client_close(client);
```

## Using it from a Logos module

A Logos core module is a Qt plugin built by `logos-module-builder`. It can reach the SDK in two ways:

1. **Link the C library** (the integration path this ABI is for). Build `libcliff_veil` with the repository's Rust
   toolchain (`rust-toolchain.toml`), put `cliff_veil.h` on the module's include path and link the library: the
   module's `metadata.json` has `nix.cmake.extra_include_dirs` and `nix.cmake.extra_link_libraries` for this. Open
   one client when the module starts, call the `cv_*` functions from a worker thread, read each answer with
   `QJsonDocument::fromJson`, free it with `cv_string_free`, and close the client when the module unloads. A sketch:

   ```cpp
   QString CliffVeilModule::show(const QString &schedule) {
       char *answer = cv_show(m_client, schedule.toUtf8().constData());
       const QString json = QString::fromUtf8(answer);
       cv_string_free(answer);
       return json;   // {"ok": true, "schedule": {...}} or {"ok": false, "error": {...}}
   }
   ```

2. **Run the command line** (`cliff-veil --json ...`), which prints the same JSON. This is what the mini-app's core
   module (`miniapp/`) does today, and why: the SDK links LEZ's wallet and the RISC Zero host crates, which need the
   repository's Rust toolchain (1.98.1, as LEZ v0.4.0-rc3 pins) and many git dependencies, while the module builder
   compiles a module's Rust crate with its own nixpkgs `rustc` (1.89) from vendored crates.io sources; and running the
   SDK in its own process keeps the wallet and the prover out of the Basecamp host process.

Status: the C ABI is tested in Rust (`sdk/ffi/src/tests.rs`, `u1_c_abi_*`) and from C (`make c-example`,
`evidence/sdk-ffi/c-example.log`) against the in-memory chain, and read-only against the dev-mode sequencer: it
opens LEZ's wallet, reads the fee configuration and prices a creation by a dry run without sending it
(`u1_c_abi_dry_runs_against_a_sequencer`, ignored by default; `evidence/sdk-ffi/sequencer-dry-run.log`). Not yet
shown: sends through the C ABI to a sequencer (the Rust API under it is tested there by
`e2e::u1_sdk_full_lifecycle_on_the_sequencer`), and a C++ module built by `logos-module-builder` that links
`libcliff_veil`.
