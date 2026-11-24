# ata-guest: the LEZ associated token account program, vendored

`vesting` pays a non-zero token creation fee into the treasury's associated token account (ATA) for the token
definition (SPEC section 6): `AccountId::for_public_pda(ATA_PROGRAM_ID, SHA-256(token_program ‖ owner ‖ definition))`.
That account must exist, and only the ATA program can create it (its `Create` is permissionless and idempotent).

The ATA program at the lez-programs commit our token program is pinned to (3f04cc37) is still written for LEZ
v0.2.5. Upstream has ported it to the LEZ 0.4 actor model on branch `refactor/ata-lez-v0.4.x`
(commit 064b73fb28320aa5c14e6da12410acaed0664048), against a LEZ commit later than v0.4.0-rc3. This directory is
that port with the manifests pointed at LEZ v0.4.0-rc3 and at token_core / nft_core 3f04cc37:

| | Upstream 064b73fb | Here |
|---|---|---|
| `src/`, `core/src/`, `guest/src/` | | byte-identical |
| `lee_core` | rev 616256b9 | tag v0.4.0-rc3 |
| `token_core`, `nft_core` | path dependencies on the same branch | lez-programs rev 3f04cc37 |
| workspace | lez-programs | its own (this `Cargo.toml`), outside the cliff-veil workspace |

The token messages it sends (`InitializeHolding`, `ReadState`, `Transfer`, `Burn`) have the same variants and order
in token_core at 3f04cc37, and that token program accepts `InitializeHolding` from another program's PDA.

```bash
cargo test --manifest-path program/ata-guest/Cargo.toml        # the upstream unit tests
scripts/guests_release.sh                                       # builds guest/ as `ata` with the other guests
```

When lez-programs publishes the port on the token program's branch, this directory is replaced by a pinned
dependency. Licence: MIT, `LICENSE` (Logos Blockchain).
