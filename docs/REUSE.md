# What is reused

cliff-veil is our code on top of LEZ, the LEZ token and associated-token-account programs, and the RFP-001
admin-authority library. This file lists each upstream piece, the exact version, how we use it, what we changed, and its
licence. Everything is pinned by tag or commit; `Cargo.lock` records the commit behind every git dependency.

Our own code is `MIT OR Apache-2.0` ([LICENSE-MIT](../LICENSE-MIT), [LICENSE-APACHE](../LICENSE-APACHE)); every crate
of the workspace says so in its manifest, except `program/ata-guest`, which keeps its upstream MIT licence.

## Upstream code that is part of what we deploy or build

| Piece | Source and version | How we use it | Changes | Licence |
|---|---|---|---|---|
| LEZ crates: `lee_core`, `lee`, `wallet`, `common`, `sequencer_service_rpc`, `program_loader_core` | `logos-blockchain/logos-execution-zone`, tag `v0.4.0-rc3`, commit `bafd07be0701398f9bc435f73ab05e9a7936c301` (`scripts/pins.txt`) | Program interface (`lee_core`), the executor the tests run (`lee`), the wallet library under the SDK and CLI, RPC types, program deployment | None. Git dependencies. | MIT (`LICENSE` of that repository); its workspace manifest states "MIT or Apache-2.0" |
| LEZ sequencer and wallet binaries | Same checkout, built by `scripts/bootstrap.sh` (`sequencer_service` with `--features standalone`, `wallet`) | The standalone sequencer the end-to-end tests run against; the wallet used for accounts and program deployment | None. Built, not vendored. | As above |
| LEZ built-in programs | Part of the same release | The clock program (read by `vesting`) and the native token program (native escrow) | None. | As above |
| Token program | `logos-blockchain/lez-programs`, branch `refactor/lez-v0.4.x`, commit `3f04cc37ec5f90358e606b0fe7d28baa6b1dbd7e` | Escrow and payout of token schedules. `program/token-guest` builds the `token_program` crate of that commit into a guest binary, as lez-programs' own guest does; `token_core` types are used by the executor tests | None. `vesting` embeds the id of the deployed token program as a build input. | MIT (Logos Blockchain) |
| Associated token account program | `logos-blockchain/lez-programs`, branch `refactor/ata-lez-v0.4.x`, commit `064b73fb28320aa5c14e6da12410acaed0664048`, vendored in `program/ata-guest/` | Locates the treasury's holding for a token creation fee (`compute_ata_seed`) and creates it (`Create` is permissionless and idempotent) | Sources (`src/`, `core/src/`, `guest/src/`) are byte-identical to upstream. Only the manifests differ: `lee_core` at tag `v0.4.0-rc3` instead of rev `616256b9`, and `token_core` / `nft_core` at lez-programs `3f04cc37` (the token program we deploy); its own workspace outside ours. See `program/ata-guest/README.md`. To be replaced by a pinned dependency when lez-programs publishes this port on the token program's branch. | MIT, `program/ata-guest/LICENSE` (Logos Blockchain) |
| Admin authority library (RFP-001) | `mmlado/lez-admin-authority`, tag `v0.2.0`, commit `e98471067f414a2f763f44450f5ae47fb285444b`, with its dependency `mmlado/lez-authority`, tag `v0.2.0`, commit `145e09ca2f9739aaa09be5b08a9288175e2e813c` | The admin of `vesting_fees`: single admin, two-step handover, renounce; the typed entrypoint and the guard in front of the handler | None. Git dependency pinned to LEZ v0.4.0-rc3. | `MIT OR Apache-2.0` |
| SPEL framework | `logos-co/spel`, commit `d7444f84cabee3371aca3bdee8ebc4c2fc45ebde` (the commit lez-programs pins for its own IDLs; SPEL's released versions up to v0.7.0 target LEZ v0.2.x) | `spel-framework-core` with features `idl-gen` and `decode`: generates `idl/*.json` from `core/` and decodes with it in the IDL tests. Used by `idl/` only; nothing from it is in a deployed guest. | None. | MIT and Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE-v2` in that repository) |
| RISC Zero | `risc0-zkvm` and `risc0-build` `=3.0.5`, `risc0-binfmt` `=3.0.4`, `risc0-zkos-v1compat` `=2.2.2` (the versions LEZ v0.4.0-rc3 locks); guest builder image `risczero/risc0-guest-builder:r0.1.91.1`, pinned by digest in `scripts/guests_release.sh` | zkVM guests, the host executor, the reproducible guest build, the image id of a guest binary (`tools/image_id`) | None. | Apache-2.0 |
| `keccak` patch | `logos-blockchain/sponges`, rev `3a56e99771beedf04946eab21a4a62adc2951377` | The same `[patch.crates-io]` as LEZ v0.4.0-rc3, so the LEZ crates resolve as they do there | None. | As the upstream repository states |

The rest of `Cargo.lock` is crates.io dependencies (`borsh`, `clap`, `serde`, `sha2`, `tokio`, ...) and the git
dependencies LEZ itself brings in (for example `logos-blockchain`, `Overwatch`, `jellyfish`, `spongefish`); none is
copied into this repository, and each carries the licence its crate declares.

## Tools used to build the mini-app

The mini-app is built with Nix from the Logos module builder at a pinned commit (`logos-co/logos-module-builder`,
`a57dc7fd1fe92d3ac7b545c5559b9c581576f2c9`; `miniapp/flake.lock` and `miniapp/ui/flake.lock` pin the rest, including
`nix-bundle-lgx`) and is installed and exercised with Basecamp 0.3.2 or later and `logosctl` 0.3.2. They are build and
runtime tools, not part of this repository.

## What is ours

Everything else: the vesting rules (`core/`), the `vesting` and `vesting_fees` programs, the SDK, the command-line
client, the IDL generator and its tests, the cost, requirement-map and scan tools, the end-to-end suite, the mini-app
sources, the scripts and the documents. The design borrows one pattern, not code: the routing through the caller's own actor
that `lez-admin-authority` and the token program use to authenticate a caller (see
[PLATFORM-FINDINGS.md](PLATFORM-FINDINGS.md), finding 1).
