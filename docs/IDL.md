# Interface description (IDL)

The programs' interfaces are described in SPEL's IDL format, version 2 (the format of SPEL's actor-model branch for
LEZ v0.4):

| File | Program | Generated from |
|---|---|---|
| `idl/vesting.json` | `vesting` | `vesting_core::Message`, `ScheduleState`, the events of `core/src/events.rs`, `Internal`, `ERRORS` |
| `idl/vesting_fees.json` | `vesting_fees` | `vesting_core::FeeMessage` (which `vesting_fees` encodes byte for byte), `FeeChanged`, the config state |

## SPEL version

SPEL's released versions (up to v0.7.0) target LEZ v0.2.x. LEZ v0.4 support is on SPEL's `refactor/lez-v0.4.x`
branch. We pin the commit that `logos-blockchain/lez-programs` (branch `refactor/lez-v0.4.x`) pins for its own IDLs:

- repository: `https://github.com/logos-co/spel`
- commit: `d7444f84cabee3371aca3bdee8ebc4c2fc45ebde` (LEZ `v0.4.0-rc3`)
- crate: `spel-framework-core`, features `idl-gen` (the generator behind `spel generate-idl`) and `decode` (the
  IDL-driven Borsh decoder behind `spel inspect`)

The pin is in `idl/Cargo.toml`; `Cargo.lock` records it.

## How the files are produced

`idl/` (crate `vesting_idl`) runs SPEL's generator, `spel_framework_core::idl_gen::generate_idl`, on `core/`, the
crate both programs and the SDK share. The generator reads the Rust source; the program needs no annotations.

`idl/vesting.json` holds exactly what
`spel generate-idl core --message Message --state ScheduleState --events events` writes, plus:

- **Events.** `core/src/events.rs` declares each event impl through a small macro, `event!(Name, selector)`, which
  a source reader does not expand. The generator reads the event names from those invocations; each selector is
  SPEL's `event_selector("vesting", name)` (the first 8 bytes of `SHA-256("vesting::<Name>")`, SPEC 7) and each
  payload type is described by SPEL's generator. If the impls are ever written out without the macro, SPEL's own
  events path is used and the output is the same.
- **`internal_messages`**: the `Internal` enum (SPEC 3.3), generated the same way. Only `vesting`'s own actors send
  these; a client never does. `spel` ignores this key.
- **`errors`**: the code table of SPEC 5 (`vesting_core::ERRORS`), as `{ code, name, msg }` rows (the shape SPEL's
  version 1 used). `spel` ignores this key.

`idl/vesting_fees.json` is generated from `vesting_core::FeeMessage` with the program name `vesting_fees`. Two
points differ from a plain generation:

- **State.** The config state, `FeeConfig { rate_bps, treasury, admin }`, holds the admin slot of
  `lez-admin-authority` v0.2.0, a git dependency the generator does not read. `FeeConfig`, `AuthoritySlot` and
  `Holder` are therefore written out in `idl/src/lib.rs`, and a test decodes real configs encoded by
  `vesting_fees::FeeConfig` with them (slot unset, held with a nominee, renounced).
- **Event selector.** `FeeChanged` is emitted under the `vesting::` namespace (SPEC 7), so its selector is
  `SHA-256("vesting::FeeChanged")[..8]`, not `vesting_fees::FeeChanged`.

The admin's own operations (`Init`, `Nominate`, `Accept`, `Renounce`) are `lez-admin-authority`'s messages behind
its tag byte 255; they are sent with that library's `admin_op` and are not part of this IDL.

## Regenerate and check

```bash
source scripts/env.sh
cargo run -p vesting_idl              # writes idl/vesting.json and idl/vesting_fees.json
cargo run -p vesting_idl -- --check   # fails if a checked-in file differs from a fresh generation
cargo test -p vesting_idl             # the u6_idl_* tests below
```

## Tests (`idl/tests/u6_idl.rs`)

Each test encodes values with `vesting_core` (the encoding the program and the SDK use), decodes them with SPEL's
decoder through the checked-in IDL, and compares the result with the values it was built from. Every field holds a
distinct value, so a missing, extra, retyped or reordered field fails.

| Test | Checks |
|---|---|
| `u6_idl_files_are_generated_from_core_and_up_to_date` | both files equal a fresh generation (so CI fails on drift) |
| `u6_idl_loads_in_spel_and_names_the_program` | `SpelIdl::from_json` accepts both; program names, state types, the SPEC 10 type list |
| `u6_idl_matches_program_messages` | every `Message` variant, its tag and every field |
| `u6_idl_matches_program_internal_messages` | every `Internal` variant, its tag and every field |
| `u6_idl_matches_program_state` | a `ScheduleState` with every optional part set |
| `u6_idl_matches_program_events` | every event of SPEC 7: name, selector equal to the program's constant, payload |
| `u6_idl_matches_program_errors` | the error table equals `vesting_core::ERRORS`, codes 1 to 29 |
| `u6_fees_idl_matches_program` | `CheckFee` and `SetFee` as the program puts them on the wire, the config state, `FeeChanged` |

The examples name each variant through an exhaustive `match`, so a new message variant does not compile until it
has an example here.

## Using the IDL with `spel`

The files are meant for `spel` built from the pinned commit (`cargo install --git https://github.com/logos-co/spel
--rev d7444f8 spel-cli`). Our tests load them with the same `spel-framework-core` code the CLI uses
(`SpelIdl::from_json`, the decoder of `spel inspect`); the CLI itself is not run in our tests. Typical use:

```bash
spel --idl idl/vesting.json                                   # lists the messages and their flags
spel --idl idl/vesting.json encode claim --schedule <ACCOUNT> --to <ACCOUNT>
spel --idl idl/vesting.json --program <VESTING_ADDRESS> inspect <SCHEDULE_ACCOUNT>
```

Every user message goes to the caller's own actor `(caller, vesting)` (SPEC 1.3), and the actors a transaction
must declare are listed in SPEC 3.2; the SDK and CLI build these transactions.
