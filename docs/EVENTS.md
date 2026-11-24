# Events

The programs emit one LEZ `ProgramEvent` (LP-0012) for each state transition of a schedule, and one when the fee
configuration changes. The definitions are in [core/src/events.rs](../core/src/events.rs); the same schema, with the
selectors, is in [idl/vesting.json](../idl/vesting.json) ([docs/IDL.md](IDL.md)); the interface text is
[SPEC.md](../SPEC.md) section 7.

## Format

A `ProgramEvent` is `{ selector: [u8; 8], data: Vec<u8> }`.

- **Selector:** the first 8 bytes of `SHA-256("vesting::<EventName>")`, the LEZ convention `<program>::<EventName>`.
  `FeeChanged` is emitted by `vesting_fees` but uses the `vesting::` prefix too.
- **Data:** the Borsh encoding of the event struct below, fields in the order listed. Integers are little-endian;
  `u128` is 16 bytes; an `AccountId` is 32 bytes; `[u8; 32]` is 32 bytes with no length prefix; `bool` is one byte.
  `Asset` is a tag byte (`0` native, `1` token followed by the 32-byte definition account). `Kind` is a tag byte: `0`
  `Linear { start: u64, cliff: u64, end: u64 }`, `1` `Milestones { amounts: Vec<u128>, signalled: u64 }` (a `u32` count,
  then the amounts).
- **Emitted by:** the schedule actor, so the `TransactionEvent` of the executed transaction has
  `account_id == <vesting program id>`. `FeeChanged` is emitted by `vesting_fees` (`account_id == <vesting_fees id>`).
- **Never in an event:** a claim's destination. `Claimed` carries the beneficiary, which is already public in the
  schedule state, and never `to`.
- An event exists only if its transaction succeeded: a refused or reverted transaction emits nothing, and its state
  changes are discarded with it.

## Schema

| Event | Selector (hex) | Fields, in Borsh order | Emitted when |
|---|---|---|---|
| `ScheduleCreated` | `68ee839f2ce9efac` | `schedule: AccountId`, `schedule_id: [u8; 32]`, `creator: AccountId`, `beneficiary: AccountId`, `asset: Asset`, `total: u128`, `kind: Kind`, `cancelable: bool`, `transferable: bool`, `cancel_authority: AccountId`, `milestone_authority: AccountId`, `refund_to: AccountId` | A schedule is opened by `Create` or `CreateBatch` (`Open` at the schedule). A batch emits one per item. `cancel_authority`, `milestone_authority` and `refund_to` are the resolved values (defaults filled in). |
| `Claimed` | `aa25b4641ce8cb29` | `schedule: AccountId`, `beneficiary: AccountId`, `amount: u128`, `at: u64`, `claimed_total: u128` | A claim pays out: `Claim` or `ClaimAsOf`, to a public or a private destination. `at` is the effective time the amount was computed at, `max(clock time or as_of, last_time)`, in milliseconds, not the time of inclusion. `claimed_total` is `claimed` after this claim. A claim with nothing claimable is refused (error 14) and emits nothing. |
| `Cancelled` | `fee82a2f47263d1f` | `schedule: AccountId`, `at: u64`, `vested_at_cancel: u128`, `refunded: u128`, `refund_to: AccountId` | A cancelable schedule is cancelled. `at` is the effective time of the cancel. `vested_at_cancel` is the vested amount, frozen; `refunded` is `total - vested_at_cancel`, paid to `refund_to`. When it is 0 the cancel is still valid and no transfer is made. Vested and unclaimed tokens stay claimable. |
| `MilestoneSignalled` | `0882439381b3eec9` | `schedule: AccountId`, `index: u8`, `amount: u128`, `signalled: u64` | The milestone authority signals milestone `index`. `amount` is what it unlocked; `signalled` is the bitmap after the signal (bit `i` is milestone `i`). Signalling the same index again is refused (error 18) and emits nothing. No tokens move at this event. |
| `BeneficiaryTransferred` | `3edd69d43332bdb2` | `schedule: AccountId`, `from: AccountId`, `to: AccountId` | The beneficiary of a transferable schedule hands it to a new beneficiary. |
| `MadeNonCancelable` | `947bede70fe02f82` | `schedule: AccountId`, `by: AccountId` | The creator or the cancel authority makes the schedule non-cancelable. `by` is the one who did it. |
| `FeeChanged` | `14e7602077f31973` | `old_rate_bps: u16`, `new_rate_bps: u16`, `treasury: AccountId`, `by: AccountId` | The admin of `vesting_fees` changes the creation-fee rate or treasury with `SetFee`. `by` is the admin; `treasury` is the treasury after the change. |

A schedule's creation fee is not an event; the fee is a transfer in the creation transaction and the rate in force is
readable from the `vesting_fees` configuration (`cliff-veil fee-show`).

## Where this is tested

- `f6_events_every_event_with_its_selector_and_schema` in [program/tests/executor.rs](../program/tests/executor.rs)
  runs the real guest through LEZ's `ValidatedStateDiff` and asserts each event's selector, its emitting account and its
  decoded fields.
- `f6_event_roundtrip_and_selectors` in [core/src/events.rs](../core/src/events.rs) recomputes every selector from its name
  and round-trips each struct.
- `u6_idl_matches_program_events` in [idl/tests/u6_idl.rs](../idl/tests/u6_idl.rs) checks the IDL entry of each event
  (name, selector, payload) against the program's constants.

## Reading events back

A client of a standalone LEZ v0.4.0-rc3 sequencer cannot read these events: its JSON-RPC has no event method, and it
stores events only for blocks finalized through Bedrock, which a standalone sequencer never does
([docs/PLATFORM-FINDINGS.md](PLATFORM-FINDINGS.md), finding 5). The events are returned with each executed transaction
at executor level, and the indexer's `getEvents` serves them for a Bedrock-backed deployment, which we have not run. The
SDK and CLI therefore read schedule state (`claimed`, `cancelled`, `signalled`, the current beneficiary) and list
schedules by scanning blocks.
