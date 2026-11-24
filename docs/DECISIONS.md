# Design decisions

The decisions that shape the program, the SDK and the interface in [SPEC.md](../SPEC.md), with the reason for each.
Several came out of reviewing the first draft of the specification against LEZ v0.4.0-rc3's sources; those are marked
"changed in review" and are listed in the changelog at the end of SPEC.md. Findings about the platform that the
decisions rest on are in [PLATFORM-FINDINGS.md](PLATFORM-FINDINGS.md).

## 1. One escrow per schedule

Every schedule is its own program-derived account (`seed = SHA-256("cliff-veil/schedule/v1" ‖ creator ‖
schedule_id)`) and holds its own escrow, batch schedules included. No account is shared between two schedules.

Reason: claims on independent schedules touch no common state, which is what the RFP's concurrency requirement asks for
and what the end-to-end test `r3_concurrent_claims_on_independent_schedules` exercises. A pooled escrow would also make
one schedule's bug a loss for all of them. The state of a schedule is written once, so a `(creator, schedule_id)` pair
can never be reused, even after the escrow is empty (error 7).

## 2. Callers reach a schedule through their own actor

A user message is sent to the caller's own actor `(caller, vesting)`, which requires the caller's authorisation and
forwards an internal message; the schedule accepts only `from == (role holder, vesting)`.

Reason: LEZ v0.4 tells a handler only whether its own account authorised the transaction and who sent the message
(finding 1). Checking the sender's actor is the only authentication a schedule can do, and it refuses a direct
transaction, another program and a copy of the same bytecode at another id. Its cost is one extra program run per
operation.

## 3. A high-water time per schedule

`ScheduleV1.last_time` is the latest effective time any claim or cancel was computed at. Claims and cancels compute at
`max(t, last_time)`, `claimable` floors at zero, and `vested_at_cancel = max(vested_raw(te), claimed)`.

Changed in review. The first draft had no such field. Because a private claim (`ClaimAsOf`) can run up to one block
ahead of the clock another transaction in the same block reads, a `Cancel` after it could compute a vested amount below
what was already claimed, ask for a refund larger than the escrow, and revert. A beneficiary who sent one such claim per
block could keep a cancelable schedule from being cancelled. The fix costs 8 bytes of state and restores the invariant
`claimed <= vested(last_time)`; the property test `prop_f2_one_time_source_under_any_interleaving` in `core/tests/properties.rs` checks it under any interleaving, and
`f2_claim_as_of_then_cancel_in_the_same_block_uses_the_later_time` in `program/tests/executor.rs` runs the case.

## 4. A cancel refunds to the account that funded the escrow

`refund_to` defaults to `source`, the account that paid the escrow, not to the creator, and is fixed at creation. A
`refund_to` equal to the schedule account is refused.

Changed in review. A token holding has one definition per account, and a credit lands only on an existing holding of the
same definition. If the creator funded from another holding, or holds another token at its own actor, a refund to the
creator would revert on every cancel, permanently, because `refund_to` cannot change. The funding account already
accepts the asset back. An explicit `refund_to` is for a treasury or a multisig; it must be initialised for the asset
before the schedule is cancelled (an earlier transaction of its owner, since a transaction has one root call).

## 5. A private claim names a one-time blinded address

On the private path `to = destination.blinded(opening)` (`AccountId::blinded`), with 32 fresh random bytes as the opening
used once. Inside the proof the claimant's private account opens the address, so the credit lands on it in the same
transaction. The canonical id and the opening never appear in the transaction.

Changed in review. The first draft passed the private account's id as `to`. LEZ publishes the public root call of a
privacy-preserving transaction, so that id would have been on chain next to the beneficiary, which defeats the feature.
The SDK takes the destination as a private account the wallet owns (or a fresh one it creates), never as a bare id,
derives `to` itself, and refuses any other destination on the private path, public or foreign. Claiming to a private
account owned by someone else is not offered by the SDK in version 1. Evidence: the scan of a real transaction in
[PRIVACY.md](PRIVACY.md).

## 6. `ClaimAsOf` for private claims, with a validity window

`ClaimAsOf { schedule, to, as_of }` reads no clock. The program sets the validity window `[as_of, ∞)` on the transaction,
so the sequencer includes it only in a block whose timestamp is at least `as_of`. The SDK sets `as_of` to the last block's
timestamp. The claim pays what was vested at `as_of`; the next claim collects the rest.

Reason: a proof takes minutes, and a clock value read inside it would be stale when the transaction is included. The
window means a claim can never pay at a time that has not happened. `attack_as_of_in_the_future_refused` in
`program/tests/executor.rs` checks that. The wallet's simulation skips validity windows, so the SDK also checks `as_of`
against the clock itself before sending (finding 7).

## 7. The fee switch is a separate program, with a hard cap in both programs

The rate and treasury live in `vesting_fees`, administered through the RFP-001 admin-authority library
(`lez-admin-authority` v0.2.0). `vesting` asks it to `CheckFee` on every creation.

Reason: the library needs a typed entrypoint with its guard in front of the handler, and `vesting` needs the raw
entrypoint to receive the clock's reply, so one program cannot be both. A separate program also leaves `vesting` with no
admin and no configuration: the worst a fee admin can do is nothing to existing schedules. Conditions the review added,
all implemented:

- **The treasury travels with the message.** `Create` and `CreateBatch` carry `treasury`, and `CheckFee` carries `asset`,
  because `vesting` cannot read another program's state and `CheckFee` has to compare the treasury.
- **Token fees go to the treasury's associated token account**, derived by `vesting` from the checked treasury and the
  token definition (`ata_seed`). One treasury account serves every token; the ATA program's `Create` is permissionless
  and idempotent. With a fee of 0 no transfer is made and no treasury actor is declared.
- **The cap is enforced twice.** `vesting_fees` refuses a rate above 100 basis points (1%), and `vesting` refuses a quoted
  fee above `floor(total * 100 / 10,000)` itself, before asking, so the cap does not depend on the other program.
- **Both programs are deployed immutable, and `Init` is part of deployment** (`scripts/deploy_local.sh`), so the admin
  exists from the start and nobody else can claim the slot.
- **Creation never depends on the admin.** `CheckFee` is answered before the library's guard: an empty config reads as
  rate 0, and after `Renounce` the rate and treasury are frozen and `CheckFee` keeps answering. A creation with `fee = 0`
  passes in every config state. Tested by `fee_check_passes_before_the_fee_program_is_initialised` in `program/tests/executor.rs` and
  `fee_after_renounce_set_fee_is_refused_and_creation_goes_on` in `program/fees/tests/executor.rs`.
- **A stale quote fails cleanly.** The creation message carries the quoted fee and treasury; a mismatch is error 27.

Cost: one more program run per creation. The rate is 0 today.

## 8. Batch caps from measurement: 164 native and 57 token; the SDK splits by cycle estimate

`MAX_BATCH_NATIVE = 164` and `MAX_BATCH_TOKEN = 57`, two constants in `core/src/create.rs`, are 85% of the largest linear
batch of the released program that the chain accepts under its 10,000,000-cycle budget (193 native, 67 token), with the
next size refused ([evidence/batch/max.txt](../evidence/batch/max.txt)). A batch above its cap is refused with error 25.
Items with many milestones and a fee cost more, so the heaviest shape fits fewer (61 native, 38 token); the SDK splits a
recipient list into transactions by a cycle estimate fitted above the measurements and fills each to 85% of the budget,
and each is dry-run before it is sent.

Changed in review. The draft carried provisional caps taken from a stripped test program (300 native, 90 token) that
did not run the real handlers. A cap above the true maximum is a wrong number in a frozen document, so the constants
were replaced by the measurement of the released program, accepted at N and refused at N+1. The margin keeps a normal
batch from needing an empty block. A batch that nevertheless exceeds the budget is refused whole by the chain and
changes nothing (`r2_batch_over_the_budget_reverts_whole`).

## 9. No creator-funded fee float

The network fee of a privacy-preserving claim is paid by a private account of the claimant, out of the claimed amount
when it is a native claim larger than the fee. The creator is never asked to pre-fund a gas allowance for beneficiaries.

Reason: a float would be an extra escrowed amount per beneficiary, a refund path to define, and a link between the
creator and the beneficiary's accounts. The SDK dry-runs the claim, and when the payer cannot cover the fee it returns an
error naming the account and the shortfall; a token claim needs a private fee payer that holds native balance.

## 10. The token program is fixed at build time

`vesting` embeds `TOKEN_PROGRAM_ID`, `FEES_PROGRAM_ID` and `ATA_PROGRAM_ID` as build inputs, from `deploy/ids-<network>.toml`.
`Asset::Token` names only a definition, so no message can name a different token program.

Reason: otherwise a creator could point a schedule at a token program of its own that lets it take the escrow back or
refuses claims. The price is that the image id of `vesting` differs per network; the ids file records the image id built
for each deployment and `scripts/check_image_ids.sh` rebuilds and checks it.

## 11. Events are asserted at executor level

Events are emitted for every state transition and the tests assert them where they can be read: in the execution a
sequencer itself runs. The end-to-end tests on the standalone sequencer assert state. Reason: a standalone rc3 sequencer
serves no events (finding 5). We chose this over leaving events untested or asserting something the sequencer cannot
show. The consequence is that executor-level tests need the guests built (see the README's test table), and that the
read-back path through the indexer is not exercised here.

## 12. The SDK runs every transaction locally before it sends it

Reason: finding 7. The refusal text exists only in a local run, a reverted transaction is charged, and the wallet's
simulation misses validity windows. The SDK never sends a transaction it expects to revert, never sets the wallet's
send-reverting option, and reports a refusal with its code and an action. Codes are stable: never reused or renumbered;
CI fails if a code has no message.

## 13. `MakeNonCancelable` stays with the creator even when a cancel authority exists

The creator, or the cancel authority, may make a schedule non-cancelable. The RFP says the creator can do this, so the
creator keeps the power even when a separate cancel authority is named; doing so removes the cancel authority's power for
good. A creator who wants the cancel authority alone to decide names it and never sends `MakeNonCancelable`. This was a
gap in the first draft, written down as a decision in review.

## 14. A payout to the escrow itself is refused

`to` and `refund_to` equal to the schedule account are refused (error 26). Such a payout would burn the beneficiary's
entitlement into the escrow. The check is cheap and the error was previously reserved for a case no message could
trigger.

## 15. Rounding and `next_unlock` are exact

Vesting is integer arithmetic with a 192-bit intermediate, rounded down on every claim. Because vested equals the total
exactly once `t >= end`, the last claim pays `total - claimed`, so no rounding remainder is stranded. `next_unlock` (the
time the SDK shows when nothing is claimable) is the first time more becomes claimable, found by binary search over the
monotone curve in at most 64 steps. The first draft used a closed form that was wrong for a total so small that its
first unit vests after the cliff.

## 16. Milestone authority stays with an account, not an oracle

Signalling is a message from the milestone authority (default: the creator; a separate account, such as a multisig, may be
named). Reason: the RFP's design rationale: milestone conditions are often qualitative and cannot be evaluated on chain.
Signalling is idempotent per index (error 18), and moves no tokens; the beneficiary claims after it.
