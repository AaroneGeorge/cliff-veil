# Privacy

What a cliff-veil claim to a private account does and does not put into public on-chain state, how that was checked on a
real transaction, and what the check does not show. The binding statement is [SPEC.md](../SPEC.md) section 9.

**Dev-mode caveat.** The private claims of the lifecycle log and the end-to-end suite were made with
`RISC0_DEV_MODE=1`: the privacy-preserving transaction is built and accepted but carries no real proof. The CLI says so
on every such claim (`dev mode: RISC0_DEV_MODE=1, ...`). A native private claim and a token private claim were also made with real
proofs, with `RISC0_DEV_MODE` unset everywhere, and included by a standalone sequencer that verifies proofs; their scans
are [below](#the-scan-of-a-private-claim-with-a-real-proof). The native claim was proved in two runs, the token claim
in the second run only.

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
(`pr3_private_claim_to_a_public_account_is_refused_before_sending`, and the CLI line in the log below). It claims only to
private accounts the wallet owns: a private account owned by someone else is refused too, and claiming to one is not
offered by the SDK in version 1.

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

## The scan of a private claim with a real proof

The same scan was run on a private claim proved for real: `RISC0_DEV_MODE` unset in the client and in the sequencer's
environment, a standalone LEZ v0.4.0-rc3 sequencer that verifies proofs, on a GitHub-hosted runner. The record is
[evidence/real-proof/20261124T155037Z/](../evidence/real-proof/20261124T155037Z/) (`summary.json`, `scan.txt`,
`private-claim-transaction.json`, `private-claim-block.json`); how the run is made and what it does not show is in
[tools/real_proof/README.md](../tools/real_proof/README.md).

| What | Value |
|---|---|
| Transaction hash | `66d6e06d68ae4b76eca4d21996809286a1aebae4e94abdf98a286d1c05200d43` |
| Block | 193 |
| Canonical private account (the destination) | `EeCek4utt1ARP7VeGwS5nbXCrw41csYaYUaZwd3joFN7` |
| One-time address named in the transaction | `7ja3oycQ17GnQrHXcYCUJhDCyjbTV5yzVwD7QYxuZ7NE` |
| `getBlock`, canonical id: base58 / hex / raw bytes | 0 / 0 / 0 |
| `getTransaction`, canonical id: base58 / hex / raw bytes | 0 / 0 / 0 |
| `getBlock`, one-time address, raw bytes in decoded base64 | 4 |
| `getTransaction`, one-time address, raw bytes in decoded base64 | 4 |
| Total, canonical id / one-time address | 0 / 8 |
| Size of the transaction (the proof is in it) | 251,270 bytes |

The result is the same as for the dev-mode claim: the canonical id of the destination is nowhere in the block or the
transaction, and the one-time address is, 8 times. The claim amount (305,555,555) is public in the same way. The run also
sent the same claim once with a dev-mode receipt; the sequencer refused it with `Invalid privacy preserving execution
circuit proof` (`sequencer.log` of the record), so the sequencer was checking proofs, and the claim in the table is one
it verified.

### Second real-proof run: a native claim and a token claim

A second run, on a GitHub-hosted runner as the first (GitHub run 45437451120), is recorded in
[evidence/real-proof/20261124T165520Z/](../evidence/real-proof/20261124T165520Z/) (`summary.json`, `scan.txt`,
`token-scan.txt`, `token-summary.json`, `token-steps.json`, the transaction and block answers, and
`control-sequencer-log.txt`). It repeats the native claim and adds the token claim.

The control claim made with a dev-mode receipt was refused again; the sequencer's log line, kept in
`control-sequencer-log.txt`, reads `Invalid privacy preserving execution circuit proof`.

| What | Native claim | Token claim |
|---|---|---|
| Transaction hash | `fc85d370db0b1044aac8ce34306e40d01a22753e2506d0d4e2ba5b04894fdc1b` | `51efe1b6c5741087a65aa0ac3c7c34e9289b474c42686a1bf52f8e0b9f7e8bb9` |
| Block | 231 | 728 |
| Amount claimed | 319,443,518 | 1,000,000 |
| Private destination (canonical id) | `FZHw3uhx234Sx5NgFLtrR56qAsP2V1R7ygoBiAvFj67G` | `51ApLWXTH2WPmcw4F1ZENDfCZyjcEWafLChnMTN4iuCo` |
| Private fee payer | the destination itself | `7duxzpSAcvYTYELv5ut858TdEqMcKJGJYUpBKGSSvj13` |
| One-time address named in the transaction | `DWWX1n8P33HSh7BHJsc1dxcxzEMVHXKDCdRBx7Q61tnS` | `BwZQ9vVs1XniBiNXUzCNkEiFhswa5zCeg2uoeCh5Cex4` |
| Scan total, destination | 0 | 0 |
| Scan total, fee payer | (the destination) | 0 |
| Scan total, one-time address | 8 | 8 |
| Size of the claim transaction | 251,270 bytes | 255,200 bytes |

The token claim goes to a private holding of the token that the beneficiary initialised before, and pays its fee from a
separate private fee payer, since a token claim brings no native balance. Three steps were proved, one after the other:
funding the private fee payer, initialising the private holding, and the claim. Each took 25 to 50 minutes on 4 CPUs
(`token-steps.json` has the seconds of each). The private holding held 1,000,000 afterwards, and the fee payer paid
22,272,688 across the initialisation and the claim. The scan found neither the destination nor the fee payer in the
block or the transaction of the claim, and the one-time address 8 times, as for the native claim.

## What the scan does not show

- It looks for identifiers. It says nothing about linking a claim to a destination by **amount or timing**: the claim's
  amount and time are public, and a later private movement of the same amount shortly afterwards is a pattern an
  observer could look for. The property is that the destination is not recorded, not that it cannot be guessed.
- Each scan covers one transaction on a local chain (the first in dev mode, the others with proofs verified by the
  sequencer), not a population of transactions on a live network. The real-proof sequencers were ephemeral and are gone;
  the records keep the RPC answers that were scanned.
- The real-proof scans cover two native claims into a fresh private account and one token claim into a private holding
  with a separate private fee payer (the token claim also ran in dev mode on a sequencer:
  [evidence/e2e/local-dev/20261124T160153Z.json](../evidence/e2e/local-dev/20261124T160153Z.json)). The token claim
  was made once.

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

The claimant also proves the transaction locally. For the real-proof claim above that took 1,229 s for the whole CLI
command on a 4-core hosted runner, with a peak of 9,612,576 KiB (9.17 GiB) of memory; the fee estimate of that claim was
5,528,784. The native claim of the second run took 1,562 s with the same peak, and each of the three proved steps of the
token claim 25 to 50 minutes. This is one run on one machine; the figures and how they were taken are in [COSTS.md](COSTS.md).
