# Privacy

What a cliff-veil claim to a private account does and does not put into public on-chain state, how that was checked on a
real transaction, and what the check does not show. The binding statement is [SPEC.md](../SPEC.md) section 9.

**Dev-mode caveat.** Every private claim in this repository was made with `RISC0_DEV_MODE=1`: the privacy-preserving
transaction is built and accepted but carries no real proof. The structure of the transaction, and therefore which
identifiers appear in it, is the same as with a proof; what dev mode does not show is a proof's cost or a verifier's
acceptance of it. A claim with a real proof has not been run, and nothing here measures its time or memory. The CLI says
so on every private claim (`dev mode: RISC0_DEV_MODE=1, ...`).

## What is visible, and what is not

| | Recorded in public on-chain state |
|---|---|
| Schedule parameters | Asset, total, curve and times (or milestone amounts), `cancelable`, `transferable`, creator, beneficiary, cancel authority, milestone authority, refund account |
| Schedule progress | `claimed`, `last_time`, cancellation, signalled milestones, the escrow balance |
| Every claim | Its amount, its time, the schedule, the beneficiary (as `Claimed` events and in the schedule state) |
| Creation, cancel, signal, transfer | The same, as events and state |
| A private claim's transaction | The root call from `(beneficiary, vesting)` with `ClaimAsOf { schedule, to, as_of }`, where `to` is a **one-time blinded address**; the public change to the schedule and its escrow |

| | Not recorded in public on-chain state, when the claim goes to a private account |
|---|---|
| The destination | The private account that receives the claim: its canonical id and its keys |
| Later movements | What the claimed tokens do afterwards |

No event contains a claim destination, and `to` is never stored in the schedule: the program learns only `to`, and never
whether it is a private account.

**Limitation.** The beneficiary address is permanently and publicly associated with the schedule from the moment it is
created, and every claim from the schedule is observably linked to that address. The privacy property covers the
post-claim movements of the tokens only: the destination private account and the activity after it are not recorded in
public on-chain state, but the beneficiary's association with the vesting position is. Hiding the beneficiary itself
would need commitment-based beneficiary registration, which depends on LP-0003 and is outside the RFP and this version.
The creator of a schedule, an observer and the program all see the same: a known beneficiary claimed this amount at this
time. None of them learns where it went.

## How a private claim hides the destination

LEZ publishes the public root call of a privacy-preserving transaction, and every delivery from the public part to a
private account. If the claim named the destination's canonical id, that id would be on chain next to the beneficiary.
So the claim names a one-time address instead:

1. The claimant's wallet picks 32 fresh random bytes, the opening, used for this claim only.
2. `to = destination.blinded(opening)` (`AccountId::blinded` in LEZ), where `destination` is the canonical id of the
   claimant's private account.
3. Inside the proof the private account opens that address (`PrivateWitness.address_openings`), so the credit lands on the
   account in the same transaction. Neither the canonical id nor the opening is in the transaction.
4. The opening is never reused: two claims to the same one-time address would be linkable.

The SDK takes the destination as an owned private account (or a fresh one it creates), never a bare id, derives `to`
itself, and refuses a public destination on the private path with an explicit error before anything is sent
(`pr3_private_claim_to_a_public_account_is_refused_before_sending`, and the CLI line in the log below). A foreign private
destination receives a pending credit at the one-time address and collects it later with the opening, which the claimant
hands over.

The network fee of the transaction is paid by a private account of the claimant: a native claim into a fresh private
account pays its own fee out of the claimed amount if the claim is larger than the fee, and a token claim needs a private
fee payer that holds native balance. No fee float is funded by the creator.

## The scan of a real private claim

[tools/scan_rpc/scan_rpc.py](../tools/scan_rpc/scan_rpc.py) fetches `getBlock` and `getTransaction` from the sequencer
exactly as a client sees them and counts an account id in every form it can take: base58 text, hex text, and, inside
every base64 string of the answer (the RPC carries blocks and transactions as base64 of their serialised bytes), as the
raw 32 bytes. The CLI lifecycle run applied it to its own private claim
([evidence/cli/local-dev-lifecycle.log](../evidence/cli/local-dev-lifecycle.log)):

| What | Value |
|---|---|
| Transaction hash | `8ea2818816b3eab753f2f194c00a54e529a2d30ba1fcabc682e07dba63dff345` |
| Block | 856 |
| Canonical private account (the destination) | `3we5si5b84rUK7BTLkoSkKBUwcFnDHR9GeNPXTgDnBNL` |
| One-time address named in the transaction | `GTuGrxh9sArtpnx9NUft6iZ2Kx1FA2gGke4ACRGpRPXB` |
| `getBlock`, canonical id: base58 / hex / raw bytes | 0 / 0 / 0 |
| `getTransaction`, canonical id: base58 / hex / raw bytes | 0 / 0 / 0 |
| `getBlock`, one-time address, raw bytes in decoded base64 | 4 |
| `getTransaction`, one-time address, raw bytes in decoded base64 | 4 |
| Total, canonical id / one-time address | 0 / 8 |

The one-time address is the control: the scan does find ids, and the one it finds is the public one. After the claim the
log's `show` of the schedule reports `claimed 500000000` and `escrow 500000000`, so the amount of the claim
(100,006,667) is readable from the public change of the schedule, as the disclosure says.

The end-to-end suite asserts related properties of state (`f2_claim_private_to_a_blinded_destination`, and
`f2_destination_is_not_in_schedule_state`, which looks for a public claim's destination in the schedule's stored state);
the scan is the check of a whole transaction as the RPC serves it.

## What the scan does not show

- It looks for identifiers. It says nothing about linking a claim to a destination by **amount or timing**: the claim's
  amount and time are public, and a later private movement of the same amount shortly afterwards is a pattern an
  observer could look for. The property is that the destination is not recorded, not that it cannot be guessed.
- It covers one transaction on a local dev-mode chain, not a population of transactions on a live network.
- It covers a native claim into a fresh private account. A token private claim (a private holding of the definition and
  a separate private fee payer) has not been run on a sequencer.
- It does not cover a claim with a real proof (dev-mode caveat above).

## What a user is shown

Before every claim to a private account the CLI and the mini-app show the same text (`PRIVACY_DISCLOSURE` in
`sdk/src/disclosure.rs`), and the mini-app does not send until it is acknowledged:

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

The mini-app's gate (a private claim is refused before the acknowledgement, with a wrong digest, and sent after it) is
shown, with a stand-in command line, in [evidence/miniapp/load-logosctl.log](../evidence/miniapp/load-logosctl.log).

## Cost of the private path

On the local dev-mode chain the estimated network fee of the claim in the lifecycle log was 5,528,416 for the private
claim and 803,320 for the public one (the CLI prints both as estimates). The private path also needs a wallet that holds
the private accounts and syncs private state. These are dev-mode figures from one run.
