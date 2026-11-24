# Composition: a launchpad that holds the milestone authority

RFP-017's soft requirements name the case this document covers: creator collateral from a token launch
(RFP-015 bonding curve, RFP-016 LBP) vesting by milestones as buyer protection. If the creator held the milestone
authority it could approve every milestone at once and drain the collateral. The authority has to sit with a
governance body or multisig that excludes the creator.

`vesting` already lets the creator name a separate milestone authority and cancel authority (SPEC 3.1, `Terms`).
This document shows the next step: the authority is **a program**, and the schedule is **created by that
program**, not by a wallet. The reference is the example program `launchpad_escrow`
(`examples/launchpad_escrow`). It is small (one source file) and is not part of the `vesting` deployment.

## What the example does

| Step | Who sends it | What happens on chain |
|---|---|---|
| `Open` | the project | The launchpad writes the launch (approver set, threshold `M`, refund account) at a new account it derives, then creates a milestone schedule in `vesting` **from that account**: creator, milestone authority and cancel authority are all the launch account; the beneficiary is the project; the collateral is debited from the project's account into the schedule's escrow. One transaction. |
| `Approve { launch, index }` | an approver | Records the approval. The `M`-th distinct approval of a milestone signals it to `vesting` in the same transaction. |
| `VoteCancel { launch }` | an approver | Records the vote. The `M`-th distinct vote cancels the schedule in `vesting` in the same transaction: the unvested collateral goes to the refund account, the vested part stays claimable. |
| `Claim` (a `vesting` message) | the project | The project claims what the approvers have released, from `vesting` directly, as any beneficiary does. |

What the project cannot do, and the exact refusals (shown by the tests listed below):

| Attempt by the project | Refused by | Code |
|---|---|---|
| `SignalMilestone` straight to `vesting` | `vesting` | `cliff-veil error 6` (only the milestone authority may signal) |
| `Cancel` straight to `vesting` | `vesting` | `cliff-veil error 5` (only the cancel authority may cancel) |
| `MakeNonCancelable` straight to `vesting` | `vesting` | `cliff-veil error 4` (only the creator or the cancel authority) |
| `Approve` or `VoteCancel` through the launchpad | `launchpad_escrow` | `launchpad-escrow error 10` (not an approver) |
| naming itself as an approver or as the refund account at `Open` | `launchpad_escrow` | `launchpad-escrow error 4`, `6` |

## Why a program can be the creator

LEZ authenticates a caller by `is_authorized` at the caller's own actor (SPEC 1.3). An account is authorised for a
delivery when it signed the transaction **or** when the delivery carries a PDA seed of the sending program that
derives that account. So a program can act as an account it derived itself, and no one else can, because the
account has no private key.

The launch account is `AccountId::for_public_pda(launchpad, SHA-256("launchpad-escrow/launch/v1" ‖ project ‖
launch_id))`. Whenever the launchpad talks to `vesting` it sends the message to `(launch, vesting)`, the launch
account's seat under `vesting`, with that seed attached. `vesting` sees an authorised caller `launch`, exactly as
if a wallet had signed, and routes the message as usual (SPEC 3.3): the schedule accepts `SignalFor` only from
`(milestone_authority, vesting)` and `CancelFor` only from `(cancel_authority, vesting)`, which is
`(launch, vesting)`. `vesting` needs nothing special for program callers; nothing in it was changed for this.

The project's own signature authorises the project's account in the same transaction, so `vesting` can debit the
collateral from it (`source = project`).

## Accounts and actors

| Actor | Account | State |
|---|---|---|
| seat under the launchpad | `(caller, launchpad)` | always empty; checks the caller signed and forwards |
| launch | `(launch, launchpad)` | `LaunchState`: project, schedule, refund account, approvers, threshold, approvals per milestone, signalled bits, cancel votes, cancelled |
| launch seat under `vesting` | `(launch, vesting)` | always empty (a `vesting` caller actor) |
| schedule | `(schedule, vesting)`, `schedule = for_public_pda(vesting, schedule_seed(launch, launch_id))` | `ScheduleV1` (SPEC 2) |
| escrow | `native(schedule)` | the collateral |

## Transaction shapes

Declared actors, root first. `launchpad` and `vesting` are program ids; `fees_config` is the `vesting_fees` config
account (SPEC 3.2).

| Operation | Signer | Declared actors |
|---|---|---|
| `Open` | project | `(project, launchpad)`, `(launch, launchpad)`, `(launch, vesting)`, `(schedule, vesting)`, `native(project)`, `native(schedule)`, `(fees_config, vesting_fees)` |
| `Approve` | approver | `(approver, launchpad)`, `(launch, launchpad)`, `(launch, vesting)`, `(schedule, vesting)` |
| `VoteCancel` | approver | as `Approve`, then the clock actor, `native(schedule)`, `native(refund_to)` |
| `Claim` | project | the `vesting` shape of SPEC 3.2 (root `(project, vesting)`) |

`Approve` and `VoteCancel` always declare the `vesting` actors, because the client cannot know whether another
approval lands first and makes this one the `M`-th. Below the threshold they are declared and not reached.

Messages are Borsh enums (`examples/launchpad_escrow/src/lib.rs`): `LaunchMessage::Open` (tag 0: `launch_id`,
`amounts`, `approvers`, `threshold`, `refund_to`, `fee`, `treasury`), `Approve` (tag 1: `launch`, `index`),
`VoteCancel` (tag 2: `launch`). `fee` and `treasury` are passed to `vesting`'s `Create` unchanged, so a launch
works with the fee switch on (the client quotes them as for any creation, SPEC 6).

## What is enforced on chain, and what is not

Enforced by `vesting` (whatever program sits in front of it):
- only `(milestone_authority, vesting)` signals and only `(cancel_authority, vesting)` cancels; these are fixed at
  creation and no message changes them;
- `MakeNonCancelable` only by the creator or the cancel authority, here both the launch account, and the
  launchpad never sends it: the collateral stays cancelable;
- the schedule is not transferable; claims go to the beneficiary only; amounts and refunds as SPEC 4.

Enforced by `launchpad_escrow`:
- the approver set (1 to 16 distinct accounts, never the project) and `M` (1 to the set size) are fixed at `Open`
  and visible in the launch state;
- one approval per approver per milestone, one cancel vote per approver; the `M`-th signals or cancels in the same
  transaction; nothing after a cancel;
- the refund account is not the project;
- the `vesting` program it calls is a build input (`CV_VESTING_PROGRAM_ID`), so a launch cannot be pointed at a
  look-alike program.

Not enforced, and why:
- **Who the approvers are.** In this example the project names the set at `Open`. The program refuses the project
  itself, but it cannot know whether an approver is independent of the project. Buyers, or the launchpad's front
  end, check the set (it is public in the launch state) before buying. A production launchpad takes the set from
  its own governance configuration instead (for example an RFP-001 admin or a council account fixed at
  deployment); only `LaunchState::new` changes.
- **What an approver's signature covers.** The platform property of SPEC 1.3 applies to approvers too: a
  signature authorises every actor of the signer's account declared in the transaction, so any program inside a
  transaction an approver signs can send messages as `(approver, launchpad)` and cast that approver's approval or
  cancel vote. An approver signs only transactions whose root call and declared actors it has checked.
- **Whether `refund_to` can be paid.** The program checks only that `refund_to` is not the project. LEZ pays only an
  account that exists or that the transaction admits (SPEC 1.2, admission of new accounts; the client adds the
  account's key as evidence). A `refund_to` that does not exist and that no client can admit (an id with no known key
  or PDA derivation behind it), or, with token collateral (below), one without a holding of the token until its owner
  initialises one (SPEC 3.1, payout destinations), cannot be paid: the vote that reaches the threshold reverts with
  the refund, so the launch cannot be cancelled and the unvested collateral stays in the schedule. It is never paid to
  the project (executor test `soft1_compose_a_cancel_reverts_while_the_refund_account_cannot_be_paid`). Buyers check
  `refund_to`, which is public in the launch state, as they check the approvers.
- **What happens to a refund.** Refunds go to `refund_to`, named at `Open`. Distributing it to buyers pro rata is
  the launchpad's own logic and is out of scope here.
- **Native collateral only.** The example locks the native token. A token launch adds the token actors of
  SPEC 3.2 to `Open` and sets `Asset::Token`; nothing else changes.

## Integrating it into an RFP-015 or RFP-016 launchpad

1. At sale close (graduation, or the end of an LBP) the sale program, not the creator, sends `Create` to
   `(sale_pda, vesting)` with its seed, as `launchpad_escrow` sends it from `(launch, vesting)`. Creator and
   both authorities are the sale's PDA; the beneficiary is the project; `refund_to` is the sale's refund pool.
2. The sale program's own rule decides when it signals or cancels: an approval threshold as here, a time rule, or an
   oracle. Whatever the rule, it sends `SignalMilestone` or `Cancel` to `(sale_pda, vesting)` with its seed.
3. The project claims from `vesting` like any beneficiary, with the SDK or the CLI (`cliff-veil claim`); a private
   claim works the same way (SPEC 9), since the beneficiary is an ordinary account.

When the collateral is held by the sale program's PDA rather than by the project's wallet, `source` is that PDA:
the seed attached to `Create` authorises the account for the whole delivery, including `vesting`'s debit of its
balance. This variant is described from the LEZ authorisation rules; the tests here fund from the project's account.

## Tests and evidence

Executor level (the real guests of `vesting`, the token program, `vesting_fees` and `launchpad_escrow`, run by
LEZ's public execution as a sequencer runs a block), `program/tests/compose.rs`:

| Test | Shows |
|---|---|
| `soft2_compose_a_launch_program_pda_creates_the_schedule_and_holds_both_authorities` | creator, milestone and cancel authority are the launch account; escrow funded from the project; `ScheduleCreated` names the launch account |
| `soft2_compose_the_project_cannot_signal_cancel_or_freeze_its_own_collateral` | codes 6, 5 and 4 from `vesting`; launchpad code 10 for the project and a stranger |
| `soft2_compose_m_of_n_approvals_signal_then_the_project_claims` | 1 of 2 approvals signals nothing (claim refused, 14); the 2nd signals; the project claims exactly that milestone |
| `soft1_compose_cancel_only_by_the_launch_rule_refunds_the_unvested_collateral` | the project's cancel refused (5); the 2nd vote cancels; refund of the unvested part; vested part claimed after |
| `compose_open_refusals_carry_their_codes` | launchpad codes 4, 5, 6, 7 at `Open` |
| `compose_a_launch_accepts_messages_only_from_seats` | an internal message sent straight to a launch is refused (3) |

On a standalone LEZ v0.4.0-rc3 sequencer (dev mode), `e2e/tests/compose.rs`, against the `launchpad_escrow`
deployment in `deploy/ids-local-dev-compose.toml` and the `vesting` of `deploy/ids-local-dev.toml`:

| Test | Shows |
|---|---|
| `soft2_compose_launch_pda_holds_the_milestone_authority_and_m_of_n_approvals_pay_the_project` | the schedule is created by the launch PDA; the project's direct `SignalMilestone` refused with code 6; one approval leaves nothing claimable (14); the second signals and the project claims |
| `soft1_compose_only_the_launch_rule_cancels_and_refunds_the_unvested_collateral` | the project's direct `Cancel` refused with code 5; one vote cancels nothing; the second cancels and refunds the unvested part |

Results, transaction hashes and blocks: `evidence/e2e/compose/` (the run of 2026-11-24 refunded 700000 of a
1000000 collateral after one 300000 milestone was signalled and claimed).

Run them:

```bash
make test-executor                       # executor tests, compose.rs included
scripts/seq.sh dev start && scripts/deploy_local.sh dev
e2e/deploy_compose.sh                    # builds the example against that vesting and deploys it
e2e/run.sh soft2_compose_launch_pda_holds_the_milestone_authority_and_m_of_n_approvals_pay_the_project \
           soft1_compose_only_the_launch_rule_cancels_and_refunds_the_unvested_collateral
```

The example is built with the local RISC Zero toolchain; it is not in the reproducible release set of
`program/IMAGE_IDS.expected`, and its image id in `deploy/ids-local-dev-compose.toml` depends on the build machine.
