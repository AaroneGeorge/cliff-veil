//! Requirement tests of the SDK against the in-memory backend. Names start with the requirement they show
//! (`u1_` = RFP Usability 1, `pr3_` = Privacy 3, `r1_` = Reliability 1, `f_` = fees).

use lee_core::account::AccountId;
use vesting_core::{Asset, BatchItem, BatchShape, BatchTerms, Kind, NextUnlock};

use crate::{
    AccountRecord, ClaimTarget, Client, Destination, Error, MemoryBackend, Outcome, Programs, Standing, linear_terms,
    milestone_terms, schedule_id_from_label,
};

const VESTING: AccountId = AccountId::new([9; 32]);
const CREATOR: AccountId = AccountId::new([1; 32]);
const ALICE: AccountId = AccountId::new([2; 32]);
const BOB: AccountId = AccountId::new([3; 32]);
const STRANGER: AccountId = AccountId::new([4; 32]);
const SHIELDED: AccountId = AccountId::new([5; 32]);
const TREASURY: AccountId = AccountId::new([6; 32]);

/// 2026-01-01T00:00:00Z in ms.
const T0: u64 = 1_767_225_600_000;
const DAY: u64 = 86_400_000;

fn chain() -> MemoryBackend {
    let mut m = MemoryBackend::new(Programs::new(VESTING), T0);
    m.add_public(CREATOR, 1_000_000);
    m.add_public(ALICE, 1_000);
    m.add_public(BOB, 1_000);
    m.add_public(STRANGER, 1_000);
    m
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// A native schedule of 100 000 to Alice: start T0, cliff T0 + 10 days, end T0 + 100 days.
async fn linear_schedule(c: &mut Client<MemoryBackend>, label: &str) -> AccountId {
    let id = schedule_id_from_label(label);
    let terms = linear_terms(ALICE, Asset::Native, 100_000, T0, T0 + 10 * DAY, T0 + 100 * DAY);
    let p = c.prepare_create(CREATOR, id, terms).await.expect("create is valid");
    c.send(&p).await.expect("create is sent");
    c.programs().schedule_account(CREATOR, &id)
}

#[test]
fn doc_example_runs() {
    rt().block_on(async {
        let mut chain = MemoryBackend::new(Programs::new(VESTING), 0);
        chain.add_public(CREATOR, 10_000);
        chain.add_public(ALICE, 100);
        let mut client = Client::new(chain);
        let create = client
            .prepare_create(
                CREATOR,
                [7; 32],
                linear_terms(ALICE, Asset::Native, 1_000, 0, 250, 1_000),
            )
            .await
            .unwrap();
        client.send(&create).await.unwrap();
        let schedule = client.programs().schedule_account(CREATOR, &[7; 32]);
        client.backend_mut().now = 500;
        assert_eq!(client.claimable(schedule).await.unwrap(), 500);
        let claim = client
            .prepare_claim(ALICE, schedule, ClaimTarget::Public(ALICE))
            .await
            .unwrap();
        client.send(&claim).await.unwrap();
        assert_eq!(client.status(schedule).await.unwrap().state.claimed, 500);
    });
}

#[test]
fn u1_full_lifecycle_create_query_claim_cancel() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "lifecycle").await;
        assert_eq!(c.backend().native(s), 100_000, "escrow holds exactly the total");
        assert_eq!(c.backend().native(CREATOR), 1_000_000 - 100_000 - 10);

        c.backend_mut().now = T0 + 50 * DAY;
        assert_eq!(c.claimable(s).await.unwrap(), 50_000);
        let claim = c.prepare_claim(ALICE, s, ClaimTarget::Public(ALICE)).await.unwrap();
        c.send(&claim).await.unwrap();
        assert_eq!(c.backend().native(ALICE), 1_000 + 50_000 - 10);

        c.backend_mut().now = T0 + 60 * DAY;
        let cancel = c.prepare_cancel(CREATOR, s).await.unwrap();
        let Outcome::Cancelled {
            refunded,
            still_claimable,
            refund_to,
            ..
        } = cancel.outcome
        else {
            panic!("cancel outcome expected")
        };
        assert_eq!((refunded, still_claimable, refund_to), (40_000, 10_000, CREATOR));
        c.send(&cancel).await.unwrap();
        let view = c.status(s).await.unwrap();
        assert_eq!(
            (view.standing, view.claimable, view.locked),
            (Standing::Cancelled, 10_000, 0)
        );

        // The vested remainder stays claimable after the cancel.
        let rest = c.prepare_claim(ALICE, s, ClaimTarget::Public(ALICE)).await.unwrap();
        c.send(&rest).await.unwrap();
        assert_eq!(c.status(s).await.unwrap().standing, Standing::Settled);
        assert_eq!(c.backend().native(s), 0);
    });
}

#[test]
fn u1_create_batch_splits_into_transactions_of_the_requested_size() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let common = BatchTerms {
            asset: Asset::Native,
            shape: BatchShape::Linear {
                start: T0,
                cliff: T0,
                end: T0 + DAY,
            },
            cancelable: true,
            transferable: false,
            cancel_authority: None,
            milestone_authority: None,
            refund_to: None,
        };
        let items: Vec<BatchItem> = (0..7u8)
            .map(|i| BatchItem {
                schedule_id: [i; 32],
                beneficiary: crate::core_id(AccountId::new([100 + i; 32])),
                total: 1_000 + u128::from(i),
            })
            .collect();
        let prepared = c
            .prepare_create_batch(CREATOR, common.clone(), items.clone(), Some(3))
            .await
            .unwrap();
        assert_eq!(prepared.len(), 3, "7 items at 3 per transaction");
        for p in &prepared {
            c.send(p).await.unwrap();
        }
        for item in &items {
            let s = c.programs().schedule_account(CREATOR, &item.schedule_id);
            assert_eq!(c.backend().native(s), item.total, "one escrow per schedule");
        }
        // A repeated id anywhere in the list is refused before anything is built.
        let mut dup = items[..2].to_vec();
        dup[1].schedule_id = dup[0].schedule_id;
        let e = c
            .prepare_create_batch(CREATOR, common.clone(), dup, None)
            .await
            .unwrap_err();
        assert_eq!(e.code(), Some(7));
        let e = c
            .prepare_create_batch(CREATOR, common, Vec::new(), None)
            .await
            .unwrap_err();
        assert_eq!(e.code(), Some(24));
    });
}

#[test]
fn u1_milestones_signal_then_claim() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let id = schedule_id_from_label("milestones");
        let p = c
            .prepare_create(CREATOR, id, milestone_terms(ALICE, Asset::Native, vec![300, 700]))
            .await
            .unwrap();
        c.send(&p).await.unwrap();
        let s = c.programs().schedule_account(CREATOR, &id);
        assert_eq!(
            c.status(s).await.unwrap().next_unlock,
            NextUnlock::WhenMilestoneSignalled
        );

        let signal = c.prepare_signal(CREATOR, s, 1).await.unwrap();
        assert_eq!(signal.outcome, Outcome::Signalled { index: 1, amount: 700 });
        c.send(&signal).await.unwrap();
        assert_eq!(c.claimable(s).await.unwrap(), 700);
    });
}

#[test]
fn r4_second_signal_of_a_milestone_is_refused_with_18() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let id = schedule_id_from_label("twice");
        let p = c
            .prepare_create(CREATOR, id, milestone_terms(ALICE, Asset::Native, vec![1, 2]))
            .await
            .unwrap();
        c.send(&p).await.unwrap();
        let s = c.programs().schedule_account(CREATOR, &id);
        let first = c.prepare_signal(CREATOR, s, 0).await.unwrap();
        c.send(&first).await.unwrap();
        let e = c.prepare_signal(CREATOR, s, 0).await.unwrap_err();
        assert_eq!(e.code(), Some(18));
        assert!(e.to_string().contains("already unlocked"), "{e}");
        // Sending the same transaction again anyway: the backend refuses with the same code; nothing unlocks twice.
        let e = c.send(&first).await.unwrap_err();
        assert_eq!(e.code(), Some(18));
        assert_eq!(c.claimable(s).await.unwrap(), 1);
        assert_eq!(c.prepare_signal(ALICE, s, 1).await.unwrap_err().code(), Some(6));
    });
}

#[test]
fn u1_transfer_beneficiary_moves_the_right_to_claim() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let id = schedule_id_from_label("transfer");
        let mut terms = linear_terms(ALICE, Asset::Native, 1_000, T0, T0, T0 + DAY);
        terms.transferable = true;
        let p = c.prepare_create(CREATOR, id, terms).await.unwrap();
        c.send(&p).await.unwrap();
        let s = c.programs().schedule_account(CREATOR, &id);
        let t = c.prepare_transfer(ALICE, s, BOB).await.unwrap();
        assert_eq!(t.outcome, Outcome::Transferred { from: ALICE, to: BOB });
        c.send(&t).await.unwrap();
        c.backend_mut().now = T0 + DAY;
        assert_eq!(
            c.prepare_claim(ALICE, s, ClaimTarget::Public(ALICE))
                .await
                .unwrap_err()
                .code(),
            Some(3)
        );
        let claim = c.prepare_claim(BOB, s, ClaimTarget::Public(BOB)).await.unwrap();
        c.send(&claim).await.unwrap();
        assert_eq!(c.backend().native(BOB), 1_000 + 1_000 - 10);

        // A schedule created non-transferable refuses with 19.
        let s2 = linear_schedule(&mut c, "fixed").await;
        assert_eq!(c.prepare_transfer(ALICE, s2, BOB).await.unwrap_err().code(), Some(19));
    });
}

#[test]
fn u1_make_non_cancelable_is_one_way() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "lock").await;
        assert_eq!(
            c.prepare_make_non_cancelable(ALICE, s).await.unwrap_err().code(),
            Some(4)
        );
        let p = c.prepare_make_non_cancelable(CREATOR, s).await.unwrap();
        c.send(&p).await.unwrap();
        assert_eq!(c.prepare_cancel(CREATOR, s).await.unwrap_err().code(), Some(15));
        assert_eq!(
            c.prepare_make_non_cancelable(CREATOR, s).await.unwrap_err().code(),
            Some(15)
        );
    });
}

#[test]
fn u3_query_reports_vested_claimable_locked_and_next_unlock() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "query").await;
        let before = c.status(s).await.unwrap();
        assert_eq!((before.vested, before.claimable, before.locked), (0, 0, 100_000));
        assert_eq!(before.next_unlock, NextUnlock::At(T0 + 10 * DAY));
        assert_eq!(before.next_unlock_text(), "2026-01-11T00:00:00.000Z");
        assert_eq!(before.escrow, Some(100_000));
        c.backend_mut().now = T0 + 25 * DAY;
        let mid = c.status(s).await.unwrap();
        assert_eq!((mid.vested, mid.claimable, mid.locked), (25_000, 25_000, 75_000));
        assert_eq!(mid.standing, Standing::Active);
    });
}

#[test]
fn u4_public_claim_summary_shows_amount_and_estimated_fee() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "summary").await;
        c.backend_mut().now = T0 + 20 * DAY;
        let p = c.prepare_claim(ALICE, s, ClaimTarget::Public(ALICE)).await.unwrap();
        let Outcome::Claim(summary) = p.outcome else {
            panic!("claim outcome expected")
        };
        assert_eq!((summary.amount, summary.fee, summary.fee_payer), (20_000, 10, ALICE));
        let text = summary.render();
        assert!(text.contains("claimable now:  at least 20000"), "{text}");
        assert!(text.contains("network fee:    ~10 (estimated)"), "{text}");
    });
}

#[test]
fn u4_public_claim_refused_when_the_beneficiary_cannot_pay_the_fee() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "broke").await;
        c.backend_mut().set_native(ALICE, 3);
        c.backend_mut().now = T0 + 20 * DAY;
        let e = c.prepare_claim(ALICE, s, ClaimTarget::Public(ALICE)).await.unwrap_err();
        assert!(matches!(e, Error::InsufficientBalance { need: 10, have: 3, .. }), "{e}");
        assert!(e.to_string().contains("fund it with at least 7 more"), "{e}");
    });
}

#[test]
fn u4_create_refused_when_the_creator_cannot_fund_it() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let terms = linear_terms(ALICE, Asset::Native, 2_000_000, T0, T0, T0 + DAY);
        let e = c.prepare_create(CREATOR, [1; 32], terms).await.unwrap_err();
        assert!(matches!(e, Error::InsufficientBalance { account: CREATOR, .. }), "{e}");
        assert!(c.backend().sent().is_empty());
    });
}

#[test]
fn u4_private_native_claim_pays_its_own_fee_out_of_the_claim() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "private-fee").await;
        c.backend_mut().now = T0 + 10 * DAY;
        let p = c
            .prepare_claim(
                ALICE,
                s,
                ClaimTarget::Private {
                    account: None,
                    fee_payer: None,
                },
            )
            .await
            .unwrap();
        let Outcome::Claim(summary) = p.outcome else {
            panic!("claim outcome expected")
        };
        let Destination::Private { account, fresh, .. } = summary.destination else {
            panic!("private destination expected")
        };
        assert!(fresh);
        // The destination is named as fee payer: the wallet must not pick another funded private account.
        assert!(matches!(p.tx.route, crate::Route::Private { fee_payer: Some(f), .. } if f == account));
        assert_eq!(
            (summary.fee_payer, summary.fee, summary.fee_payer_holds),
            (account, 50, 10_000)
        );
        c.send(&p).await.unwrap();
        assert_eq!(c.backend().private(account), Some(10_000 - 50));
    });
}

#[test]
fn u4_private_fee_shortfall_is_an_actionable_error() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        // Total 40 over 100 days: at the cliff only 4 vest, below the private fee of 50.
        let id = schedule_id_from_label("tiny");
        let p = c
            .prepare_create(
                CREATOR,
                id,
                linear_terms(ALICE, Asset::Native, 40, T0, T0 + 10 * DAY, T0 + 100 * DAY),
            )
            .await
            .unwrap();
        c.send(&p).await.unwrap();
        let s = c.programs().schedule_account(CREATOR, &id);
        c.backend_mut().now = T0 + 10 * DAY;
        let sent_before = c.backend().sent().len();
        let e = c
            .prepare_claim(
                ALICE,
                s,
                ClaimTarget::Private {
                    account: None,
                    fee_payer: None,
                },
            )
            .await
            .unwrap_err();
        let Error::PrivateFeeShortfall { need, have, .. } = &e else {
            panic!("fee shortfall expected, got {e}")
        };
        assert_eq!((*need, *have), (50, 4));
        assert!(e.to_string().contains("short by 46"), "{e}");
        assert!(e.to_string().contains("name a funded private account"), "{e}");
        assert_eq!(c.backend().sent().len(), sent_before, "nothing sent");

        // With a funded private fee payer the same claim goes through.
        c.backend_mut().add_private(SHIELDED, 500);
        let p = c
            .prepare_claim(
                ALICE,
                s,
                ClaimTarget::Private {
                    account: None,
                    fee_payer: Some(SHIELDED),
                },
            )
            .await
            .unwrap();
        c.send(&p).await.unwrap();
        assert_eq!(c.backend().private(SHIELDED), Some(450));
    });
}

#[test]
fn u7_nothing_claimable_before_the_cliff_names_the_next_unlock_in_utc() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "early").await;
        c.backend_mut().now = T0 + DAY;
        for target in [
            ClaimTarget::Public(ALICE),
            ClaimTarget::Private {
                account: Some(SHIELDED),
                fee_payer: None,
            },
        ] {
            c.backend_mut().add_private(SHIELDED, 0);
            let e = c.prepare_claim(ALICE, s, target).await.unwrap_err();
            assert!(matches!(e, Error::NothingClaimable { next: NextUnlock::At(t) } if t == T0 + 10 * DAY));
            assert_eq!(
                e.to_string(),
                "cliff-veil error 14: nothing claimable yet; next unlock at 2026-01-11T00:00:00.000Z (1768089600000 ms)"
            );
        }
    });
}

#[test]
fn u7_refusals_from_the_chain_map_to_codes_and_actions() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "raw").await;
        c.backend_mut().now = T0 + 50 * DAY;
        // A transaction built without the SDK's checks: a stranger claims. The chain's refusal comes back mapped.
        let tx = c.programs().claim(STRANGER, s, STRANGER, &Asset::Native).unwrap();
        let e = crate::Backend::submit(c.backend_mut(), &tx).await.unwrap_err();
        assert_eq!(e.code(), Some(3));
        assert!(e.to_string().contains("only the beneficiary may do this"), "{e}");
        assert!(
            e.to_string()
                .contains("What to do: send from the schedule's current beneficiary"),
            "{e}"
        );
        // The SDK refuses the same claim before building it, with the same code.
        assert_eq!(
            c.prepare_claim(STRANGER, s, ClaimTarget::Public(STRANGER))
                .await
                .unwrap_err()
                .code(),
            Some(3)
        );
        assert_eq!(c.prepare_cancel(ALICE, s).await.unwrap_err().code(), Some(5));
        assert!(matches!(
            c.status(AccountId::new([77; 32])).await.unwrap_err(),
            Error::NoSchedule(_)
        ));
        assert_eq!(
            c.prepare_claim(ALICE, s, ClaimTarget::Public(s))
                .await
                .unwrap_err()
                .code(),
            Some(26)
        );
    });
}

#[test]
fn r1_a_refused_claim_consumes_nothing_and_can_be_retried() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "retry").await;
        c.backend_mut().now = T0 + 50 * DAY;
        let tx = c.programs().claim(STRANGER, s, STRANGER, &Asset::Native).unwrap();
        let before = (c.backend().native(s), c.status(s).await.unwrap().state);
        assert!(crate::Backend::submit(c.backend_mut(), &tx).await.is_err());
        assert_eq!((c.backend().native(s), c.status(s).await.unwrap().state), before);
        let p = c.prepare_claim(ALICE, s, ClaimTarget::Public(ALICE)).await.unwrap();
        c.send(&p).await.unwrap();
        assert_eq!(c.status(s).await.unwrap().state.claimed, 50_000);
    });
}

#[test]
fn pr1_private_claim_reaches_the_private_account_and_never_names_it() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "private").await;
        c.backend_mut().add_private(SHIELDED, 100);
        c.backend_mut().now = T0 + 30 * DAY;
        let p = c
            .prepare_claim(
                ALICE,
                s,
                ClaimTarget::Private {
                    account: Some(SHIELDED),
                    fee_payer: None,
                },
            )
            .await
            .unwrap();
        c.send(&p).await.unwrap();
        assert_eq!(c.backend().private(SHIELDED), Some(100 + 30_000 - 50));
        assert_eq!(c.status(s).await.unwrap().state.claimed, 30_000);

        // What an observer sees: the message and the public actors. The private account is in neither, and it has
        // no public account.
        let sent = c.backend().sent().last().unwrap().clone();
        assert!(sent.private);
        assert!(
            !sent.message.windows(32).any(|w| w == SHIELDED.value()),
            "canonical id not in the message"
        );
        assert!(sent.public_actors.iter().all(|a| a.account_id != SHIELDED));
        assert!(!c.backend().public_accounts().contains(&SHIELDED));
        // The schedule, the beneficiary (signer) and the amount are public: the beneficiary is the root actor.
        assert_eq!(sent.public_actors[0].account_id, ALICE);
        assert!(sent.public_actors.iter().any(|a| a.account_id == s));
    });
}

#[test]
fn pr1_every_private_claim_uses_a_fresh_one_time_address() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "fresh").await;
        c.backend_mut().add_private(SHIELDED, 100);
        let mut seen = Vec::new();
        for day in [20, 40] {
            c.backend_mut().now = T0 + day * DAY;
            let p = c
                .prepare_claim(
                    ALICE,
                    s,
                    ClaimTarget::Private {
                        account: Some(SHIELDED),
                        fee_payer: None,
                    },
                )
                .await
                .unwrap();
            let Outcome::Claim(summary) = &p.outcome else { panic!() };
            let Destination::Private { blinded, .. } = summary.destination else {
                panic!()
            };
            assert_ne!(blinded, SHIELDED);
            seen.push(blinded);
            c.send(&p).await.unwrap();
        }
        assert_ne!(
            seen[0], seen[1],
            "two claims to one account are not linkable by address"
        );
    });
}

#[test]
fn pr3_private_claim_to_a_public_account_is_rejected_before_sending() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "pr3").await;
        c.backend_mut().now = T0 + 50 * DAY;
        let sent_before = c.backend().sent().len();
        let e = c
            .prepare_claim(
                ALICE,
                s,
                ClaimTarget::Private {
                    account: Some(BOB),
                    fee_payer: None,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                e,
                Error::NotPrivateAccount {
                    account: BOB,
                    found: AccountRecord::PublicOwned
                }
            ),
            "{e}"
        );
        assert!(e.to_string().contains("not a private account of this wallet"), "{e}");
        // An account the wallet holds no key for is refused too, and so is a public fee payer.
        let e = c
            .prepare_claim(
                ALICE,
                s,
                ClaimTarget::Private {
                    account: Some(STRANGER.blinded(&[1; 32])),
                    fee_payer: None,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                e,
                Error::NotPrivateAccount {
                    found: AccountRecord::NotOwned,
                    ..
                }
            ),
            "{e}"
        );
        c.backend_mut().add_private(SHIELDED, 0);
        let e = c
            .prepare_claim(
                ALICE,
                s,
                ClaimTarget::Private {
                    account: Some(SHIELDED),
                    fee_payer: Some(ALICE),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(e, Error::NotPrivateAccount { account: ALICE, .. }), "{e}");
        assert_eq!(c.backend().sent().len(), sent_before, "nothing was sent");
        assert_eq!(c.status(s).await.unwrap().state.claimed, 0);
    });
}

#[test]
fn f_fee_config_is_read_and_quoted_at_creation() {
    rt().block_on(async {
        let mut m = MemoryBackend::new(
            Programs {
                fees: Some(AccountId::new([8; 32])),
                ..Programs::new(VESTING)
            },
            T0,
        );
        m.add_public(CREATOR, 1_000_000);
        m.fees.rate_bps = 50;
        m.fees.treasury = TREASURY;
        let mut c = Client::new(m);
        let config = c.fee_config().await.unwrap();
        assert_eq!(
            (config.rate_bps, config.treasury, config.deployed),
            (50, TREASURY, true)
        );
        assert_eq!(c.quote_fee(10_000, CREATOR).await.unwrap(), (50, TREASURY));
        let p = c
            .prepare_create(
                CREATOR,
                [3; 32],
                linear_terms(ALICE, Asset::Native, 10_000, T0, T0, T0 + DAY),
            )
            .await
            .unwrap();
        assert!(matches!(
            p.outcome,
            Outcome::Created {
                protocol_fee: 50,
                treasury: TREASURY,
                ..
            }
        ));
        c.send(&p).await.unwrap();
        assert_eq!(c.backend().native(TREASURY), 50);

        // A changed rate between quote and send: the program refuses with 27 and nothing moves.
        let p = c
            .prepare_create(
                CREATOR,
                [4; 32],
                linear_terms(ALICE, Asset::Native, 10_000, T0, T0, T0 + DAY),
            )
            .await
            .unwrap();
        c.backend_mut().fees.rate_bps = 60;
        assert_eq!(c.send(&p).await.unwrap_err().code(), Some(27));
        assert_eq!(c.backend().native(TREASURY), 50);
    });
}

#[test]
fn u1_bad_terms_are_refused_with_their_codes() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let mut t = linear_terms(ALICE, Asset::Native, 0, T0, T0, T0 + DAY);
        assert_eq!(
            c.prepare_create(CREATOR, [1; 32], t.clone()).await.unwrap_err().code(),
            Some(10)
        );
        t.total = 5;
        t.kind = Kind::Linear {
            start: T0 + 1,
            cliff: T0,
            end: T0 + DAY,
        };
        assert_eq!(
            c.prepare_create(CREATOR, [1; 32], t).await.unwrap_err().code(),
            Some(11)
        );
        let s = linear_schedule(&mut c, "dup").await;
        let again = linear_terms(ALICE, Asset::Native, 100_000, T0, T0 + 10 * DAY, T0 + 100 * DAY);
        let e = c
            .prepare_create(CREATOR, schedule_id_from_label("dup"), again)
            .await
            .unwrap_err();
        assert_eq!(e.code(), Some(7));
        assert!(c.status(s).await.is_ok());
    });
}

#[test]
fn u7_a_private_claim_ahead_of_the_clock_is_not_sent() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "window").await;
        c.backend_mut().add_private(SHIELDED, 0);
        c.backend_mut().now = T0 + 30 * DAY;
        let p = c
            .prepare_claim(
                ALICE,
                s,
                ClaimTarget::Private {
                    account: Some(SHIELDED),
                    fee_payer: None,
                },
            )
            .await
            .unwrap();
        // The clock reads earlier than the claim's as_of (another node's view, a reorganised head): refused locally.
        c.backend_mut().now = T0 + 29 * DAY;
        let sent_before = c.backend().sent().len();
        let e = c.send(&p).await.unwrap_err();
        assert!(
            matches!(
                e,
                Error::Chain {
                    kind: crate::ChainFailure::ValidityWindow,
                    ..
                }
            ),
            "{e}"
        );
        assert_eq!(c.backend().sent().len(), sent_before, "nothing sent, no fee paid");
        c.backend_mut().now = T0 + 30 * DAY;
        c.send(&p).await.unwrap();
    });
}

/// 64 milestone weights summing to 10 000 basis points.
fn bps64() -> Vec<u16> {
    let mut bps = vec![156u16; 63];
    bps.push(10_000 - 156 * 63);
    bps
}

fn batch_terms(shape: BatchShape) -> BatchTerms {
    BatchTerms {
        asset: Asset::Native,
        shape,
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    }
}

fn batch_items(n: u16, total: u128) -> Vec<BatchItem> {
    (0..n)
        .map(|i| {
            let mut id = [0u8; 32];
            id[..2].copy_from_slice(&i.to_le_bytes());
            let mut b = [0x77; 32];
            b[..2].copy_from_slice(&i.to_le_bytes());
            BatchItem {
                schedule_id: id,
                beneficiary: crate::core_id(AccountId::new(b)),
                total,
            }
        })
        .collect()
}

#[test]
fn u1_f5_heaviest_batch_with_a_fee_is_split_so_every_transaction_fits() {
    rt().block_on(async {
        let mut m = MemoryBackend::new(
            Programs {
                fees: Some(AccountId::new([8; 32])),
                ..Programs::new(VESTING)
            },
            T0,
        );
        m.add_public(CREATOR, 10_000_000);
        m.fees.rate_bps = 50;
        m.fees.treasury = TREASURY;
        let mut c = Client::new(m);
        let common = batch_terms(BatchShape::Milestones { bps: bps64() });
        assert_eq!(c.batch_capacity(&common).await.unwrap(), 51);
        let items = batch_items(120, 10_000);
        let prepared = c
            .prepare_create_batch(CREATOR, common.clone(), items.clone(), None)
            .await
            .unwrap();
        let sizes: Vec<usize> = prepared.iter().map(|p| p.tx.schedules.len()).collect();
        assert_eq!(
            sizes,
            vec![51, 51, 18],
            "64-milestone items with a fee: 51 per transaction, not the cap of 164"
        );
        for p in &prepared {
            assert!(
                p.estimate.cycles <= crate::batch::TARGET_CYCLES,
                "{} cycles",
                p.estimate.cycles
            );
            c.send(p).await.unwrap();
        }
        for item in &items {
            assert_eq!(
                c.backend()
                    .native(c.programs().schedule_account(CREATOR, &item.schedule_id)),
                10_000
            );
        }
        // Each transaction pays 50 bps of its own total: of 51 x 10 000 twice, and of 18 x 10 000.
        assert_eq!(c.backend().native(TREASURY), 2 * 2_550 + 900);

        // A requested size above what fits is lowered to what fits; a smaller one is kept.
        let more = batch_items(60, 10_000)
            .into_iter()
            .map(|mut i| {
                i.schedule_id[31] = 1;
                i
            })
            .collect::<Vec<_>>();
        let p = c
            .prepare_create_batch(CREATOR, common.clone(), more.clone(), Some(100))
            .await
            .unwrap();
        assert_eq!(p.iter().map(|p| p.tx.schedules.len()).collect::<Vec<_>>(), vec![51, 9]);
        let p = c.prepare_create_batch(CREATOR, common, more, Some(20)).await.unwrap();
        assert_eq!(
            p.iter().map(|p| p.tx.schedules.len()).collect::<Vec<_>>(),
            vec![20, 20, 20]
        );
    });
}

#[test]
fn u1_f5_linear_batch_fills_transactions_up_to_the_model() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let common = batch_terms(BatchShape::Linear {
            start: T0,
            cliff: T0,
            end: T0 + DAY,
        });
        let prepared = c
            .prepare_create_batch(CREATOR, common.clone(), batch_items(400, 100), None)
            .await
            .unwrap();
        let sizes: Vec<usize> = prepared.iter().map(|p| p.tx.schedules.len()).collect();
        assert_eq!(sizes, vec![162, 162, 76]);
        assert!(
            prepared
                .iter()
                .all(|p| p.estimate.cycles <= crate::batch::TARGET_CYCLES)
        );
        // Zero, or above the program's cap, is refused with 25.
        assert_eq!(
            c.prepare_create_batch(CREATOR, common.clone(), batch_items(3, 100), Some(0))
                .await
                .unwrap_err()
                .code(),
            Some(25)
        );
        assert_eq!(
            c.prepare_create_batch(CREATOR, common, batch_items(3, 100), Some(165))
                .await
                .unwrap_err()
                .code(),
            Some(25)
        );
    });
}

#[test]
fn u2_list_by_creator_finds_single_and_batch_schedules() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let single = linear_schedule(&mut c, "single").await;
        let common = batch_terms(BatchShape::Linear {
            start: T0,
            cliff: T0,
            end: T0 + DAY,
        });
        for p in c
            .prepare_create_batch(CREATOR, common, batch_items(3, 100), None)
            .await
            .unwrap()
        {
            c.send(&p).await.unwrap();
        }
        c.backend_mut().add_public(BOB, 1_000_000);
        let p = c
            .prepare_create(BOB, [5; 32], linear_terms(ALICE, Asset::Native, 10, T0, T0, T0 + DAY))
            .await
            .unwrap();
        c.send(&p).await.unwrap();

        let mut cache = crate::ScanCache::new(VESTING);
        let mine = c
            .list(crate::ListFilter::Creator(CREATOR), &mut cache, None)
            .await
            .unwrap();
        assert_eq!(mine.len(), 4);
        assert!(mine.iter().any(|v| v.account == single));
        let bobs = c.list(crate::ListFilter::Creator(BOB), &mut cache, None).await.unwrap();
        assert_eq!(bobs.len(), 1);
        assert_eq!(cache.schedules.len(), 5);
    });
}

#[test]
fn u2_list_by_beneficiary_follows_transfers() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let id = schedule_id_from_label("moving");
        let mut terms = linear_terms(ALICE, Asset::Native, 1_000, T0, T0, T0 + DAY);
        terms.transferable = true;
        let p = c.prepare_create(CREATOR, id, terms).await.unwrap();
        c.send(&p).await.unwrap();
        let s = c.programs().schedule_account(CREATOR, &id);
        let kept = linear_schedule(&mut c, "kept").await;
        let mut cache = crate::ScanCache::new(VESTING);
        let alice = c
            .list(crate::ListFilter::Beneficiary(ALICE), &mut cache, None)
            .await
            .unwrap();
        assert_eq!(alice.iter().map(|v| v.account).collect::<Vec<_>>().len(), 2);

        let t = c.prepare_transfer(ALICE, s, BOB).await.unwrap();
        c.send(&t).await.unwrap();
        let alice = c
            .list(crate::ListFilter::Beneficiary(ALICE), &mut cache, None)
            .await
            .unwrap();
        assert_eq!(alice.iter().map(|v| v.account).collect::<Vec<_>>(), vec![kept]);
        let bob = c
            .list(crate::ListFilter::Beneficiary(BOB), &mut cache, None)
            .await
            .unwrap();
        assert_eq!(bob.iter().map(|v| v.account).collect::<Vec<_>>(), vec![s]);
    });
}

#[test]
fn u2_scan_is_incremental_skips_reverted_creations_and_honours_from_block() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let first = linear_schedule(&mut c, "first").await;
        let mut cache = crate::ScanCache::new(VESTING);
        let last = c.scan(&mut cache, None).await.unwrap();
        assert_eq!(cache.next_block, last + 1);
        assert_eq!(cache.schedules.len(), 1);

        // A creation included as reverted opened nothing and is not listed.
        let ghost = c
            .programs()
            .create(
                CREATOR,
                [0xEE; 32],
                linear_terms(ALICE, Asset::Native, 5, T0, T0, T0 + DAY),
                0,
                CREATOR,
            )
            .unwrap();
        c.backend_mut().record_reverted(&ghost);
        let second = linear_schedule(&mut c, "second").await;
        let all = c
            .list(crate::ListFilter::Creator(CREATOR), &mut cache, None)
            .await
            .unwrap();
        assert_eq!(
            all.iter().map(|v| v.account).collect::<std::collections::BTreeSet<_>>(),
            [first, second].into()
        );

        // A fresh cache started after the first creation's block does not see it.
        let mut late = crate::ScanCache::new(VESTING);
        let from = last + 1;
        let seen = c
            .list(crate::ListFilter::Creator(CREATOR), &mut late, Some(from))
            .await
            .unwrap();
        assert_eq!(seen.iter().map(|v| v.account).collect::<Vec<_>>(), vec![second]);

        // A cache of another deployment is reset, not reused.
        let mut foreign = crate::ScanCache::new(AccountId::new([0x42; 32]));
        foreign.next_block = 1_000;
        assert_eq!(
            c.list(crate::ListFilter::Creator(CREATOR), &mut foreign, None)
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(foreign.vesting, VESTING);
    });
}

#[test]
fn u3_listed_schedules_carry_claimable_and_next_unlock() {
    rt().block_on(async {
        let mut c = Client::new(chain());
        let s = linear_schedule(&mut c, "listed").await;
        c.backend_mut().now = T0 + 20 * DAY;
        let mut cache = crate::ScanCache::new(VESTING);
        let views = c
            .list(crate::ListFilter::Beneficiary(ALICE), &mut cache, None)
            .await
            .unwrap();
        let v = views.iter().find(|v| v.account == s).unwrap();
        assert_eq!((v.claimable, v.locked, v.standing), (20_000, 80_000, Standing::Active));
        assert_eq!(v.next_unlock, NextUnlock::At(T0 + 20 * DAY + 86_400));
    });
}
