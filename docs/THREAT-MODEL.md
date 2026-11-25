# Threat model

What cliff-veil protects, who it trusts for what, how each known attack is stopped, the test or record that shows it,
and what risk remains. It covers the `vesting` and `vesting_fees` programs, the rules crate (`core/`), the SDK and its
C ABI, and the read-only public endpoint, as built against LEZ v0.4.0-rc3. The interface it refers to is
[SPEC.md](../SPEC.md); the reasons behind the design are in [DECISIONS.md](DECISIONS.md) and the platform facts in
[PLATFORM-FINDINGS.md](PLATFORM-FINDINGS.md).

**No schedule should hold real value until the external audit is closed.** Nothing here has had a third-party audit.
Every test named below can be run from this repository; executor tests need the guests built (see the README's test
table).

Test locations used below:

| Short name | File |
|---|---|
| executor | `program/tests/executor.rs` (the real `vesting`, token and fee guests run by LEZ's executor) |
| compose | `program/tests/compose.rs` |
| fees executor | `program/fees/tests/executor.rs` |
| rules | `core/src/rules.rs`, `core/src/create.rs`, `core/src/fees.rs` (unit tests) |
| properties | `core/tests/properties.rs` |
| e2e | `e2e/tests/requirements.rs`, `e2e/tests/fees_on.rs`, `e2e/tests/compose.rs` (standalone sequencer) |
| sdk | `sdk/src/tests.rs` |
| ffi | `sdk/ffi/src/tests.rs` |
| endpoint | `tools/public_endpoint/test_policy.py` |

## 1. Assets

| Asset | Where it lives | What losing it means |
|---|---|---|
| Escrowed funds | The schedule's own account: a native balance, or a holding of the pinned token program. No private key exists for it; only `vesting` can debit it, by attaching the schedule's seed (SPEC 1.2) | Theft, or funds locked for good |
| Claim rights | `beneficiary` in the schedule state; it changes only by `TransferBeneficiary`, and only when `transferable` | Someone else collects the vested amount |
| Creator's rights | `cancel_authority`, `milestone_authority`, `refund_to`, all fixed at creation; `cancelable`, which can only go from true to false | Early release, a cancel that should not happen, or a refund paid to the wrong account |
| Fee configuration | The `vesting_fees` config account: rate (0 to 100 basis points) and treasury, changed by its admin | Overcharging, or creation stalled |
| Privacy of the destination | The private account a private claim pays, and what its owner does afterwards | The link between a beneficiary and where the claimed tokens went |

## 2. Actors and trust

| Actor | Can | Cannot | Trusted for |
|---|---|---|---|
| Creator | Create schedules in its own namespace (the schedule account is derived from the creator's id); `MakeNonCancelable`; by default it is also the cancel authority and the milestone authority | Claim (3); change the beneficiary, the terms, the authorities or `refund_to` after creation; debit an escrow | Choosing honest authorities and a refund account that can be paid |
| Beneficiary | Claim what has vested, to a public or a private account; transfer the right when the schedule is transferable | Claim more than vested; cancel (5); signal (6) | Nothing beyond its own position |
| Cancel authority | `Cancel` (unvested part to `refund_to`, vested part stays claimable); `MakeNonCancelable` | Choose where the refund goes; touch the vested part; claim | Not cancelling without cause |
| Milestone authority | `SignalMilestone` for an index, once (18) | Move tokens itself; take back a signal; claim | Signalling only milestones that were met |
| Fee admin (`vesting_fees`) | Set the rate (0 to 100 basis points) and the treasury; hand over in two steps; renounce | Raise the rate above 1% (28 in both programs); touch an existing schedule; change `vesting` | Not stalling new creations |
| Sequencer operator | Order, delay or drop transactions; set block timestamps, which are the chain clock `vesting` reads | Sign for an account; forge a proof when the sequencer verifies proofs (dev mode unset) | Liveness, fair ordering, honest time |
| Prover / runner of a private claim | Sees the whole witness: the claimant's keys, the private destination and the opening | Nothing on chain without the claimant's keys | Privacy of the claim, and the keys it holds |
| Public endpoint operator | Answer read requests, or answer them falsely; go offline | Send a transaction through the endpoint (it forwards reads only) | Honest reads, for clients that use it |

## 3. Attack surface

| # | What an attacker tries | How the design stops it | Test or evidence | Residual risk |
|---|---|---|---|---|
| A1 | A stranger claims a schedule's vested funds, through its own seat or through the beneficiary's seat without the beneficiary's signature | Every user message runs at the caller's own seat `(caller, vesting)`, which requires `is_authorized` (1); the schedule accepts `ClaimFor` only from `(beneficiary, vesting)` (3) (SPEC 1.3) | executor `attack_stranger_claims` (3 for the stranger, 1 without the signature, 3 for the creator; escrow unchanged) | None known |
| A2 | A transaction or another program sends a message straight to the schedule account | A schedule (non-empty state) refuses any user-level message with 23; an `Internal` message sent by a transaction decodes as a user message and is refused the same way (SPEC 1.4) | executor `attack_message_straight_to_the_schedule` | None known |
| A3 | A forged `Open` overwrites a schedule (new beneficiary) or opens an unfunded one at a fresh address | `Open` is never built from a user message; it is accepted only from the creator's seat, at the derived address, on an empty actor (7, 9); a transaction's `Open` is decoded as a user message at an account it cannot sign for | executor `attack_forged_open` (23 on the existing schedule, 1 or 2 on the fresh one, no state written) | None known |
| A4 | The same bytecode deployed at another id claims from a real schedule | A copy is a different program: its seat forwards to `(schedule, copy)`, which holds no schedule (8) | executor `attack_wrong_program_copy` | None known |
| A5 | A private claim pays at a time that has not happened (`ClaimAsOf` with a future `as_of`) | `ClaimAsOf` sets the transaction's validity window to `[as_of, ∞)`, so the sequencer includes it only in a block whose timestamp is at least `as_of` (DECISIONS 6) | executor `attack_as_of_in_the_future_refused` (`OutOfValidityWindow`); sdk `u7_a_private_claim_ahead_of_the_clock_is_not_sent` | Time is the chain's clock; see A20 |
| A6 | A beneficiary blocks a cancel by sending a `ClaimAsOf` one block ahead of the clock in every block, so that the cancel computes a vested amount below what was claimed and its refund reverts | `last_time`: claims and cancels compute at `max(t, last_time)`, and `vested_at_cancel = max(vested_raw, claimed)`; the invariant `claimed <= vested(last_time)` holds (DECISIONS 3) | executor `f2_claim_as_of_then_cancel_in_the_same_block_uses_the_later_time`; properties `prop_f2_one_time_source_under_any_interleaving`, `prop_f3_cancel_conserves_value_for_any_time_order`; rules `f2_claim_as_of_an_earlier_time_is_computed_at_the_last_time` | None known |
| A7 | Double claim: claim the same vested amount twice, in two transactions or inside one | `claimed` is written before the payout; the next claim pays `vested - claimed`, and 14 when that is zero; a second operation inside the same transaction finds `pending` set (21); a refusal reverts the whole transaction | rules `r1_second_operation_while_pending_is_21`, `r1_refused_claim_changes_nothing_and_can_be_retried`; properties `prop_f1a_claims_sum_to_total`; executor `r1_failed_claim_leaves_state`; e2e `r1_failed_claim_on_chain_changes_nothing_then_retry_pays` | None known |
| A8 | Batch funding mismatch: a batch whose escrows hold less than their totals, a duplicate id counted twice, or a sum that wraps | Duplicate ids refused (7), the sum computed with checked arithmetic (29), each item funded by its own transfer of exactly its total into its own escrow, any bad item refuses the whole batch, and an over-budget batch reverts whole | rules `f5_duplicate_schedule_id_in_batch_is_7`, `f5_batch_total_overflow_is_29`, `f5_bad_item_refuses_the_whole_batch`; executor `f5_batch_native_gives_each_item_its_own_escrow`, `f5_batch_token_gives_each_item_its_own_escrow`, `f5_batch_duplicate_id_refused`, `r2_batch_over_the_budget_reverts_whole`; properties `prop_f5_batch_milestone_amounts_sum_to_total` | None known |
| A9 | Name someone else's account, or another schedule's escrow, as the funding `source` | The funding transfer from `source` carries no seed, so it succeeds only if `source` authorised the transaction (`open_and_fund` in `program/guest/src/lib.rs`) | Code reading; no dedicated test. Listed for the audit package | An auditor should confirm with a test |
| A10 | Pay a claim or a refund into the schedule's own escrow, burning the entitlement | `to` and `refund_to` equal to the schedule account are refused (26), on `Claim`, `ClaimAsOf` and at creation (DECISIONS 14) | executor `attack_payout_to_the_escrow_refused` | None known |
| A11 | Make a cancel impossible by giving it a refund account that cannot be paid | `refund_to` defaults to `source`, the account that funded the escrow and so already holds the asset (DECISIONS 4). It is fixed at creation; nobody else can close a holding | executor `f3_cancel_refunds_an_explicit_refund_account`, `r2_cancel_atomic` (an unpayable refund reverts the whole cancel); e2e `f3_token_cancel_refunds_the_unvested_part_to_the_refund_holding`, `r2_failed_cancel_on_chain_keeps_the_prior_state` | A creator who names an explicit `refund_to` that is never initialised for the asset cannot cancel; the vested part stays claimable. The SDK shows `refund_to` in the cancel summary |
| A12 | Fee path: charge more than 1%, charge a fee the creator did not quote, or send it to another account | Cap enforced twice: `vesting_fees` refuses a rate above 100 basis points and `vesting` refuses a quoted fee above `floor(total * 100 / 10,000)` before it asks (28). `CheckFee` compares the quoted fee and treasury with the config (27). It is answered only at the config actor, so another actor of the program cannot read as rate 0 | rules `fee_switch_capped`, `vesting_enforces_the_cap_itself`; fees executor `fee_set_fee_above_the_cap_is_refused`, `fee_wrong_fee_or_treasury_is_refused`, `fee_check_is_answered_only_at_the_config_account`, `fee_set_fee_by_a_non_admin_is_refused`; executor `fee_mismatch_refused_by_the_fee_program`; e2e `fee3_fee4_switch_is_admin_only_capped_and_a_stale_quote_is_refused` | A schedule of fewer than 100 units pays no fee at 1% (floor) |
| A13 | Token fee sent to an account the treasury does not control | `vesting` derives the treasury's associated token account itself from the checked treasury and the token definition (`ata_seed`), with the ATA program id fixed at build time | rules `ata_seed_hashes_program_owner_definition_in_order`; executor `fee1_fee2_fee4_token_fee_paid_by_the_creator_at_creation_to_the_treasury_ata`; e2e `fee4_token_fee_destination_is_the_treasury_ata_created_by_the_ata_program` | The ATA must exist before a non-zero token fee is paid; anyone can create it, and the SDK does |
| A14 | Fee admin stalls creation, or the fee program holds creation hostage | `CheckFee` is answered before the admin library's guard: an empty config reads as rate 0, and after `Renounce` the rate and treasury are frozen and `CheckFee` keeps answering. The admin hands over in two steps | executor `fee_check_passes_before_the_fee_program_is_initialised`; fees executor `fee_empty_config_reads_as_rate_zero`, `fee_after_renounce_set_fee_is_refused_and_creation_goes_on`, `fee_admin_hands_over_in_two_steps`; e2e `fee3_admin_switch_refuses_above_the_cap_and_renounce_freezes_it_while_creation_goes_on` | The admin can pause new creations, and can do nothing else: by changing the rate between a quote and its execution (27, a clean failure), or by setting a non-zero rate with a treasury that cannot receive. If that state is then frozen by `Renounce` or a lost key, creation on that deployment stays stalled and a new deployment is the remedy. Existing schedules are never affected |
| A15 | A creator points a schedule at a token program of its own that lets it take the escrow back or refuses claims | `Asset::Token` names only a definition; the token program id is a build input of `vesting` (`TOKEN_PROGRAM_ID`, as are `FEES_PROGRAM_ID` and `ATA_PROGRAM_ID`), so no message can name another one (DECISIONS 10). The image id per network is recorded and rebuilt by `scripts/check_image_ids.sh` | `program/guest/src/lib.rs` (build inputs); `program/IMAGE_IDS.expected`; `scripts/check_image_ids.sh`; reproducible builds in `evidence/guests/reproducible-local-dev.log` | Whatever powers the pinned token program gives a definition's authority over holdings apply to the escrow holding as to any other; `vesting` does not restrict which definitions are used. A beneficiary checks the token |
| A16 | PDA admission: squat a schedule address before its creator, or reach an unadmitted account | The schedule address is derived from the creator's id, so a caller can only open schedules in its own namespace; `Open` attaches the seed, which admits the new account. A delivery to an account that does not exist needs admission evidence or a seed, and the engine refuses it otherwise (SPEC 1.2). A `(creator, schedule_id)` pair can never be reused (7) | rules `seed_is_sha256_of_preimage_and_pinned`; executor `attack_unknown_schedule_and_garbage`, `create_refusals_carry_their_codes`; fees executor `fee_empty_config_needs_admission_evidence` | None known |
| A17 | A program inside a transaction the beneficiary (or an authority, or an approver) signs acts as that signer's seat | Not preventable by `vesting`: on LEZ v0.4 a signature authorises every actor of the signer's account declared in the transaction (SPEC 1.3) | Platform property, documented in SPEC 1.3 and [COMPOSITION.md](COMPOSITION.md) | A signer must check the root call and the declared actors of every transaction it signs. The `vesting` transactions the SDK builds have the signer's own `vesting` seat as their root (SPEC 3.2) |
| A18 | Milestone abuse: signal the same milestone twice, signal on a linear schedule, signal an index that does not exist | Idempotent per index (18); 20 on a linear schedule; 17 out of range; signals are refused after a cancel (16) | executor `r4_milestone_idempotent`, `soft2_milestone_authority`, `f3_a_cancelled_schedule_refuses_cancel_signal_and_make_non_cancelable`; sdk `r4_second_signal_of_a_milestone_is_refused_with_18` | See section 4 for a dishonest authority |
| A19 | Transfer the claim right of a non-transferable schedule, or transfer someone else's | `transferable` is written once at creation; only the current beneficiary transfers (3, 19) | executor `f4_transfer_refused_when_not_transferable`, `f4_transfer_moves_the_claim_right_when_transferable`; rules `f4_transfer_by_holder_only`; e2e `f4_non_transferable_is_refused_and_the_flag_never_changes` | None known |
| A20 | The sequencer sets false timestamps, reorders a cancel before a claim, or censors | Not preventable by `vesting`: it reads the chain clock (PLATFORM-FINDINGS 2). Ordering cannot take the vested part: a cancel leaves the vested part claimable, and `last_time` keeps the arithmetic consistent in any order | executor `f3_cancel_returns_unvested_and_keeps_vested_claimable`; properties `prop_f3_cancel_conserves_value_for_any_time_order` | A sequencer that lies about time moves every schedule's time. On a network with honest sequencing this is the network's assumption, not this program's |
| A21 | Re-entry: a program called during a claim or cancel calls back into the schedule | State is written before any transfer; a message that finds `pending` set is refused (21); the only programs called after the state write are the clock, the native token program and the pinned token program. No user code runs inside a claim or a cancel | rules `r1_second_operation_while_pending_is_21`, `r1_clock_reply_takes_the_pending_operation_once` | None known |
| A22 | Malformed input: bytes that decode as no message, a broken state, arithmetic overflow | Every message is decoded strictly (2); trailing or foreign bytes in a state are refused; amounts use a 192-bit intermediate and overflow is an error (29), not a panic | executor `attack_unknown_schedule_and_garbage`; rules `foreign_or_trailing_bytes_are_refused`, `broken_linear_state_is_an_overflow_not_a_panic`, `f1a_max_total_and_max_times_do_not_overflow`, `f5_batch_total_overflow_is_29` | None known |

## 4. Signing authorities: compromise and loss

Each role is an account fixed in the schedule at creation. No message changes the cancel authority, the milestone
authority or `refund_to`. What each key can and cannot do if it is stolen, and what happens if it is lost:

| Key | If stolen, the thief can | The thief cannot | If lost |
|---|---|---|---|
| Creator | Make its schedules non-cancelable (helps the beneficiary only); act as cancel or milestone authority where the creator holds those roles (the default); create new schedules funded from the creator's own accounts | Claim (3); redirect a refund (`refund_to` is fixed); change the beneficiary, the terms or the authorities | Where the creator is also the cancel authority, its schedules can no longer be cancelled. Where it is also the milestone authority, unsignalled milestones stay in escrow for good, unless a separate cancel authority cancels and refunds them |
| Cancel authority | Cancel: the unvested part goes to the fixed `refund_to`, the vested part stays claimable by the beneficiary; make the schedule non-cancelable | Send the refund anywhere but `refund_to`; touch the vested part; claim | The schedule is in effect non-cancelable |
| Milestone authority | Signal every remaining milestone at once, releasing all of it to the beneficiary | Pay itself (the beneficiary claims); un-signal | Unsignalled milestones stay in escrow; if the schedule is cancelable, the cancel authority can cancel and refund them (rules `f3_cancel_milestones_refunds_unsignalled`) |
| Beneficiary | Claim the vested amount to any account; transfer the right if the schedule is transferable | Claim more than vested; cancel; signal | The vested amount can never be claimed; a cancel returns only the unvested part |
| Fee admin | Set a rate up to 1% and a treasury; pause new creations (A14); hand over or renounce | Exceed 1%; touch existing schedules | The rate and treasury are frozen as they are; existing schedules are unaffected |

`MakeNonCancelable` is one way (executor `f3_make_non_cancelable_is_one_way`, rules `f3_non_cancelable_is_one_way`).
The creator keeps that power even when a separate cancel authority is named, and using it removes the cancel
authority's power for good (DECISIONS 13).

**Delegating authorities.** The creator can name a separate cancel authority and milestone authority at creation,
such as a multisig account or a program. [COMPOSITION.md](COMPOSITION.md) shows the program case: `launchpad_escrow`
creates the schedule from an account it derives, so creator, cancel authority and milestone authority are a program
account with no private key, and it signals or cancels only when `M` of `N` approvers agree. The project cannot
signal, cancel or freeze its own collateral (compose `soft2_compose_the_project_cannot_signal_cancel_or_freeze_its_own_collateral`,
codes 6, 5 and 4), and the `M`-th approval signals in the same transaction (compose
`soft2_compose_m_of_n_approvals_signal_then_the_project_claims`).

**Co-signing caveat.** A delegated authority is only as independent as the people behind it. In the example the
project names the approvers at `Open`; the program refuses the project itself but cannot know whether an approver is
independent of it. And A17 applies to approvers too: any program inside a transaction an approver signs can cast that
approver's vote. Approvers sign only transactions whose root call and declared actors they have checked.

## 5. Privacy

What a claim to a private account does not record in public on-chain state: the destination private account and what
the claimed tokens do afterwards. The full statement and the scans are in [PRIVACY.md](PRIVACY.md).

| Attack or limit | How the design handles it | Evidence | Residual |
|---|---|---|---|
| Read the destination from the transaction | The claim names a one-time blinded address, `destination.blinded(opening)`, with 32 fresh random bytes; the private account opens it inside the proof. No event or state field holds a destination | e2e `f2_claim_private_to_a_blinded_destination`, `f2_destination_is_not_in_schedule_state`; RPC scans of a dev-mode claim and of three real-proof claims, one included on the public chain, each finding the canonical destination 0 times ([PRIVACY.md](PRIVACY.md)) | Each scan is one transaction on a chain we run, not a population on a live network |
| Link two claims by a reused address | The opening is used once; the SDK derives `to` itself | sdk `pr1_every_private_claim_uses_a_fresh_one_time_address` | None known |
| Send a private claim to a public or foreign account by mistake | The SDK refuses any destination on the private path that is not a private account the wallet owns, before sending | sdk `pr3_private_claim_to_a_public_account_is_rejected_before_sending`; e2e `pr3_private_claim_to_a_public_account_is_refused_before_sending` | Claiming to someone else's private account is not offered in version 1 |
| Identify the beneficiary | Not hidden. The beneficiary address is public and linked to the schedule from creation, and every claim is linked to it | Stated in the disclosure every private claim shows (`sdk/src/disclosure.rs`) | Hiding the beneficiary needs commitment-based registration, outside this version |
| Link a claim to a later movement by amount or timing | Not prevented. The amount and time of every claim are public | Stated in [PRIVACY.md](PRIVACY.md) | An observer can look for a private movement of the same amount soon after. The anonymity set is whatever private activity the chain carries, small on a new chain |
| Treat a dev-mode claim as private and sound | Dev-mode claims carry no real proof and are labelled "dev mode" by the CLI, the SDK and the mini-app. A sequencer with dev mode unset refused a claim made with a dev-mode receipt (`Invalid privacy preserving execution circuit proof`) | `evidence/real-proof/20261124T155037Z/`, `evidence/real-proof/20261124T165520Z/control-sequencer-log.txt` | A chain whose sequencer runs in dev mode does not check proofs, so it must never hold value |
| The prover learns the destination | The claimant proves locally; the witness holds its keys, the destination and the opening | Proving cost in [COSTS.md](COSTS.md) (a native claim took 1,229 to 1,572 s at a peak of about 9.17 GiB on 4 cores) | Whoever runs the prover sees everything the claim hides. Delegated proving moves that trust to the prover. The claim on the public chain used a beneficiary key made on the runner for that run only |

## 6. SDK and C ABI

| Risk | How it is handled | Evidence |
|---|---|---|
| A module sends a private claim without showing the disclosure | The library sends a prepared private claim only with its `disclosure_ack` token, a SHA-256 over the prepared id, the one-time address and the disclosure text; otherwise it answers `DisclosureNotAcknowledged` and sends nothing. The route is derived from the wallet's account record, not from a request field. A token is valid for one claim only ([SDK.md](SDK.md)) | ffi `u1_c_abi_prepared_transactions_are_used_once`, `u1_c_abi_full_lifecycle_with_public_and_private_claims`. The token proves that the call path carried the disclosure; it cannot prove that a person read it, which is the module's part |
| A panic unwinds into C | Every entry runs inside `catch_unwind`; a panic becomes an `ok: false` answer, and a later call on a poisoned client answers `ClientPoisoned` | ffi `u1_c_abi_a_panic_becomes_an_error_and_never_unwinds_into_c`, `u1_c_abi_one_handle_serves_many_threads`. The ABI crate denies `unsafe` code except in `sdk/ffi/src/abi.rs` and its tests |
| A refused transaction is sent and charged without an explanation | On rc3 the sequencer returns no refusal text and charges a reverted transaction (PLATFORM-FINDINGS 7). The SDK runs every transaction locally against current state first, reports the code and an action, and never sends one it expects to revert. The wallet's simulation skips validity windows, so the SDK checks `as_of` against the clock itself | sdk `u7_refusals_from_the_chain_map_to_codes_and_actions`, `u7_a_private_claim_ahead_of_the_clock_is_not_sent`; e2e `r1_failed_claim_on_chain_changes_nothing_then_retry_pays` |
| A dry run read against stale or false state | The dry run is only as good as the state it reads. A transaction that would now fail is refused by the chain and changes nothing, except its charged fee | Residual: a client that reads from an endpoint it does not trust can be misled about balances and quotes; it cannot be made to sign anything it did not build |

## 7. Deployments, upgrades and the public endpoint

**Immutability.** `vesting` and `vesting_fees` are deployed immutable (`--immutable` in `scripts/deploy_local.sh`):
no account can replace their code. `vesting` has no admin and no configuration. The cost is that a bug in a deployed
version cannot be patched for schedules already created on it. A fix ships as a new deployment at a new program id,
and an existing schedule moves only by the paths it already has: its beneficiary claims what has vested, and a
cancelable schedule can be cancelled and created again on the new version. Guests are built reproducibly in a pinned
image and their image ids are checked (`program/IMAGE_IDS.expected`, `scripts/check_image_ids.sh`), so anyone can
confirm that a deployed program is this source.

**Public endpoint.** The read-only relay of our public chain ([tools/public_endpoint/](../tools/public_endpoint/README.md))
forwards 14 allow-listed read methods and refuses everything else, `sendTransaction` included, with `-32601`. A
request is checked twice, by the public relay and again by the dialer on the sequencer's host, and re-encoded before it
reaches the sequencer. The host opens the connection to the relay, so the relay never calls the sequencer. Limits: 4 KiB
per request, replies at most 2 MiB, 5 requests a second per visitor, 20 in total, at most 16 entries per list
parameter. Tests: `tools/public_endpoint/test_policy.py`. Residual: the endpoint and the chain behind it are run by
us; a reader trusts us for honest answers and for availability, and it is not a Logos network. It holds no value.

## 8. Residual risks and the audit

Open items, in order of weight:

1. **No third-party audit.** No schedule should hold real value until the external audit is closed (milestone M5).
2. **Platform trust.** `vesting` trusts LEZ v0.4.0-rc3: its authorisation rules, its clock, its proof verification and
   the pinned token program. They are checked again on testnet 0.4 when the programs are ported (milestone M1).
3. **Authorities are people.** A dishonest milestone authority can release everything early, and a dishonest cancel
   authority can cancel early (the refund still goes only to `refund_to`). Delegate both to a multisig or a program
   where that matters (section 4).
4. **Privacy limits** of section 5: the beneficiary is public, amounts and times are public, the anonymity set is
   small, and the prover sees the witness.
5. **Fee admin** can pause new creations (A14).
6. **Gaps in the tests.** A9 (a stranger's account as the funding source) has no dedicated test; four private claims
   with a real proof (three native, one token) have been made, all on chains we operate; sends through the C ABI to a sequencer are not yet
   shown ([SDK.md](SDK.md)).

The audit's scope is both programs, the rules crate and the SDK's private-claim path; the RFP-001 admin library and
the token program are in scope where our use of them is. The audit package is prepared at a frozen commit, and every
finding is fixed or answered with a test before the final release; a fix to a deployed program ships as a new
immutable version with migration notes. How to report a vulnerability: [SECURITY.md](../SECURITY.md).
