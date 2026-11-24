# cliff-veil interface specification

Status: **Version 1 (frozen).** A change to anything below is a versioned change: new enum variants are appended,
existing variants and field orders never change, error codes are never reused or renumbered, and every change is
listed in the changelog at the end of this file.

Target: LEZ v0.4.0-rc3 (commit `bafd07b`), actor model. Token and associated-token-account programs:
`logos-blockchain/lez-programs`, branch `refactor/lez-v0.4.x` at commit `3f04cc3`. Encodings are Borsh. Times are
LEZ clock timestamps in milliseconds (`u64`). Amounts are `u128`. "Account" means a LEZ `AccountId` (32 bytes).

This document is the contract between the program (`program/`), the pure rules crate (`core/`), the SDK and CLI
(`sdk/`, `cli/`), the IDL (`idl/`), the end-to-end tests (`e2e/`) and the mini-app (`miniapp/`). Every type below
is defined once, in `core/`, with the same field order.

---

## 1. Programs and accounts

### 1.1 Programs

| Program | What it is | Id |
|---|---|---|
| `vesting` | The vesting program (this repository, `program/`) | Its deployment address (the program-loader header account), recorded per network in `deploy/ids-<network>.toml` |
| `vesting_fees` | The fee switch: one config account, administered through `lez-admin-authority` v0.2.0 (`program/fees/`) | As above |
| token | The LEZ fungible token program at the pinned commit | As above. The one token program a deployment of `vesting` uses is fixed at build time (`TOKEN_PROGRAM_ID`, from `deploy/ids-<network>.toml`). `Asset::Token` names only a definition, so no message can name another token program |
| associated token account (ATA) | The ATA program at the pinned commit; used only to locate the treasury's holding for a token fee (section 6) | As above (`ATA_PROGRAM_ID`, build input) |
| native token | Built into LEZ (`lee_core::native_token`, id `[0; 32]`) | Fixed |
| clock | Built into LEZ (`clock_core::clock_actor()`) | Fixed |

`vesting` embeds the ids of `vesting_fees`, the token program and the ATA program at build time (`FEES_PROGRAM_ID`,
`TOKEN_PROGRAM_ID`, `ATA_PROGRAM_ID`). They are build inputs, so the image id of `vesting` is fixed per network;
`deploy/ids-<network>.toml` records the ids and the image id built for each.

### 1.2 Actors used by `vesting`

An actor is an account together with a program. `vesting` uses three kinds:

| Actor | Account | Derivation | State under `vesting` |
|---|---|---|---|
| Schedule | program-derived | `seed = SHA-256("cliff-veil/schedule/v1" ‖ creator ‖ schedule_id)`, account = `AccountId::for_public_pda(vesting, seed)` | `ScheduleState` (section 2) |
| Caller actor ("seat") | the caller's own account | none: `(caller, vesting)` | always empty; never written |
| Escrow | the schedule's account | same account as the schedule | native balance under the native token program, or a token holding under the token program |

- `creator` is 32 bytes, `schedule_id` is 32 bytes chosen by the creator (the SDK uses a random value or
  `SHA-256(label)`). The literal prefix is the 22 ASCII bytes `cliff-veil/schedule/v1`.
- No private key exists for a schedule account. Only `vesting` can authorise a debit of its escrow, by attaching
  `seed` to the debit message (`native_token::custody_transfer`, or token `Transfer` with `with_pda_seeds`).
- One escrow per schedule, batch schedules included. No account is shared between two schedules, so claims on
  independent schedules never touch common state.

**Admission of new accounts.** LEZ rc3 delivers a message to a public account only if the account already exists,
the transaction carries admission evidence for it (`PublicAccountEvidence::Key` or `::Pda`), or the delivery
carries a PDA seed of the sending program that derives it. Therefore:
- the caller actor sends `Open` to a new schedule **with the schedule seed attached**, which admits the new account;
- a transaction that pays a never-seen public account (a fresh claim destination, refund account or treasury), or
  that sends to the actor of a never-seen caller, carries `PublicAccountEvidence::Key` for it. The SDK adds this.

**One root call per transaction.** An rc3 transaction has exactly one root call. Any preparation a claim, cancel or
fee transfer depends on (a token holding for a destination, a refund account or the treasury) is a transaction of
its own, sent earlier (sections 3.1 and 6).

`vesting_fees` uses one config actor: account = `AccountId::for_public_pda(vesting_fees, lez_admin_authority::seed(b"cliff-veil/fees/v1"))`.

### 1.3 How a caller is authenticated

LEZ 0.4 tells a handler only whether the account it runs on authorised the transaction (`ReceiveInput::is_authorized`)
and which actor sent the message (`ReceiveInput::from`, set by the engine). So:

1. Every user message is sent to the caller's own actor `(caller, vesting)`. That actor requires
   `is_authorized` (the caller signed, or the caller is a PDA whose owning program attached its seed) and
   forwards an internal message to the schedule actor `(schedule, vesting)`.
2. The schedule accepts an internal message only when `from == (expected_role_holder, vesting)` exactly, where the
   role holder is read from its own state (beneficiary, creator, cancel authority, milestone authority).
3. A message sent straight to a schedule by a transaction (`from == None`), by another program, or by a copy of
   the same bytecode deployed at another id, is refused (section 1.4).

This is the routing `lez-admin-authority` uses for its admin, and the token program uses for `Mint`.

Platform property, not a choice of `vesting`: a signature authorises every actor of the signer's account declared
in the transaction, so any program in a transaction the beneficiary signs can send messages as
`(beneficiary, vesting)`. A beneficiary signs only transactions whose root call and declared actors it has
checked.

### 1.4 Message dispatch

`vesting` uses the raw entrypoint (`actor_entrypoint!(raw ...)`), because it receives the clock's reply, and decodes
by sender:

| `input.from` | Decoded as |
|---|---|
| `Some(clock_core::clock_actor())` | `clock_core::Reply` |
| `Some(actor)` with `actor.program_account_id == vesting` | `Internal` (section 3.3) |
| anything else (`None`, or another program) | `Message` (section 3.1), handled as a caller actor |

Before handling, the receiving actor's own state is checked against the decoded kind:
- a `Message` received by an actor whose state is not empty (a schedule) is refused with 23: only caller actors,
  which are always empty, accept `Message`;
- an `Internal` message other than `Open`, or a clock reply, received by an empty actor is refused with 8.

---

## 2. Schedule state

`ScheduleState` is the Borsh-encoded actor state of a schedule actor under `vesting`. It is a versioned enum;
only `V1` exists.

```rust
pub enum ScheduleState {
    V1(ScheduleV1),            // tag 0
}

pub struct ScheduleV1 {
    pub schedule_id: [u8; 32],
    pub creator: AccountId,
    pub beneficiary: AccountId,         // changes only by TransferBeneficiary
    pub asset: Asset,
    pub total: u128,                    // escrowed at creation, > 0
    pub claimed: u128,                  // paid out so far, <= vested(last_time)
    pub last_time: u64,                 // high-water time, section 4; 0 until the first claim or cancel
    pub kind: Kind,
    pub cancelable: bool,               // true -> false only (MakeNonCancelable)
    pub transferable: bool,             // written at creation, never changed
    pub cancel_authority: AccountId,    // default: creator
    pub milestone_authority: AccountId, // default: creator
    pub refund_to: AccountId,           // default: the funding account (`source`); fixed at creation
    pub cancelled: Option<Cancellation>,
    pub pending: Option<Pending>,       // Some only inside one transaction
}

pub enum Asset {
    Native,                                 // tag 0
    Token { definition: AccountId },        // tag 1; program = TOKEN_PROGRAM_ID
}

pub enum Kind {
    Linear { start: u64, cliff: u64, end: u64 },        // tag 0; start <= cliff <= end, start < end
    Milestones { amounts: Vec<u128>, signalled: u64 },  // tag 1; 1..=64 amounts, each > 0, sum == total
}

pub struct Cancellation {
    pub at: u64,              // effective time of the cancel: max(clock time, last_time)
    pub vested_at_cancel: u128,
    pub refunded: u128,       // total - vested_at_cancel
}

pub enum Pending {
    Claim { to: AccountId },  // tag 0
    Cancel,                   // tag 1
}
```

Rules:
- The state is written once at creation (`lee_core::program::write_once` semantics): a non-empty schedule actor
  refuses `Open` (error 7), so a `(creator, schedule_id)` pair can never be reused, even after the escrow is empty.
- `last_time` is the latest effective time any claim or cancel on this schedule was computed at. It never
  decreases (section 4).
- `pending` is set when a handler asks the clock for the time and cleared by the clock reply in the same
  transaction. A message that finds `pending` set is refused (error 21). Because the clock reply is synchronous,
  `pending` is always `None` between transactions, so a destination in `Pending::Claim` is never stored past its
  transaction.
- Fully linear vesting is `Linear` with `cliff == start`. A pure cliff (all at once) is `cliff == end`.
- Milestone bit `i` of `signalled` is milestone `amounts[i]`.

---

## 3. Messages

### 3.1 `Message` (sent by a transaction to the caller's own actor)

Borsh enum, tags in order. "Caller" is the account the message is sent to; it must have authorised the
transaction (else error 1).

| Tag | Message | Fields | Who may call | Effect |
|---|---|---|---|---|
| 0 | `Create` | `schedule_id: [u8;32]`, `terms: Terms`, `source: AccountId`, `fee: u128`, `treasury: AccountId` | anyone (the caller becomes `creator`) | Checks `fee` against the hard cap (28) and asks `vesting_fees` to check `fee` and `treasury` (`CheckFee`, 27; section 6). Opens the schedule at the derived account, initialises its escrow (token: `InitializeHolding` with the seed), moves exactly `terms.total` from `source` into the escrow, and, when `fee > 0`, pays `fee` from `source` to the fee destination (section 6). One transaction; any failure reverts all. Emits `ScheduleCreated` |
| 1 | `CreateBatch` | `common: BatchTerms`, `items: Vec<BatchItem>`, `source: AccountId`, `fee: u128`, `treasury: AccountId` | anyone | As `Create` for every item, each with its own schedule account and escrow, in one transaction; one fee on the sum of item totals. `1 <= items.len() <= MAX_BATCH_NATIVE` or `MAX_BATCH_TOKEN` (errors 24, 25; section 8); two items with the same `schedule_id` are refused (7). One `ScheduleCreated` per item |
| 2 | `Claim` | `schedule: AccountId`, `to: AccountId` | the beneficiary | Refuses `to == schedule` (26). Reads the clock (granularity 1 ms) and pays `claimable(now)` (section 4) from the escrow to `to`. Refused with error 14 when nothing is claimable. Emits `Claimed` |
| 3 | `ClaimAsOf` | `schedule: AccountId`, `to: AccountId`, `as_of: u64` | the beneficiary | As `Claim`, computed at `as_of` instead of a clock read (section 4.1); the program sets the validity window `[as_of, ∞)` on the transaction, so it can only be included in a block whose timestamp is at least `as_of`. Used for privacy-preserving claims, whose proof may take minutes; there `to` is a one-time blinded address (section 9) |
| 4 | `Cancel` | `schedule: AccountId` | the cancel authority | Only if `cancelable` and not cancelled (errors 15, 16). Reads the clock, freezes vesting at the effective time (section 4) and pays `refunded = total - vested_at_cancel` to `refund_to`; when `refunded == 0` the cancel is valid and no transfer is made. Vested but unclaimed tokens stay claimable. Emits `Cancelled` |
| 5 | `MakeNonCancelable` | `schedule: AccountId` | the creator or the cancel authority | `cancelable := false`; one way. Refused when already non-cancelable (error 15) or cancelled (16). Emits `MadeNonCancelable` |
| 6 | `SignalMilestone` | `schedule: AccountId`, `index: u8` | the milestone authority | Sets bit `index`. Refused for a linear schedule (20), an index out of range (17), a bit already set (18), a cancelled schedule (16). Moves no tokens. Emits `MilestoneSignalled` |
| 7 | `TransferBeneficiary` | `schedule: AccountId`, `new_beneficiary: AccountId` | the current beneficiary | Only if `transferable` (19). Emits `BeneficiaryTransferred` |

Decision on `MakeNonCancelable`: the creator keeps the power to make a schedule non-cancelable even when a separate
cancel authority is named (the RFP's cancellation requirement says "by the creator"). Doing so removes the cancel
authority's power for good. A creator who wants the cancel authority alone to decide names it and does not send
`MakeNonCancelable`.

```rust
pub struct Terms {
    pub beneficiary: AccountId,
    pub asset: Asset,
    pub total: u128,
    pub kind: Kind,                          // Milestones.signalled must be 0
    pub cancelable: bool,                    // SDK default: true
    pub transferable: bool,                  // SDK default: false
    pub cancel_authority: Option<AccountId>, // None -> creator
    pub milestone_authority: Option<AccountId>, // None -> creator
    pub refund_to: Option<AccountId>,        // None -> source (the account that funded the escrow)
}

pub struct BatchTerms {                      // shared by every item
    pub asset: Asset,
    pub shape: BatchShape,
    pub cancelable: bool,
    pub transferable: bool,
    pub cancel_authority: Option<AccountId>,
    pub milestone_authority: Option<AccountId>,
    pub refund_to: Option<AccountId>,        // None -> source
}

pub enum BatchShape {
    Linear { start: u64, cliff: u64, end: u64 },   // tag 0
    /// Milestone weights in basis points, summing to 10_000. Item amounts are
    /// floor(total * bps / 10_000); the last milestone takes the remainder.
    Milestones { bps: Vec<u16> },                    // tag 1
}

pub struct BatchItem {
    pub schedule_id: [u8; 32],
    pub beneficiary: AccountId,
    pub total: u128,
}
```

Funding sources: for `Asset::Native`, `source` is an account whose native balance is debited (it must have
authorised the transaction; normally `source == creator`). For `Asset::Token`, `source` is a token holding of the
definition that has authorised the transaction (normally the creator's own account).

Refund account: `refund_to` defaults to `source`, the account that funded the escrow, which therefore already accepts
the asset back. An explicit `refund_to` equal to the schedule account is refused at `Open` (26).

Payout destinations (`to`, `refund_to`):
- Any account other than the schedule account itself (26).
- Token schedules: a credit lands only on an existing holding of the same definition, and only the account itself
  can initialise its holding. A destination or refund account without a holding is initialised by its owner, with
  token `InitializeHolding`, in an earlier transaction; the SDK checks before sending and, if the holding is missing, refuses with `NoHolding` and names the transaction the owner has to send. A
  claim to an account with no holding reverts as a whole; nothing is consumed and the claim can be retried. The same
  holds for a private token destination: its private holding is initialised in an earlier privacy-preserving
  transaction (section 9).
- `to` is never stored past its transaction and never emitted.

### 3.2 Transaction shapes

Every actor reached must be declared in the transaction. The SDK builds these lists. Each transaction has one root
call (section 1.2).

Admission evidence: `Key` for every account in the list that does not exist yet and is not a signer (section 1.2).
`fees_config` is the `vesting_fees` config account; `treasury_ata` is the treasury's associated token account for
the definition (section 6). `native(x)` is `Actor::native_balance(x)`.

| Operation | Root actor | Declared actors | Signer |
|---|---|---|---|
| Create, native | `(creator, vesting)` | root, `(schedule, vesting)`, `native(source)`, `native(schedule)`, `(fees_config, vesting_fees)`, `native(treasury)` if fee > 0 | creator (and `source` if different) |
| Create, token | `(creator, vesting)` | root, `(schedule, vesting)`, `(schedule, token)`, `(source, token)`, `(definition, token)`, `(fees_config, vesting_fees)`, `(treasury_ata, token)` if fee > 0 | creator (and `source` if different) |
| CreateBatch, native | `(creator, vesting)` | root, for every item `(schedule_i, vesting)` and `native(schedule_i)`, then `native(source)`, `(fees_config, vesting_fees)`, `native(treasury)` if fee > 0 | creator (and `source` if different) |
| CreateBatch, token | `(creator, vesting)` | root, for every item `(schedule_i, vesting)` and `(schedule_i, token)`, then `(source, token)`, `(definition, token)`, `(fees_config, vesting_fees)`, `(treasury_ata, token)` if fee > 0 | creator (and `source` if different) |
| Claim (public) | `(beneficiary, vesting)` | root, `(schedule, vesting)`, clock actor, escrow actor, destination actor (`native(to)` or `(to, token)`) | beneficiary |
| ClaimAsOf, public destination | `(beneficiary, vesting)` | root, `(schedule, vesting)`, escrow actor, destination actor | beneficiary |
| ClaimAsOf, privacy-preserving | `(beneficiary, vesting)` | public part: root, `(schedule, vesting)`, escrow actor, destination actor at the blinded address `to`; private part: the claimant's destination account, which opens `to` inside the proof (`PrivateWitness.address_openings`), and the private fee payer (section 9) | beneficiary (public), claimant (private, inside the proof) |
| Cancel | `(cancel_authority, vesting)` | root, `(schedule, vesting)`, clock actor, escrow actor, refund actor | cancel authority |
| MakeNonCancelable, SignalMilestone, TransferBeneficiary | caller | root, `(schedule, vesting)` | caller |

### 3.3 `Internal` (caller actor to schedule; accepted only from a `vesting` actor)

| Tag | Message | Accepted only from | Effect at the schedule |
|---|---|---|---|
| 0 | `Open { state: ScheduleV1 }` | `(state.creator, vesting)` | Refused, in this order: receiving account other than the derivation of `(state.creator, state.schedule_id)` (9); actor not empty (7); terms invalid (10, 11, 13, 12, 29, as `ScheduleV1::validate_new`); state not fresh, that is `claimed != 0`, `last_time != 0`, `cancelled` or `pending` set (2); `refund_to` equal to the receiving account (26). Otherwise writes the state |
| 1 | `ClaimFor { to }` | `(beneficiary, vesting)` (else 3) | Refuses `to` equal to the receiving account (26). Sets `pending = Claim{to}`, asks the clock |
| 2 | `ClaimForAsOf { to, as_of }` | `(beneficiary, vesting)` (else 3) | Refuses `to` equal to the receiving account (26). Pays at `max(as_of, last_time)`, sets the validity window `[as_of, ∞)` |
| 3 | `CancelFor` | `(cancel_authority, vesting)` (else 5) | Checks 16, 15; sets `pending = Cancel`, asks the clock |
| 4 | `MakeNonCancelableFor` | `(creator, vesting)` or `(cancel_authority, vesting)` (else 4) | as in 3.1 |
| 5 | `SignalFor { index }` | `(milestone_authority, vesting)` (else 6) | as in 3.1 |
| 6 | `TransferFor { new_beneficiary }` | `(beneficiary, vesting)` (else 3) | as in 3.1 |

The clock reply (`Reply::Timestamp(now)`) completes the pending operation and clears `pending`; a reply with
nothing pending is refused (22).

### 3.4 `vesting_fees` messages

Typed entrypoint, `lez-admin-authority` v0.2.0 guard. Admin operations (`Init`, `Nominate`, `Accept`, `Renounce`)
are the library's own, sent from the admin's seat as `admin_op(..)`. The two messages below are encoded exactly as
`vesting_core::FeeMessage`: `vesting_fees` declares its message enum with the same variants, tags and field order
(adding only the library's guard attributes), and a test checks that both encode every variant to the same bytes.

| Tag | Message | Who | Effect |
|---|---|---|---|
| 0 | `CheckFee { total: u128, fee: u128, treasury: AccountId, asset: Asset }` | anyone (read-only) | Refuses (error 27) unless `fee == floor(total * rate_bps / 10_000)` and, when `fee > 0`, `treasury` is the configured treasury (any treasury is accepted when the fee is 0). `treasury` is always the treasury account itself, never its associated token account: `vesting` derives the fee destination (section 6). `asset` names the asset the fee is paid in; version 1 applies one rate to every asset. Writes nothing. Answered only at the config actor (27 at any other actor of `vesting_fees`), and before the library's guard, so it is accepted in every config state: an empty config (before `Init`) reads as rate 0, and it keeps working after `Renounce` |
| 1 | `SetFee { rate_bps: u16, treasury: AccountId }` | admin only (`#[only(Admin)]`) | Refuses `rate_bps > MAX_FEE_BPS` (error 28). Sets both fields. Emits `FeeChanged` |

Config state: `{ rate_bps: u16, treasury: AccountId, admin: AuthoritySlot }`, Borsh, in this order. `Init` writes
`rate_bps = 0`, `treasury` all zero, and installs the admin; an empty config is read as that same state without an
admin. The token and ATA programs are not part of the config: `vesting` holds them as build inputs (section 1.1).

---

## 4. Vesting arithmetic (`core/`, shared by program and SDK)

All integer, no floating point. `mul_div_floor(a: u128, b: u64, c: u64)` computes `floor(a * b / c)` with a 192-bit
intermediate (it never overflows; the result is `<= a` because `b <= c`).

```
vested_raw(t):
  Linear{start, cliff, end}:
      t <  cliff  -> 0
      t >= end    -> total
      otherwise   -> mul_div_floor(total, t - start, end - start)
  Milestones{amounts, signalled}:
      sum of amounts[i] for every set bit i

vested(t)      = vested_at_cancel        if cancelled
               = vested_raw(t)           otherwise
effective(t)   = max(t, last_time)
claimable(t)   = vested(effective(t)) - claimed, floored at 0
```

- **Claim at time `t`** (the clock time, or `as_of`): computed at `te = effective(t)`. Pays
  `claimable(t)`, sets `claimed += amount` and `last_time = te`. `Claimed.at = te`. Refused with 14 when the amount
  is 0; a refused claim changes nothing.
- **Cancel at clock time `c`**: computed at `te = effective(c)`.
  `vested_at_cancel = max(vested_raw(te), claimed)`, `refunded = total - vested_at_cancel`, `last_time = te`,
  `Cancellation.at = Cancelled.at = te`. The escrow then holds `vested_at_cancel - claimed`, all of it claimable.
  With the effective time `vested_raw(te) >= claimed` already holds; the `max` is a second guard, so the refund never
  exceeds what the escrow holds.
- Rounding is down on every claim. Because `vested_raw(t) == total` exactly once `t >= end`, the last claim pays
  `total - claimed`, which includes every unit lost to rounding: nothing is ever stranded.
- At the cliff the amount `mul_div_floor(total, cliff - start, end - start)` unlocks at once; after it, accrual
  is linear from `start`.
- Batch milestone amounts: `amounts[i] = floor(total * bps[i] / 10_000)` for `i < n-1`, and
  `amounts[n-1] = total - sum(others)`.

Invariants (property-tested in `core/` under any interleaving of claims at any time, signals, `MakeNonCancelable`
and a cancel at any clock time):
- `vested` is non-decreasing in `t`, and `vested(t) <= total`.
- `last_time` never decreases.
- `claimed <= vested(last_time)`, hence `claimed <= vested(effective(t))` for every `t`. (For a `t` earlier than
  `last_time`, `vested(t)` may be below `claimed`; no computation uses such a `t`.)
- A cancel of a cancelable, not cancelled schedule always succeeds, and `claimed + refunded + escrow == total`
  holds after every operation.

`next_unlock(t)` (used in error 14 and by the SDK), at `te = effective(t)`:
- Cancelled, or `max(claimed, vested_raw(te)) == total`: none, ever.
- Milestones: when the milestone authority signals one.
- Linear: the smallest `t' > te` with `vested_raw(t') > max(claimed, vested_raw(te))`, the first moment more
  becomes claimable. When nothing is claimable at `te` (the case of error 14) this is the first time with
  `vested_raw(t') > claimed`, never before `cliff`. Found by binary search over `(te, end]` on the monotone curve
  (at most 64 steps), which is exact for every total and span, including a total so small that the first unit vests
  after the cliff.

### 4.1 Time

- **Clock time.** In rc3 the clock account is updated by the last transaction of each block, so every other
  transaction in block `n` reads block `n-1`'s timestamp. `Claim` and `Cancel` use that time.
- **`as_of`.** `ClaimAsOf` reads no clock. The validity window `[as_of, ∞)` lets the sequencer include it only in a
  block whose own timestamp is at least `as_of`, so `as_of` can be up to one block ahead of the clock time another
  transaction in the same block reads. `last_time` makes the two sources one: a `Cancel` after such a claim in the
  same block is computed at the claim's time, never earlier.
- **What the SDK uses.** The SDK sets `as_of` to the timestamp of the last block when it builds the claim. A
  privacy-preserving claim is included after its proof is done, minutes later; it pays what was vested at `as_of`,
  and the next claim collects the rest. "Claimable now" in the SDK and CLI is `claimable(T)` with `T` the last
  block's timestamp: the value a `Claim` in the next block computes, or less if another block lands first.
- **`Claimed.at` and `Cancelled.at`** are the effective time `te`, not the inclusion time of the transaction.

---

## 5. Errors

The program refuses by panicking with the text `cliff-veil error <code>: <message>`, which LEZ returns in the
failed transaction's error (`ProgramExecutionFailed`). Codes are stable: a code is never reused or renumbered. The
SDK maps every code to the message below (`vesting_core::ERRORS`) and CI fails if one is missing.

| Code | Name | Message |
|---|---|---|
| 1 | `NotAuthorised` | the caller did not authorise this transaction |
| 2 | `InvalidMessage` | the message does not decode |
| 3 | `NotBeneficiary` | only the beneficiary may do this |
| 4 | `NotCreator` | only the creator or the cancel authority may do this |
| 5 | `NotCancelAuthority` | only the cancel authority may cancel |
| 6 | `NotMilestoneAuthority` | only the milestone authority may signal a milestone |
| 7 | `ScheduleExists` | a schedule with this id already exists for this creator |
| 8 | `NoSuchSchedule` | no schedule at this account |
| 9 | `AddressMismatch` | the schedule account does not match the creator and schedule id |
| 10 | `ZeroTotal` | the total must be greater than zero |
| 11 | `BadTimes` | times must satisfy start <= cliff <= end and start < end |
| 12 | `BadMilestones` | milestone amounts must be non-zero and sum to the total |
| 13 | `TooManyMilestones` | at most 64 milestones |
| 14 | `NothingClaimable` | nothing claimable yet, next unlock at `<T>` (or: "next unlock when a milestone is signalled"; or: "nothing left to vest") |
| 15 | `NotCancelable` | the schedule is not cancelable |
| 16 | `AlreadyCancelled` | the schedule is cancelled |
| 17 | `MilestoneOutOfRange` | no milestone with this index |
| 18 | `MilestoneAlreadySignalled` | this milestone was already signalled |
| 19 | `NotTransferable` | this schedule is not transferable |
| 20 | `NotMilestoneSchedule` | this schedule has no milestones |
| 21 | `OperationPending` | another operation on this schedule is in progress in this transaction |
| 22 | `UnexpectedClockReply` | clock reply with no operation pending |
| 23 | `UnexpectedSender` | a schedule accepts messages only from a vesting actor |
| 24 | `EmptyBatch` | a batch needs at least one item |
| 25 | `BatchTooLarge` | too many schedules in one batch |
| 26 | `PayoutToEscrow` | a claim destination or refund account cannot be the schedule account |
| 27 | `FeeMismatch` | the creation fee does not match the current rate or treasury |
| 28 | `FeeAboveCap` | the fee rate exceeds the hard cap |
| 29 | `Overflow` | amount overflow |

When each is raised: 1 at the caller actor; 2 for an undecodable message or a hand-made `Open` with a state that is
not fresh; 3 to 6 by role (section 3.3); 7 to 13 and 26 at `Open` and batch validation; 8 and 23 by dispatch
(section 1.4); 9 guards `Open`: `vesting` itself names the creator and derives the schedule account when it sends
`Open`, and no user message forwards to `Open`, so a well-formed transaction cannot reach 9; it stays as a guard;
14 to 20 by the rules of section 4; 21 and 22 guard the clock round trip (a synchronous reply makes
them unreachable in a well-formed transaction; they stay as guards); 24 and 25 by `CreateBatch`; 27 by
`vesting_fees`; 28 by `vesting_fees` (`SetFee`) and by `vesting` (fee above the cap on the creation total); 29 only
for a state no valid creation can produce.

`<T>` in code 14 is the `next_unlock` time in milliseconds since the Unix epoch; the SDK also renders it as
ISO-8601 UTC. Failures raised by LEZ itself (a token transfer to an account without a holding, insufficient
balance, a validity window not yet open) are not program codes; the SDK maps the known ones to messages
(`sdk/src/errors.rs`).

How a client reads a code: the SDK executes every transaction locally against current state before sending it and
reports the refusal text from that run, with the code parsed by `vesting_core::parse_code`. On LEZ v0.4.0-rc3 the
sequencer does not return a program's refusal text: a transaction sent anyway is included as reverted and its fee is
charged. The local run skips validity windows, so before sending `ClaimAsOf` the SDK also checks `as_of` against the
latest clock timestamp. The SDK never sends a transaction it expects to revert. A privacy-preserving transaction that
would be refused fails while its proof is built locally, before anything is sent.

---

## 6. Fees

- **Who pays:** the creator, from the funding `source`, in the schedule's asset.
- **When:** at creation only (`Create`, `CreateBatch`). Claims, cancels, signals and transfers carry no protocol fee
  (only the network's own transaction fee).
- **Rate:** `fee = floor(total * rate_bps / 10_000)` per creation message (batch: on the sum of item totals).
  `rate_bps = 0` after `Init`. Hard cap `MAX_FEE_BPS = 100` (1%), a constant in `core/`; no message can raise it.
- **Cap enforced twice:** `vesting_fees` refuses a rate above the cap (28), and `vesting` itself refuses a quoted
  `fee` above `floor(total * MAX_FEE_BPS / 10_000)` (28) before it asks `vesting_fees`, so the cap does not depend on
  the other program.
- **Where:** the creation message names `treasury`; `vesting_fees` checks it is the configured treasury whenever
  `fee > 0`. A native fee goes to `native(treasury)`. A token fee goes to the treasury's associated token account
  for the definition: `treasury_ata = AccountId::for_public_pda(ATA_PROGRAM_ID, compute_ata_seed(TOKEN_PROGRAM_ID,
  treasury, definition))` (ATA program core, `compute_ata_seed`), credited as the actor `(treasury_ata, token)`.
  `vesting` derives it from the checked treasury with `vesting_core::ata_seed`, so one treasury account serves every
  token. The ATA must exist: anyone can create it with the ATA program's `Create`, which is permissionless and idempotent; the SDK sends it in
  an earlier transaction when the fee is non-zero and the ATA is missing. With `fee == 0` no transfer is made and
  no treasury actor is declared.
- **Who changes it:** the admin of `vesting_fees`, through `lez-admin-authority` v0.2.0 (single admin, two-step
  handover, renounceable). The library is `MIT OR Apache-2.0`, pinned to LEZ v0.4.0-rc3.
- The creation message carries the fee and treasury the SDK quoted; `vesting_fees` refuses a mismatch (error 27), so
  a rate or treasury change between quote and execution makes creation fail cleanly rather than charge an
  unexpected amount.
- No broker fee.

**Deployment.** Both programs are deployed immutable: no account can replace either program's code afterwards. The
deployment script sends `vesting_fees`' `Init` from the deployer right after deploying it, before `vesting` is used,
so the admin exists from the start and no one else can claim the slot; a treasury is set later with `SetFee`.
Creation never depends on the admin: `CheckFee` is answered before the library's guard, an empty config (the library
would refuse any other message on it, its E101) reads as rate 0, and after `Renounce` the rate and treasury are
frozen and `CheckFee` keeps answering. So a creation with `fee = 0` passes in every config state, and a non-zero fee
passes only with the configured rate and treasury.

---

## 7. Events (LP-0012 `ProgramEvent`)

- Selector: the first 8 bytes of `SHA-256("vesting::<EventName>")` (the LEZ convention `<program>::<EventName>`).
- Data: the Borsh encoding of the event struct below.
- Emitted by the schedule actor (`TransactionEvent.account_id == vesting`), except `FeeChanged` (emitted by
  `vesting_fees`).
- No event contains a claim destination.

| Event | Emitted by | Fields (Borsh order) |
|---|---|---|
| `ScheduleCreated` | `Open` | `schedule: AccountId, schedule_id: [u8; 32], creator: AccountId, beneficiary: AccountId, asset: Asset, total: u128, kind: Kind, cancelable: bool, transferable: bool, cancel_authority: AccountId, milestone_authority: AccountId, refund_to: AccountId` |
| `Claimed` | claim | `schedule: AccountId, beneficiary: AccountId, amount: u128, at: u64, claimed_total: u128` (`at` = the effective time, section 4.1) |
| `Cancelled` | cancel | `schedule: AccountId, at: u64, vested_at_cancel: u128, refunded: u128, refund_to: AccountId` (`at` = the effective time) |
| `MilestoneSignalled` | signal | `schedule: AccountId, index: u8, amount: u128, signalled: u64` |
| `BeneficiaryTransferred` | transfer | `schedule: AccountId, from: AccountId, to: AccountId` |
| `MadeNonCancelable` | make non-cancelable | `schedule: AccountId, by: AccountId` |
| `FeeChanged` | `vesting_fees` | `old_rate_bps: u16, new_rate_bps: u16, treasury: AccountId, by: AccountId` |

Reading events back: LEZ v0.4.0-rc3 returns events with each executed transaction, and the indexer serves them
(`getEvents`). The rc3 sequencer's JSON-RPC has no event method, and a standalone sequencer stores events only for
blocks finalized through Bedrock, which a standalone sequencer never does. Tests therefore assert events at
executor level (the same execution the sequencer runs), and the SDK reads current state (section 11) where events
are not available.

---

## 8. Batch limit

A batch executes in one transaction and must fit the chain's execution budget: in the rc3 fee market a
transaction's gas limit may not exceed `MAX_GAS_EXEC`, and neither may the total of a block. A maximum-size batch
therefore fits only an otherwise empty block. Block size (transaction bytes per block) is not the binding limit.

The caps are two constants in `core/src/create.rs`: `MAX_BATCH_NATIVE = 164` and `MAX_BATCH_TOKEN = 57`. Each is
85% of the largest batch of linear schedules the chain accepts under the real budget, measured in LEZ's executor
with the released program (`program/tests/batch_limit.rs`, figures in `evidence/batch/max.txt`): 193 native
schedules are accepted and 194 refused; 67 token schedules are accepted and 68 refused. Heavier items fit fewer:
with 64 milestones per item and a creation fee, 61 native and 38 token schedules fit. A batch above its cap is
refused with 25; a batch under the cap that exceeds the budget is refused whole by the chain (out of gas) and
changes nothing. `docs/COSTS.md` lists the maxima, the cycles of a batch at each cap and its size in bytes.

The SDK splits a longer recipient list into several transactions, each filled to an estimated 85% of the budget and
never above the program's cap (per-item cycles by asset, milestone count and fee, fitted at or above the measured
batches), so a batch does not need an empty block. It sets each transaction's gas limit from a local dry run.

---

## 9. Privacy model

What is public and what is not, in the terms the RFP uses:

- **Recorded in public on-chain state:** every schedule parameter (asset, total, kind and times or milestone
  amounts, flags, authorities, creator, beneficiary, refund account), `claimed`, `last_time`, cancellation and
  milestone state, the escrow balance, and every event above. For each claim that means: its amount, its time, the
  schedule and the beneficiary.
- **Not recorded in public on-chain state, when claiming to a private account:** the destination private account
  and later movements of the claimed tokens.
- **Limitation:** the beneficiary address is permanently and publicly associated with the vesting schedule from
  the moment it is created. Every claim from the schedule is linked to that address in public on-chain state. The
  privacy property covers post-claim token movements only: the destination private account and subsequent activity
  are not recorded in public on-chain state, but the beneficiary's association with the vesting position is.
  Keeping the beneficiary address itself out of public on-chain state would require commitment-based beneficiary
  registration (depending on LP-0003), which is outside the scope of the RFP and of version 1.

How a private claim works:

- A claim to a private account is a `ClaimAsOf` in a privacy-preserving transaction proved by the claimant. Its
  root call `(beneficiary, vesting)` and its arguments are public: rc3 publishes the public root call of a
  privacy-preserving transaction and every public-to-private delivery. So `to` must not be the destination's
  canonical id.
- `to` is a **one-time blinded address**: `to = destination.blinded(opening)` (`AccountId::blinded` in LEZ), where
  `destination` is the canonical id of the private account that receives the claim and `opening` is 32 fresh random
  bytes chosen for this claim only. Inside the proof the claimant's private account opens the address
  (`PrivateWitness.address_openings`), so the credit lands on it in the same transaction. Neither the canonical id
  nor the opening appears in the transaction. An opening is never reused: two claims to the same blinded address
  would be linkable.
- The SDK takes the destination as a private account the claimant's wallet owns, never as a bare id, and derives
  `to` itself. Before building the transaction it refuses any destination that is not a private account the wallet
  owns, public or foreign (`NotPrivateAccount`). The program never learns whether `to` is private.
- The destination takes part in the proof and is credited in the same transaction. Claiming to a private account
  owned by someone else (a pending credit at the blinded address, collected later by its owner with the opening) is
  possible on the chain but is not offered by the SDK in version 1.

Network fee of a private claim:

- The network fee of a privacy-preserving transaction is a native transfer run after the root call, paid by a
  private account of the claimant whose balance after the root call covers it. A native claim into a fresh private
  destination can therefore pay its own fee out of the claimed amount, provided the claim is larger than the fee.
- A token claim credits tokens, not native balance, so it needs a private fee payer that already holds native
  balance, and a private holding of the definition at the destination, initialised in an earlier privacy-preserving
  transaction. Each is a transaction of its own.
- The SDK checks this by a local dry run before proving and returns an actionable error naming the account and the
  shortfall. No creator-funded fee float exists.

---

## 10. Interface description (IDL)

`idl/` writes `idl/vesting.json` from the `core/` types, and CI checks it is up to date. It covers:
- `Message` (section 3.1) with every variant and field, and the types it uses (`Terms`, `BatchTerms`, `BatchShape`,
  `BatchItem`, `Asset`, `Kind`);
- `Internal` (section 3.3), marked internal: clients never send it;
- `ScheduleState` and `ScheduleV1` with `Cancellation` and `Pending`, as the account layout of a schedule actor;
- every event with its selector (section 7);
- the error table (section 5);
- `FeeMessage` of `vesting_fees` (section 3.4).

`vesting` uses the raw entrypoint (section 1.4), so its IDL is written from `core/` rather than generated from the
guest's entrypoint. A test encodes one example of every message, state and event with `core/` and checks it against
the IDL's description, so the two cannot drift.

---

## 11. Reading state

Everything a client shows comes from current state and the rules in `core/`:

- **Schedule account:** derived from `(creator, schedule_id)` (section 1.2).
- **Schedule:** sequencer RPC `getAccountView` with the actor `(schedule, vesting)` returns its actor state;
  `ScheduleState::decode` reads it. Empty state: no schedule at this account.
- **Escrow balance:** `getAccountView` with `native(schedule)` (native) or `(schedule, token)` (token holding).
- **Now:** the timestamp of the last block (`getLastBlockId`, then `getBlock`), the time the clock reports to
  transactions in the next block (section 4.1).
- **Claimable now and next unlock:** `claimable(now)` and `next_unlock(now)` from `core/`, the same functions the
  program runs.
- **Listing schedules** by creator or beneficiary: a standalone rc3 sequencer serves no events (section 7), so the
  SDK scans blocks (`getBlockRange`) for root calls to `(x, vesting)` that carry `Create` or `CreateBatch`, derives
  each schedule account, and reads its current state (which holds the current beneficiary after any transfer).

---

## 12. Changelog

- **Version 1 (frozen).** Changes made before the freeze:
  - State: `last_time: u64` added to `ScheduleV1` after `claimed`. Claims and cancels are computed at
    `max(t, last_time)`; `claimable` floors at 0; `vested_at_cancel = max(vested_raw(te), claimed)`. Fixes a cancel
    that could revert after a `ClaimAsOf` one block ahead of the clock in the same block, and corrects the stated
    invariant to `claimed <= vested(last_time)`.
  - `next_unlock`: defined as the exact first time more becomes claimable, found by binary search, replacing a
    closed form that was wrong for very small totals.
  - `Terms.refund_to` and `BatchTerms.refund_to`: default is `source`, not the creator.
  - `Create` and `CreateBatch`: `treasury: AccountId` appended. `CheckFee`: `asset: Asset` appended. Token fees go
    to the treasury's associated token account, derived by `vesting` (`vesting_core::ata_seed`); `vesting` enforces
    the fee cap itself; `CheckFee` is answered only at the config actor and reads an empty config as rate 0;
    deployment is immutable and runs `Init`.
  - Error 26: was `UnsupportedTokenProgram`, which no message could trigger (`Asset::Token` names no program); now
    `PayoutToEscrow`, refusing `to` or `refund_to` equal to the schedule account. Error 23 is defined by dispatch
    (section 1.4). Error 25's message carries no numbers.
  - Events: `ScheduleCreated` gains `schedule_id`; `Claimed` gains `beneficiary`; `at` is the effective time.
  - Private claims: `to` is a one-time blinded address; the network fee text is corrected (a native claim pays its
    own fee); the limitation sentence is added.
  - Token destinations: holdings are initialised by their owner in an earlier transaction (one root call per
    transaction).
  - Batch caps: `MAX_BATCH_NATIVE` and `MAX_BATCH_TOKEN` are constants in `core/`, set from the measurement of the
    released program (`docs/COSTS.md`).
  - Added: time rules (4.1), cancel with nothing to refund, `Open` validation order, `CreateBatch` transaction
    shapes, the `MakeNonCancelable` decision, the IDL (10) and reading state (11).
- **Version 1, editorial (no interface change).** Section 8 states the final batch caps (164 native, 57 token, 85%
  of the measured maxima 193 and 67) and how the SDK fills a batch. Section 1.1 names the ids file
  `deploy/ids-<network>.toml`. Section 5 states that code 9 is a guard no well-formed transaction reaches.
  Section 9 states that the SDK claims only to private accounts the wallet owns; a claim to a foreign private
  account is not offered in version 1.
