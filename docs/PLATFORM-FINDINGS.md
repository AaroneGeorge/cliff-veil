# Platform findings on LEZ v0.4.0-rc3

What we had to learn about LEZ v0.4.0-rc3 (tag `v0.4.0-rc3`, commit `bafd07be0701398f9bc435f73ab05e9a7936c301`) before
the design in [SPEC.md](../SPEC.md) could be written, and what each finding changed. Each was first checked against
LEZ's own sources and a test program run through LEZ's executor and a dev-mode standalone sequencer, then re-checked
on the released program. Where the run logs of that early work are not part of this repository, the text says so
and points to the test or log in this repository that shows the same property on the released program. Paths in the
form `lee/...` and `lez/...` are in the LEZ repository at the tag above.

| # | Finding | Consequence in this repository |
|---|---|---|
| 1 | A handler learns only whether its own account authorised the transaction and which actor sent the message. | Every user message goes to the caller's own actor, which forwards to the schedule. |
| 2 | The clock is read by messaging the clock actor inside the same transaction; a transaction in block `n` sees the timestamp of block `n-1`. | Public `Claim` and `Cancel` read the clock; the private path uses `ClaimAsOf`; the schedule keeps a high-water time. |
| 3 | A public credit lands in the same transaction only if its recipient is declared and admitted; an owned private destination is credited in the same transaction. | The SDK declares every actor and adds admission evidence; a failed claim reverts as a whole. |
| 4 | The batch limit is the chain's 10,000,000-cycle execution budget, not transaction size. | Two measured caps, and SDK splitting by a cycle estimate. |
| 5 | A standalone sequencer cannot return program events. | Events are emitted as specified and asserted at executor level; the SDK reads state and scans blocks. |
| 6 | A token escrow can be a token holding owned by a program-derived account, and the token program accepts a debit authorised by the deriving program. | One escrow per schedule; no change to the token program. |
| 7 | The sequencer does not return a program's refusal text. | The SDK runs every transaction locally first and reads the refusal from that run. |

## 1. Caller authorisation goes through the caller's own actor

LEZ v0.4 runs handlers per *actor*, an account together with a program. A handler is told two things: whether the
account it runs on authorised the transaction (`ReceiveInput::is_authorized`), and which actor sent the message
(`ReceiveInput::from`). `is_authorized` is true when the account signed or when the program that owns a derived account
attached its seed (`lee/state_machine/core/src/execution_state.rs`, around lines 1431-1469). `from` is set by the
engine, so a program cannot forge it, and only `vesting` code running on account X produces a message from the actor
`(X, vesting)` (same file, around lines 1305-1314).

So `vesting` authenticates a caller in two hops. The transaction's root call is a message to the caller's own actor
`(caller, vesting)`, which checks `is_authorized` and forwards an internal message to the schedule actor. The schedule
accepts an internal message only when `from == (role holder, vesting)`, with the role holder (beneficiary, creator,
cancel authority, milestone authority) read from its own state. The same shape is used by `lez-admin-authority` for its
admin and by the token program for `Mint`. A message sent straight to a schedule (`from == None`), by another program,
or by a copy of the `vesting` bytecode deployed at another id is refused ([SPEC.md](../SPEC.md) sections 1.3 and 1.4).

A property of the platform to know about as a beneficiary: a signature authorises every actor of the signer's account
that the transaction declares, so any program in a transaction the beneficiary signs can send messages as
`(beneficiary, vesting)`. A beneficiary should sign only transactions whose root call and declared actors it has
checked. The SDK builds them.

New accounts need admission. LEZ v0.4.0-rc3 delivers a message to a public account only if the account exists, the
transaction carries admission evidence for it (`PublicAccountEvidence::Key` or `::Pda`), or the delivery carries a PDA
seed of the sending program (`execution_state.rs`, around lines 1190-1209). The first version of the test program failed
every creation for that reason. `Open` is therefore sent to a new schedule with its seed attached, and a transaction
that pays a never-seen public account carries `Key` evidence for it.

Evidence in this repository: `attack_stranger_claims`, `attack_message_straight_to_the_schedule`, `attack_forged_open`
and `attack_wrong_program_copy` in [program/tests/executor.rs](../program/tests/executor.rs), each with the refusal code
it expects; `soft_cancel_authority_cancels_and_the_creator_cannot` and `soft_milestone_authority_signals_and_the_creator_cannot`
in the end-to-end run ([evidence/e2e/local-dev/20261124T140628Z.json](../evidence/e2e/local-dev/20261124T140628Z.json)).

## 2. The clock is read inside one transaction, one block late

`vesting` uses the raw entrypoint because it must receive a reply. For a public claim or cancel the schedule actor
stores `pending = Claim { to }` (or `Cancel`) in its state and sends `ReadTimestamp` to the clock actor. LEZ delivers
messages depth first and the clock replies synchronously, so the reply reaches the schedule in the same transaction; the
handler sees `from == clock_actor()`, completes the operation and clears `pending`. Between transactions `pending` is
always empty.

The clock account is updated by the last transaction of each block, so every other transaction in block `n` reads
the timestamp of block `n-1` (`lez/programs/clock/core/src/lib.rs`). Two consequences:

- A claim made right after a block reads a timestamp one block old. "Claimable now" in the SDK and CLI is computed at
  the last block's timestamp, the time a transaction in the next block would see.
- A privacy-preserving claim takes minutes to prove. A clock read inside it would expire when the rounded value changed,
  so the private path uses `ClaimAsOf`: the time is an argument, and the program sets a validity window `[as_of, ∞)` on the
  transaction so the sequencer includes it only in a block whose timestamp is at least `as_of`.

Mixing the two time sources has a trap: a `ClaimAsOf` can pay at a time up to one block ahead of what a `Cancel` in the
same block reads. Without memory of the later time the cancel would compute a refund larger than the escrow holds and
revert. The schedule therefore keeps a high-water time `last_time`, and claims and cancels compute at
`max(t, last_time)` ([docs/DECISIONS.md](DECISIONS.md), decision 3).

Evidence: `f2_claim_as_of_window` and `f2_claim_as_of_then_cancel_in_the_same_block_uses_the_later_time` in
[program/tests/executor.rs](../program/tests/executor.rs); the cycle cost of the clock round trip is the difference
between `claim` (99,583 cycles) and `claim_as_of` (57,142 cycles) in [docs/COSTS.md](COSTS.md).

## 3. Async credit timing: when a payout lands

A payout is a debit of the escrow followed by an asynchronous credit message to the destination. What happens to the
credit depends on the destination:

- **Public destination, declared in the transaction.** LEZ promotes the credit into the same transaction, so the
  debit, the credit, the schedule's state write and the event are one atomic unit. A never-seen destination needs
  admission evidence (finding 1), or the whole transaction fails and the escrow is untouched.
- **Public destination, not declared.** The credit would stay a published asynchronous message, which needs a recovery
  binding; the transaction fails as a whole (`UnboundAsyncDestination`) with no debit.
- **Token destination without a holding.** The token program refuses the credit ("recipient has no token holding;
  initialize it first") and the whole claim reverts. Only the account itself can initialise its holding, and a
  transaction has exactly one root call, so a destination or refund account is initialised in an earlier transaction of
  its owner. The SDK checks and refuses with the action to take.
- **Owned private destination that takes part in the proof.** The prover chooses which asynchronous messages to promote
  (`lee/state_machine/src/privacy_preserving_transaction/circuit/mod.rs`), and the wallet credits an owned private
  destination in the same transaction. The first private claim into a fresh private account was run on a dev-mode
  sequencer; the final program's run is [evidence/cli/local-dev-lifecycle.log](../evidence/cli/local-dev-lifecycle.log).
- **Foreign private destination.** It receives a pending credit that its owner collects later with the opening of the
  one-time address (the claimant hands it over).

The network fee of a privacy-preserving transaction is a native transfer run after the root call, paid by a private
account of the claimant whose balance after the root call covers it. A native claim into a fresh private account can
therefore pay its own fee out of the claim, provided the claim is larger than the fee. In the lifecycle log the
estimated fee was 5,528,416 and the claim 100,006,667, leaving a private balance of 94,478,251. A token claim brings no
native balance, so it needs a private fee payer that already holds some.

Evidence: `f2_public_claim_pays_a_fresh_account_and_never_emits_it` and `r1_failed_claim_leaves_state` in
[program/tests/executor.rs](../program/tests/executor.rs); `r1_failed_claim_on_chain_changes_nothing_then_retry_pays`,
`f2_claim_private_to_a_blinded_destination` and `pr1_sdk_claims_to_a_public_and_to_a_private_account` in the end-to-end
run. The early test program's run of the first private claim into a fresh private account is not in this repository.

## 4. The batch limit is the execution budget

`CreateBatch` sends one `Open` and one funding transfer per item, each to its own schedule account and escrow, in one
transaction. The binding limit is the chain's execution budget: in the LEZ fee market a transaction's gas limit may not
exceed `MAX_GAS_EXEC`, which is also the total for a block (`lez/programs/fee/core/src/market.rs`,
`validity.rs`). That is 10,000,000 cycles ([docs/COSTS.md](COSTS.md)). Block size, counted in transaction bytes, is not
the limit. A batch near the limit fills a block alone.

We measured the largest accepted batch of the released program under the real budget, with the next size refused as out
of gas ([evidence/batch/max.txt](../evidence/batch/max.txt), test `program/tests/batch_limit.rs`):

| Items | Accepted | Refused |
|---|---|---|
| Native, linear | 193 | 194 |
| Token, linear | 67 | 68 |
| Native, 64 milestones each, with a creation fee | 61 | 62 |
| Token, 64 milestones each, with a creation fee | 38 | 39 |

The program's caps are 85% of the linear maxima: 164 native and 57 token (`MAX_BATCH_NATIVE`, `MAX_BATCH_TOKEN`), a batch
above its cap being refused with error 25. A batch of 164 native schedules costs 8,515,387 cycles in the cost run
([docs/COSTS.md](COSTS.md)), and one of 165 is refused. The heaviest items fit fewer, so the SDK splits a recipient list
by an estimate of the cycles each transaction needs and fills each up to 85% of the budget (`sdk/src/batch.rs`), then
dry-runs every transaction before sending. The documented maximum for one transaction is therefore 164 native or 57 token
schedules of the linear kind, and fewer with many milestones (the table). The early test program with a smaller state
and event measured much higher maxima, which is why the caps were not set from it.

## 5. Events cannot be read back from a standalone sequencer

The program emits LP-0012 events ([docs/EVENTS.md](EVENTS.md)). Reading them back:

- Executor level: running a transaction through LEZ's `ValidatedStateDiff` returns the events with their selector and
  emitting account. This is the execution the sequencer itself runs, and it is what the tests assert
  (`f6_events_every_event_with_its_selector_and_schema` in [program/tests/executor.rs](../program/tests/executor.rs)).
- Sequencer RPC: the rc3 sequencer's JSON-RPC (`lez/sequencer/service/rpc/src/lib.rs`) has no event method. A call to
  `getEvents` on our local sequencer returns `Method not found` (error code -32601).
- Why: the sequencer stores a block's events only for blocks applied as finalized from the Bedrock channel
  (`lez/sequencer/core/src/lib.rs`). A standalone sequencer runs against a mock Bedrock that never finalizes anything,
  so no events past genesis are stored. `getEvents` exists on the indexer's RPC (`lez/indexer/service/rpc/src/lib.rs`),
  which follows Bedrock.

Consequences: events are emitted exactly as specified and tested at executor level; the end-to-end tests on the
standalone sequencer assert state (schedule, escrow, balances), not events; the SDK reads current state, and lists
schedules by scanning blocks for creation root calls instead of reading `ScheduleCreated`
([SPEC.md](../SPEC.md) section 11). Event read-back through the indexer is for a Bedrock-backed deployment and has not
been run.

## 6. Token escrow on a program-derived account

A schedule's account is derived from the creator and the schedule id (`seed = SHA-256("cliff-veil/schedule/v1" ‖
creator ‖ schedule_id)`, `AccountId::for_public_pda(vesting, seed)`). No key exists for it; only `vesting` can authorise
a debit, by attaching the seed. For a token schedule the escrow is a token holding at that account:

- At creation `vesting` sends the token program `InitializeHolding { definition }` to `(schedule, token)` with the
  seed attached (the seed authorises the empty holding), and then a `Transfer` from the creator's holding into it.
- At claim and cancel `vesting` sends `Transfer` from `(schedule, token)` with the seed attached, and the token
  program accepts the seeded debit.

This needs no change to the token program, which we build from lez-programs at the pinned commit
([docs/REUSE.md](REUSE.md)). Two constraints of the token program shape the interface: a holding is one definition per
account, and a credit lands only on an existing holding of the same definition. So a refund account defaults to the
funding account (decision 4), a creation fee in a token goes to the treasury's associated token account (decision 7),
and the token program is a build-time constant of `vesting`, so no message can name another token program.

Evidence: `f1a_cliff_then_linear_lifecycle_token` and `f1c_milestones_lifecycle_token` in
[program/tests/executor.rs](../program/tests/executor.rs); `f1_escrow_token_holds_exactly_the_total` in the end-to-end
run. Cycle figures for token operations on a sequencer are not measured yet ([docs/COSTS.md](COSTS.md)).

## 7. The sequencer does not return a program's refusal text

When a program refuses, it panics with `cliff-veil error <code>: <message>`. Where a client can read that text:

| What was sent | Where the text appears |
|---|---|
| A public transaction, through LEZ's wallet with default settings | In the wallet's local simulation, before anything is sent: the error carries `Guest panicked: <panic text>`. |
| The same transaction with the wallet told to send reverting transactions | Nowhere. `sendTransaction` returns a hash; the block records only the index of the reverted transaction (`BlockBody.reverted`); the fee is charged; there is no receipt or reason method. |
| A privacy-preserving transaction | While the wallet builds the proof locally (the program runs inside it); nothing reaches the sequencer. |

The reasons, in LEZ's sources: `sendTransaction` checks only size, the stateless check and the fee screen, so a program
refusal can never come back as an RPC error (`lez/sequencer/actors/rpc_server/src/actor/service.rs`); a charged public
transaction whose action fails is included as reverted and charged (`lez/chain_state/src/apply.rs`); the block stores
only `reverted: Vec<u64>` (`lez/common/src/block.rs`). On our local sequencer a reverted claim is included, charges its
fee and changes nothing else (`r1_failed_claim_on_chain_changes_nothing_then_retry_pays` in the end-to-end run).

A gap: the wallet's simulation does not check validity windows. A `ClaimAsOf` whose `as_of` is ahead of the chain
clock passes the simulation and would be included as a silent, charged revert.

So the SDK:

1. runs every transaction locally against current state before sending it, parses the refusal with
   `vesting_core::parse_code` (it matches the `cliff-veil error <n>:` prefix, because an assertion's panic text carries
   extra lines), and reports the message and an action; it never sets the wallet's send-reverting option;
2. checks `as_of` against the latest clock timestamp itself before sending a `ClaimAsOf`;
3. for a privacy-preserving transaction, lets the failure surface while the proof is built, before anything is sent.

The CLI shows this: a claim before the cliff prints `cliff-veil error 14: nothing claimable yet; next unlock at ...` and
sends nothing ([evidence/cli/local-dev-lifecycle.log](../evidence/cli/local-dev-lifecycle.log)). The early run that showed
each case in the table is not part of this repository.
