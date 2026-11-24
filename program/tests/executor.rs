//! The `vesting` program at executor level: the real guests (`vesting`, `vesting_fees`, the token program) run by
//! LEZ's public execution on a `V03State`, exactly as the sequencer runs a block, under the 10,000,000-cycle cap.
//!
//! Test names start with the requirement they prove (`docs/requirements.toml`). Every refusal asserts the exact
//! program code (SPEC 5), and that the refused transaction changed nothing.
//!
//! ```text
//! source scripts/env.sh && cargo test -p vesting_methods --test executor -- --nocapture --test-threads=1
//! ```
//!
//! Each transaction prints `cycles <operation> <asset> <n>`, the source of `evidence/cost/executor-cycles.txt`.
//! Under `RISC0_SKIP_BUILD=1` (`make test`) the guests are not built and every test here says so and returns.

mod common;

use common::*;
use lee::error::LeeError;
use lee_core::account::{AccountId, Actor};
use vesting_core::{
    Asset, BatchShape, BeneficiaryTransferred, Cancelled, Claimed, Event, Internal, Kind, MAX_BATCH_NATIVE,
    MadeNonCancelable, Message, MilestoneSignalled, ScheduleCreated, event_selector,
};
use vesting_program::{FEES_CONFIG_SEED, FEES_PROGRAM_ID, schedule_account};

const TOTAL: u128 = 1_000_000;
const START: u64 = T0;
const CLIFF: u64 = T0 + 10 * DAY;
const END: u64 = T0 + 100 * DAY;

macro_rules! chain {
    () => {
        match Chain::new(T0) {
            Some(chain) => chain,
            None => return,
        }
    };
}

fn cycles(op: &str, asset: &str, out: &Outcome) {
    println!("cycles {op} {asset} {}", out.cycles);
}

/// The refusal text of a failed transaction.
fn text(sent: &Sent) -> String {
    match sent {
        Err(LeeError::ProgramExecutionFailed(text)) => text.clone(),
        other => panic!("expected a program refusal, got {other:?}"),
    }
}

fn treasury(p: &Parties) -> AccountId {
    id(&p.treasury)
}

/// A chain with a native schedule `n` of [`TOTAL`] from the creator, linear with a cliff; returns its account.
fn native_schedule(chain: &mut Chain, p: &Parties, n: u32) -> AccountId {
    chain.fund_native(id(&p.creator), 10 * TOTAL);
    let terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);
    let out = chain.create(&p.creator, n, terms, 0, treasury(p)).expect("create");
    cycles("create", "native", &out);
    chain.schedule_of(&p.creator, n)
}

/// A token definition of `supply` held by the creator; returns the asset.
fn token_asset(chain: &mut Chain, p: &Parties, supply: u128) -> Asset {
    let def = chain.new_token(&key(0x20), &p.creator, supply);
    Asset::Token {
        definition: core_id(def),
    }
}

fn definition(asset: Asset) -> AccountId {
    match asset {
        Asset::Token { definition } => lez(definition),
        Asset::Native => panic!("not a token asset"),
    }
}

// ================================================================================================ F1: schedule kinds

#[test]
fn f1a_cliff_then_linear_lifecycle_native() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    let to = id(&p.recipient);
    assert_eq!(chain.native(schedule), TOTAL, "escrow holds exactly the total");
    assert_eq!(chain.native(id(&p.creator)), 9 * TOTAL);

    chain.tick(CLIFF - 1);
    let r = chain.claim(&p.beneficiary, schedule, to);
    assert_eq!(code(&r), 14);
    assert!(text(&r).contains(&format!("next unlock at {CLIFF}")), "{}", text(&r));

    chain.tick(CLIFF);
    let out = chain.claim(&p.beneficiary, schedule, to).expect("claim at the cliff");
    cycles("claim", "native", &out);
    assert_eq!(chain.native(to), TOTAL / 10, "the cliff lump: accrual since start");

    chain.tick(T0 + 55 * DAY);
    chain.claim(&p.beneficiary, schedule, to).expect("claim midway");
    assert_eq!(chain.native(to), TOTAL * 55 / 100);

    chain.tick(END);
    chain.claim(&p.beneficiary, schedule, to).expect("last claim");
    assert_eq!(chain.native(to), TOTAL);
    assert_eq!(chain.native(schedule), 0, "the last claim empties the escrow");
    let s = chain.schedule(schedule).unwrap();
    assert_eq!((s.claimed, s.last_time, s.pending), (TOTAL, END, None));

    let r = chain.claim(&p.beneficiary, schedule, to);
    assert_eq!(code(&r), 14);
    assert!(text(&r).contains("nothing left to vest"), "{}", text(&r));
}

#[test]
fn f1b_fully_linear_lifecycle_native() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, START, END);
    chain.create(&p.creator, 1, terms, 0, treasury(&p)).expect("create");
    let schedule = chain.schedule_of(&p.creator, 1);
    let to = id(&p.recipient);
    assert_eq!(code(&chain.claim(&p.beneficiary, schedule, to)), 14, "nothing at start");
    chain.tick(T0 + 25 * DAY);
    chain.claim(&p.beneficiary, schedule, to).expect("a quarter");
    assert_eq!(chain.native(to), TOTAL / 4);
    chain.tick(T0 + 25 * DAY + 864_000);
    chain
        .claim(&p.beneficiary, schedule, to)
        .expect("a ten-thousandth more");
    assert_eq!(
        chain.native(to),
        TOTAL / 4 + TOTAL / 10_000,
        "864,000 ms of a 100-day span"
    );
    chain.tick(END + DAY);
    chain.claim(&p.beneficiary, schedule, to).expect("the rest");
    assert_eq!((chain.native(to), chain.native(schedule)), (TOTAL, 0));
}

#[test]
fn f1c_milestones_lifecycle_native() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let terms = milestones(&p.beneficiary, Asset::Native, &[200_000, 300_000, 500_000]);
    chain.create(&p.creator, 1, terms, 0, treasury(&p)).expect("create");
    let schedule = chain.schedule_of(&p.creator, 1);
    let to = id(&p.recipient);
    let r = chain.claim(&p.beneficiary, schedule, to);
    assert_eq!(code(&r), 14);
    assert!(text(&r).contains("when a milestone is signalled"), "{}", text(&r));

    let out = chain
        .signal(&p.creator, schedule, 1)
        .expect("the creator signals by default");
    cycles("signal", "native", &out);
    chain.claim(&p.beneficiary, schedule, to).expect("claim milestone 1");
    assert_eq!(chain.native(to), 300_000);
    chain.signal(&p.creator, schedule, 0).expect("signal 0");
    chain.signal(&p.creator, schedule, 2).expect("signal 2");
    chain.claim(&p.beneficiary, schedule, to).expect("claim the rest");
    assert_eq!((chain.native(to), chain.native(schedule)), (TOTAL, 0));
    assert_eq!(code(&chain.signal(&p.creator, schedule, 3)), 17, "no milestone 3");
}

#[test]
fn f1a_cliff_then_linear_lifecycle_token() {
    let mut chain = chain!();
    let p = parties();
    let asset = token_asset(&mut chain, &p, 10 * TOTAL);
    let def = definition(asset);
    let terms = linear(&p.beneficiary, asset, TOTAL, START, CLIFF, END);
    let out = chain.create(&p.creator, 1, terms, 0, treasury(&p)).expect("create");
    cycles("create", "token", &out);
    let schedule = chain.schedule_of(&p.creator, 1);
    assert_eq!(
        chain.token(schedule),
        Some(TOTAL),
        "the program initialised and funded the escrow holding"
    );
    assert_eq!(chain.token(id(&p.creator)), Some(9 * TOTAL));

    let to = id(&p.recipient);
    chain.init_holding(&p.recipient, def);
    chain.tick(T0 + 40 * DAY);
    let out = chain.claim(&p.beneficiary, schedule, to).expect("token claim");
    cycles("claim", "token", &out);
    assert_eq!(chain.token(to), Some(TOTAL * 40 / 100));

    chain.tick(T0 + 70 * DAY);
    let out = chain.cancel(&p.creator, schedule).expect("token cancel");
    cycles("cancel", "token", &out);
    assert_eq!(
        chain.token(id(&p.creator)),
        Some(9 * TOTAL + TOTAL * 30 / 100),
        "unvested back to the source"
    );
    assert_eq!(chain.token(schedule), Some(TOTAL * 30 / 100), "vested, unclaimed stays");
    chain.tick(END);
    chain
        .claim(&p.beneficiary, schedule, to)
        .expect("claim the vested rest");
    assert_eq!(
        (chain.token(to), chain.token(schedule)),
        (Some(TOTAL * 70 / 100), Some(0))
    );
}

#[test]
fn f1c_milestones_lifecycle_token() {
    let mut chain = chain!();
    let p = parties();
    let asset = token_asset(&mut chain, &p, TOTAL);
    let terms = milestones(&p.beneficiary, asset, &[400_000, 600_000]);
    chain.create(&p.creator, 1, terms, 0, treasury(&p)).expect("create");
    let schedule = chain.schedule_of(&p.creator, 1);
    let to = id(&p.recipient);
    chain.init_holding(&p.recipient, definition(asset));
    chain.signal(&p.creator, schedule, 1).expect("signal");
    chain.claim(&p.beneficiary, schedule, to).expect("claim");
    assert_eq!(chain.token(to), Some(600_000));
}

// ================================================================================================ F2: claims

#[test]
fn f2_public_claim_pays_a_fresh_account_and_never_emits_it() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    let fresh = key(0x31);
    chain.evidence.push(key_evidence(&fresh));
    chain.tick(T0 + 30 * DAY);
    let out = chain
        .claim(&p.beneficiary, schedule, id(&fresh))
        .expect("claim to a never-seen account");
    assert_eq!(
        chain.native(id(&fresh)),
        TOTAL * 30 / 100,
        "credited in the same transaction"
    );
    let destination = id(&fresh).into_value();
    for event in &out.events {
        assert!(
            !event.event.data.windows(32).any(|w| w == destination),
            "an event carries the claim destination"
        );
    }
    let claimed: Claimed = out.one(chain.vesting);
    assert_eq!(claimed.beneficiary, cid(&p.beneficiary));
    let s = chain.schedule(schedule).unwrap();
    assert_eq!(s.pending, None, "the destination is not stored");
}

#[test]
fn f2_claim_as_of_window() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    let to = id(&p.recipient);
    let as_of = T0 + 50 * DAY;

    // A block before `as_of`: the validity window refuses the transaction, nothing moves.
    let r = chain.claim_as_of(&p.beneficiary, schedule, to, as_of, as_of - 1);
    assert!(matches!(r, Err(LeeError::OutOfValidityWindow)), "{r:?}");
    assert_eq!(chain.native(schedule), TOTAL);

    // A block at or after `as_of`: pays what was vested at `as_of`, whatever the clock says.
    let out = chain
        .claim_as_of(&p.beneficiary, schedule, to, as_of, as_of + 60_000)
        .expect("claim as of");
    cycles("claim_as_of", "native", &out);
    assert_eq!(chain.native(to), TOTAL / 2);
    let claimed: Claimed = out.one(chain.vesting);
    assert_eq!(
        (claimed.at, claimed.amount, claimed.claimed_total),
        (as_of, TOTAL / 2, TOTAL / 2)
    );
    assert_eq!(chain.schedule(schedule).unwrap().last_time, as_of);

    // The clock still reads T0: a public claim is computed at last_time (high-water) and finds nothing new.
    assert_eq!(code(&chain.claim(&p.beneficiary, schedule, to)), 14);
    // An older as_of is computed at last_time too.
    let r = chain.claim_as_of(&p.beneficiary, schedule, to, T0 + DAY, as_of + 60_000);
    assert_eq!(code(&r), 14);
}

#[test]
fn f2_claim_as_of_then_cancel_in_the_same_block_uses_the_later_time() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    chain.tick(T0 + 40 * DAY);
    // Next block's own timestamp is 41 days; the clock still reads 40 days.
    let as_of = T0 + 41 * DAY;
    chain
        .claim_as_of(&p.beneficiary, schedule, id(&p.recipient), as_of, as_of)
        .expect("claim ahead of the clock");
    let out = chain.cancel(&p.creator, schedule).expect("the cancel is not blocked");
    let cancelled: Cancelled = out.one(chain.vesting);
    assert_eq!(cancelled.at, as_of, "computed at the claim's time, never earlier");
    assert_eq!(cancelled.vested_at_cancel, TOTAL * 41 / 100);
    assert_eq!(
        chain.native(schedule),
        0,
        "everything vested was claimed, the rest refunded"
    );
}

// ================================================================================================ F3: cancellation

#[test]
fn f3_cancel_returns_unvested_and_keeps_vested_claimable() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    let to = id(&p.recipient);
    chain.tick(T0 + 20 * DAY);
    chain.claim(&p.beneficiary, schedule, to).expect("claim 20%");
    chain.tick(T0 + 40 * DAY);
    let creator_before = chain.native(id(&p.creator));
    let out = chain.cancel(&p.creator, schedule).expect("cancel");
    cycles("cancel", "native", &out);
    let cancelled: Cancelled = out.one(chain.vesting);
    assert_eq!(
        (
            cancelled.at,
            cancelled.vested_at_cancel,
            cancelled.refunded,
            cancelled.refund_to
        ),
        (T0 + 40 * DAY, TOTAL * 40 / 100, TOTAL * 60 / 100, cid(&p.creator))
    );
    assert_eq!(
        chain.native(id(&p.creator)),
        creator_before + TOTAL * 60 / 100,
        "unvested back to the source"
    );
    assert_eq!(
        chain.native(schedule),
        TOTAL * 20 / 100,
        "vested, unclaimed stays in escrow"
    );

    // Time keeps running, vesting does not; the beneficiary collects what had vested.
    chain.tick(END);
    chain.claim(&p.beneficiary, schedule, to).expect("claim after cancel");
    assert_eq!((chain.native(to), chain.native(schedule)), (TOTAL * 40 / 100, 0));
    let r = chain.claim(&p.beneficiary, schedule, to);
    assert_eq!(code(&r), 14);
    assert!(text(&r).contains("nothing left to vest"));
}

#[test]
fn f3_cancel_with_nothing_unvested_makes_no_transfer() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    chain.tick(END);
    let creator_before = chain.native(id(&p.creator));
    let out = chain
        .cancel(&p.creator, schedule)
        .expect("a cancel with zero refund is valid");
    let cancelled: Cancelled = out.one(chain.vesting);
    assert_eq!((cancelled.vested_at_cancel, cancelled.refunded), (TOTAL, 0));
    assert_eq!(chain.native(id(&p.creator)), creator_before);
    assert_eq!(chain.native(schedule), TOTAL, "all of it still claimable");
    chain
        .claim(&p.beneficiary, schedule, id(&p.recipient))
        .expect("claim everything");
    assert_eq!(chain.native(id(&p.recipient)), TOTAL);
}

#[test]
fn f3_cancel_refunds_an_explicit_refund_account() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let mut terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);
    terms.refund_to = Some(cid(&p.refund));
    chain.create(&p.creator, 1, terms, 0, treasury(&p)).expect("create");
    let schedule = chain.schedule_of(&p.creator, 1);
    chain.tick(T0 + 30 * DAY);
    chain.cancel(&p.creator, schedule).expect("cancel");
    assert_eq!(chain.native(id(&p.refund)), TOTAL * 70 / 100);
    assert_eq!(chain.native(id(&p.creator)), 0);
}

#[test]
fn f3_make_non_cancelable_is_one_way() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    let out = chain
        .make_non_cancelable(&p.creator, schedule)
        .expect("make non-cancelable");
    cycles("make_non_cancelable", "native", &out);
    let event: MadeNonCancelable = out.one(chain.vesting);
    assert_eq!((event.schedule, event.by), (core_id(schedule), cid(&p.creator)));
    assert!(!chain.schedule(schedule).unwrap().cancelable);

    let before = chain.schedule(schedule);
    chain.tick(T0 + 50 * DAY);
    assert_eq!(code(&chain.cancel(&p.creator, schedule)), 15);
    assert_eq!(
        code(&chain.make_non_cancelable(&p.creator, schedule)),
        15,
        "already non-cancelable"
    );
    assert_eq!(chain.schedule(schedule), before, "refusals change nothing");
    assert_eq!(chain.native(schedule), TOTAL);
}

#[test]
fn f3_a_cancelled_schedule_refuses_cancel_signal_and_make_non_cancelable() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let terms = milestones(&p.beneficiary, Asset::Native, &[500_000, 500_000]);
    chain.create(&p.creator, 1, terms, 0, treasury(&p)).expect("create");
    let schedule = chain.schedule_of(&p.creator, 1);
    chain.signal(&p.creator, schedule, 0).expect("signal");
    chain.cancel(&p.creator, schedule).expect("cancel");
    assert_eq!(
        chain.native(id(&p.creator)),
        500_000,
        "the unsignalled milestone is refunded"
    );
    assert_eq!(code(&chain.cancel(&p.creator, schedule)), 16);
    assert_eq!(code(&chain.signal(&p.creator, schedule, 1)), 16);
    assert_eq!(code(&chain.make_non_cancelable(&p.creator, schedule)), 16);
    chain
        .claim(&p.beneficiary, schedule, id(&p.recipient))
        .expect("the signalled milestone stays claimable");
    assert_eq!(chain.native(id(&p.recipient)), 500_000);
}

// ================================================================================================ F4: transfer

#[test]
fn f4_transfer_moves_the_claim_right_when_transferable() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let mut terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);
    terms.transferable = true;
    chain.create(&p.creator, 1, terms, 0, treasury(&p)).expect("create");
    let schedule = chain.schedule_of(&p.creator, 1);
    chain.tick(T0 + 30 * DAY);
    chain
        .claim(&p.beneficiary, schedule, id(&p.recipient))
        .expect("claim before the transfer");

    let out = chain.transfer(&p.beneficiary, schedule, id(&p.heir)).expect("transfer");
    cycles("transfer", "native", &out);
    let event: BeneficiaryTransferred = out.one(chain.vesting);
    assert_eq!(
        (event.schedule, event.from, event.to),
        (core_id(schedule), cid(&p.beneficiary), cid(&p.heir))
    );
    chain.tick(T0 + 50 * DAY);
    assert_eq!(
        code(&chain.claim(&p.beneficiary, schedule, id(&p.recipient))),
        3,
        "the old beneficiary"
    );
    assert_eq!(code(&chain.transfer(&p.beneficiary, schedule, id(&p.beneficiary))), 3);
    chain
        .claim(&p.heir, schedule, id(&p.heir))
        .expect("the new beneficiary claims");
    assert_eq!(
        chain.native(id(&p.heir)),
        TOTAL * 20 / 100,
        "only what was not yet claimed"
    );
}

#[test]
fn f4_transfer_refused_when_not_transferable() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    let before = chain.schedule(schedule);
    assert_eq!(code(&chain.transfer(&p.beneficiary, schedule, id(&p.heir))), 19);
    assert_eq!(code(&chain.transfer(&p.stranger, schedule, id(&p.stranger))), 3);
    assert_eq!(chain.schedule(schedule), before);
}

// ================================================================================================ F5: batch

#[test]
fn f5_batch_native_gives_each_item_its_own_escrow() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), 10 * TOTAL);
    let items = batch_items(0, 5, 0)
        .into_iter()
        .enumerate()
        .map(|(i, mut item)| {
            item.total = 1_000 + i as u128;
            item
        })
        .collect::<Vec<_>>();
    let out = chain
        .create_batch(
            &p.creator,
            batch_linear(Asset::Native),
            items.clone(),
            0,
            treasury(&p),
            CYCLE_CAP,
        )
        .expect("batch");
    cycles("create_batch_5", "native", &out);
    let created: Vec<_> = out
        .vesting_events(chain.vesting)
        .into_iter()
        .filter(|e| e.event.selector == ScheduleCreated::SELECTOR)
        .map(|e| borsh::from_slice::<ScheduleCreated>(&e.event.data).unwrap())
        .collect();
    assert_eq!(created.len(), 5, "one ScheduleCreated per item");
    let mut accounts = Vec::new();
    for (item, event) in items.iter().zip(&created) {
        let schedule = schedule_account(&chain.vesting, &cid(&p.creator), &item.schedule_id);
        assert_eq!(chain.native(schedule), item.total, "own escrow, own amount");
        let s = chain.schedule(schedule).unwrap();
        assert_eq!(
            (s.beneficiary, s.total, s.schedule_id),
            (item.beneficiary, item.total, item.schedule_id)
        );
        assert_eq!((event.schedule, event.total), (core_id(schedule), item.total));
        accounts.push(schedule);
    }
    accounts.sort_unstable();
    accounts.dedup();
    assert_eq!(accounts.len(), 5, "no account shared between schedules");
    assert_eq!(
        chain.native(id(&p.creator)),
        10 * TOTAL - (1_000 + 1_001 + 1_002 + 1_003 + 1_004)
    );
}

#[test]
fn f5_batch_duplicate_id_refused() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let mut items = batch_items(0, 3, 100);
    items[2].schedule_id = items[0].schedule_id;
    let r = chain.create_batch(
        &p.creator,
        batch_linear(Asset::Native),
        items,
        0,
        treasury(&p),
        CYCLE_CAP,
    );
    assert_eq!(code(&r), 7);
    assert_eq!(chain.native(id(&p.creator)), TOTAL);
    assert_eq!(chain.schedule(chain.schedule_of(&p.creator, 0)), None, "nothing opened");
}

#[test]
fn f5_batch_empty_or_above_the_cap_refused() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let r = chain.create_batch(
        &p.creator,
        batch_linear(Asset::Native),
        Vec::new(),
        0,
        treasury(&p),
        CYCLE_CAP,
    );
    assert_eq!(code(&r), 24);
    let n = u32::try_from(MAX_BATCH_NATIVE).unwrap() + 1;
    let r = chain.create_batch(
        &p.creator,
        batch_linear(Asset::Native),
        batch_items(0, n, 1),
        0,
        treasury(&p),
        CYCLE_CAP,
    );
    assert_eq!(code(&r), 25);
    assert_eq!(chain.native(id(&p.creator)), TOTAL);
}

#[test]
fn f5_batch_milestones_split_by_weights() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let mut common = batch_linear(Asset::Native);
    common.shape = BatchShape::Milestones {
        bps: vec![3_333, 3_333, 3_334],
    };
    let items = batch_items(0, 2, 1_000);
    chain
        .create_batch(&p.creator, common, items, 0, treasury(&p), CYCLE_CAP)
        .expect("batch");
    let s = chain.schedule(chain.schedule_of(&p.creator, 1)).unwrap();
    assert_eq!(
        s.kind,
        Kind::Milestones {
            amounts: vec![333, 333, 334],
            signalled: 0
        }
    );
}

#[test]
fn f5_batch_token_gives_each_item_its_own_escrow() {
    let mut chain = chain!();
    let p = parties();
    let asset = token_asset(&mut chain, &p, TOTAL);
    let out = chain
        .create_batch(
            &p.creator,
            batch_linear(asset),
            batch_items(0, 3, 1_000),
            0,
            treasury(&p),
            CYCLE_CAP,
        )
        .expect("token batch");
    cycles("create_batch_3", "token", &out);
    for n in 0..3 {
        assert_eq!(chain.token(chain.schedule_of(&p.creator, n)), Some(1_000));
    }
    assert_eq!(chain.token(id(&p.creator)), Some(TOTAL - 3_000));
}

#[test]
fn f5_batch_at_the_cap_fits_the_chain_budget_native() {
    let mut chain = chain!();
    let p = parties();
    let n = u32::try_from(vesting_core::MAX_BATCH_NATIVE).unwrap();
    chain.fund_native(id(&p.creator), u128::from(n) * TOTAL);
    let out = chain
        .create_batch(
            &p.creator,
            batch_linear(Asset::Native),
            batch_items(0, n, TOTAL),
            0,
            treasury(&p),
            CYCLE_CAP,
        )
        .expect("a batch at MAX_BATCH_NATIVE fits the 10M-cycle budget");
    println!("cycles create_batch_{n} native {}", out.cycles);
    assert_eq!(chain.native(chain.schedule_of(&p.creator, n - 1)), TOTAL);
}

#[test]
fn f5_batch_at_the_cap_fits_the_chain_budget_token() {
    let mut chain = chain!();
    let p = parties();
    let n = u32::try_from(vesting_core::MAX_BATCH_TOKEN).unwrap();
    let asset = token_asset(&mut chain, &p, u128::from(n) * TOTAL);
    let out = chain
        .create_batch(
            &p.creator,
            batch_linear(asset),
            batch_items(0, n, TOTAL),
            0,
            treasury(&p),
            CYCLE_CAP,
        )
        .expect("a batch at MAX_BATCH_TOKEN fits the 10M-cycle budget");
    println!("cycles create_batch_{n} token {}", out.cycles);
    assert_eq!(chain.token(chain.schedule_of(&p.creator, n - 1)), Some(TOTAL));
}

/// 85% of the largest batch of the heaviest items the chain accepts (64 milestones each, with a fee): 61 native and
/// 38 token fit, 62 and 39 do not (`batch_limit.rs`, `measure_batch_max_*_heaviest`).
const HEAVIEST_NATIVE: u32 = 51;
const HEAVIEST_TOKEN: u32 = 32;

#[test]
fn f5_batch_of_the_heaviest_items_fits_native() {
    let mut chain = chain!();
    let p = parties();
    let n = HEAVIEST_NATIVE;
    chain.set_fee_config(100, treasury(&p));
    chain.fund_native(id(&p.creator), 2 * u128::from(n) * TOTAL);
    let fee = u128::from(n) * TOTAL / 100;
    let out = chain
        .create_batch(
            &p.creator,
            batch_heaviest(Asset::Native),
            batch_items(0, n, TOTAL),
            fee,
            treasury(&p),
            CYCLE_CAP,
        )
        .expect("64-milestone items with a fee");
    println!("cycles create_batch_{n}_64_milestones_with_fee native {}", out.cycles);
    assert_eq!(chain.native(treasury(&p)), fee);
}

#[test]
fn f5_batch_of_the_heaviest_items_fits_token() {
    let mut chain = chain!();
    let p = parties();
    let n = HEAVIEST_TOKEN;
    let asset = token_asset(&mut chain, &p, 2 * u128::from(n) * TOTAL);
    let ata = chain.put_treasury_ata(treasury(&p), definition(asset));
    chain.set_fee_config(100, treasury(&p));
    let fee = u128::from(n) * TOTAL / 100;
    let out = chain
        .create_batch(
            &p.creator,
            batch_heaviest(asset),
            batch_items(0, n, TOTAL),
            fee,
            treasury(&p),
            CYCLE_CAP,
        )
        .expect("64-milestone items with a fee");
    println!("cycles create_batch_{n}_64_milestones_with_fee token {}", out.cycles);
    assert_eq!(chain.token(ata), Some(fee));
}

#[test]
fn r2_batch_over_the_budget_reverts_whole() {
    // At the cap, 64-milestone items cost more than the chain's budget: the chain refuses the batch, nothing opens.
    let mut chain = chain!();
    let p = parties();
    let n = u32::try_from(vesting_core::MAX_BATCH_NATIVE).unwrap();
    chain.fund_native(id(&p.creator), u128::from(n) * TOTAL);
    let r = chain.create_batch(
        &p.creator,
        batch_heaviest(Asset::Native),
        batch_items(0, n, TOTAL),
        0,
        treasury(&p),
        CYCLE_CAP,
    );
    assert!(matches!(r, Err(LeeError::OutOfGas { .. })), "{r:?}");
    assert_eq!(chain.native(id(&p.creator)), u128::from(n) * TOTAL);
    assert_eq!(chain.schedule(chain.schedule_of(&p.creator, 0)), None);
    assert_eq!(chain.native(chain.schedule_of(&p.creator, 0)), 0);
}

// ================================================================================================ F6: events

#[test]
fn f6_events_every_event_with_its_selector_and_schema() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let mut terms = milestones(&p.beneficiary, Asset::Native, &[100_000, 900_000]);
    terms.transferable = true;
    terms.cancel_authority = Some(cid(&p.canceller));
    terms.milestone_authority = Some(cid(&p.signaller));
    let schedule = chain.schedule_of(&p.creator, 1);
    let s = core_id(schedule);

    let out = chain.create(&p.creator, 1, terms, 0, treasury(&p)).expect("create");
    let created: ScheduleCreated = out.one(chain.vesting);
    assert_eq!(
        created,
        ScheduleCreated {
            schedule: s,
            schedule_id: sid(1),
            creator: cid(&p.creator),
            beneficiary: cid(&p.beneficiary),
            asset: Asset::Native,
            total: TOTAL,
            kind: Kind::Milestones {
                amounts: vec![100_000, 900_000],
                signalled: 0
            },
            cancelable: true,
            transferable: true,
            cancel_authority: cid(&p.canceller),
            milestone_authority: cid(&p.signaller),
            refund_to: cid(&p.creator),
        }
    );

    let out = chain.signal(&p.signaller, schedule, 0).expect("signal");
    let signalled: MilestoneSignalled = out.one(chain.vesting);
    assert_eq!(
        signalled,
        MilestoneSignalled {
            schedule: s,
            index: 0,
            amount: 100_000,
            signalled: 0b1
        }
    );

    chain.tick(T0 + 5_000);
    let out = chain.claim(&p.beneficiary, schedule, id(&p.recipient)).expect("claim");
    let claimed: Claimed = out.one(chain.vesting);
    assert_eq!(
        claimed,
        Claimed {
            schedule: s,
            beneficiary: cid(&p.beneficiary),
            amount: 100_000,
            at: T0 + 5_000,
            claimed_total: 100_000
        }
    );

    let out = chain.transfer(&p.beneficiary, schedule, id(&p.heir)).expect("transfer");
    let transferred: BeneficiaryTransferred = out.one(chain.vesting);
    assert_eq!(
        transferred,
        BeneficiaryTransferred {
            schedule: s,
            from: cid(&p.beneficiary),
            to: cid(&p.heir)
        }
    );

    let out = chain.cancel(&p.canceller, schedule).expect("cancel");
    let cancelled: Cancelled = out.one(chain.vesting);
    assert_eq!(
        cancelled,
        Cancelled {
            schedule: s,
            at: T0 + 5_000,
            vested_at_cancel: 100_000,
            refunded: 900_000,
            refund_to: cid(&p.creator)
        }
    );

    // A second schedule for the last event.
    chain.fund_native(id(&p.creator), TOTAL);
    let terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);
    chain.create(&p.creator, 2, terms, 0, treasury(&p)).expect("create 2");
    let second = chain.schedule_of(&p.creator, 2);
    let out = chain
        .make_non_cancelable(&p.creator, second)
        .expect("make non-cancelable");
    let made: MadeNonCancelable = out.one(chain.vesting);
    assert_eq!(
        made,
        MadeNonCancelable {
            schedule: core_id(second),
            by: cid(&p.creator)
        }
    );

    // Selectors: the first 8 bytes of SHA-256("vesting::<EventName>"), and emitted by the vesting program.
    for (name, selector) in [
        (ScheduleCreated::NAME, ScheduleCreated::SELECTOR),
        (Claimed::NAME, Claimed::SELECTOR),
        (Cancelled::NAME, Cancelled::SELECTOR),
        (MilestoneSignalled::NAME, MilestoneSignalled::SELECTOR),
        (BeneficiaryTransferred::NAME, BeneficiaryTransferred::SELECTOR),
        (MadeNonCancelable::NAME, MadeNonCancelable::SELECTOR),
    ] {
        assert_eq!(selector, event_selector(name), "{name}");
    }
    assert!(out.events.iter().all(|e| e.account_id == chain.vesting));
}

// ================================================================================================ R: reliability

#[test]
fn r1_failed_claim_leaves_state() {
    let mut chain = chain!();
    let p = parties();
    let asset = token_asset(&mut chain, &p, TOTAL);
    chain
        .create(
            &p.creator,
            1,
            linear(&p.beneficiary, asset, TOTAL, START, CLIFF, END),
            0,
            treasury(&p),
        )
        .expect("create");
    let schedule = chain.schedule_of(&p.creator, 1);
    let to = id(&p.recipient);
    let before = chain.schedule(schedule);

    // Nothing claimable: refused with 14, nothing changes.
    assert_eq!(code(&chain.claim(&p.beneficiary, schedule, to)), 14);
    assert_eq!(chain.schedule(schedule), before);

    // A destination with no holding of the token: the token program refuses the credit, the whole claim reverts.
    chain.tick(T0 + 50 * DAY);
    let r = chain.claim(&p.beneficiary, schedule, to);
    assert!(r.is_err(), "{r:?}");
    assert_eq!(chain.schedule(schedule), before, "nothing consumed");
    assert_eq!(chain.token(schedule), Some(TOTAL));

    // The owner initialises its holding in a transaction of its own; the same claim then succeeds.
    chain.init_holding(&p.recipient, definition(asset));
    chain.claim(&p.beneficiary, schedule, to).expect("retry");
    assert_eq!(chain.token(to), Some(TOTAL / 2));
}

#[test]
fn r2_cancel_atomic() {
    let mut chain = chain!();
    let p = parties();
    let asset = token_asset(&mut chain, &p, TOTAL);
    let mut terms = linear(&p.beneficiary, asset, TOTAL, START, CLIFF, END);
    terms.refund_to = Some(cid(&p.refund));
    chain.create(&p.creator, 1, terms, 0, treasury(&p)).expect("create");
    let schedule = chain.schedule_of(&p.creator, 1);
    chain.tick(T0 + 30 * DAY);
    let before = chain.schedule(schedule);

    // The refund account has no holding: the refund fails, and with it the whole cancel.
    let r = chain.cancel(&p.creator, schedule);
    assert!(r.is_err(), "{r:?}");
    assert_eq!(chain.schedule(schedule), before, "not cancelled, nothing pending");
    assert_eq!(chain.token(schedule), Some(TOTAL));

    chain.init_holding(&p.refund, definition(asset));
    chain.cancel(&p.creator, schedule).expect("cancel");
    assert_eq!(chain.token(id(&p.refund)), Some(TOTAL * 70 / 100));
    assert_eq!(chain.token(schedule), Some(TOTAL * 30 / 100));
    let s = chain.schedule(schedule).unwrap();
    assert_eq!(s.cancelled.map(|c| c.refunded), Some(TOTAL * 70 / 100));
    assert_eq!(
        s.claimed + s.cancelled.unwrap().refunded + chain.token(schedule).unwrap(),
        TOTAL
    );
}

#[test]
fn r3_independent_schedules() {
    let mut chain = chain!();
    let p = parties();
    let a = native_schedule(&mut chain, &p, 1);
    let terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);
    chain.create(&p.creator, 2, terms, 0, treasury(&p)).expect("create b");
    let b = chain.schedule_of(&p.creator, 2);
    assert_ne!(a, b);
    let b_before = chain.schedule(b);

    chain.tick(T0 + 50 * DAY);
    chain.claim(&p.beneficiary, a, id(&p.recipient)).expect("claim a");
    chain.cancel(&p.creator, a).expect("cancel a");
    assert_eq!(chain.schedule(b), b_before, "b untouched by a's claim and cancel");
    assert_eq!(chain.native(b), TOTAL);
    chain.claim(&p.beneficiary, b, id(&p.recipient)).expect("claim b");
    assert_eq!(chain.native(b), TOTAL / 2);
}

#[test]
fn r4_milestone_idempotent() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    chain
        .create(
            &p.creator,
            1,
            milestones(&p.beneficiary, Asset::Native, &[250_000, 750_000]),
            0,
            treasury(&p),
        )
        .expect("create");
    let schedule = chain.schedule_of(&p.creator, 1);
    chain.signal(&p.creator, schedule, 1).expect("signal");
    let before = chain.schedule(schedule);
    assert_eq!(code(&chain.signal(&p.creator, schedule, 1)), 18);
    assert_eq!(chain.schedule(schedule), before);
    chain.claim(&p.beneficiary, schedule, id(&p.recipient)).expect("claim");
    assert_eq!(chain.native(id(&p.recipient)), 750_000, "unlocked once");
    assert_eq!(code(&chain.claim(&p.beneficiary, schedule, id(&p.recipient))), 14);
}

// ================================================================================================ soft requirements

#[test]
fn soft1_cancel_authority() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), 2 * TOTAL);
    let mut terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);
    terms.cancel_authority = Some(cid(&p.canceller));
    chain
        .create(&p.creator, 1, terms.clone(), 0, treasury(&p))
        .expect("create");
    chain.create(&p.creator, 2, terms, 0, treasury(&p)).expect("create 2");
    let schedule = chain.schedule_of(&p.creator, 1);
    chain.tick(T0 + 50 * DAY);
    assert_eq!(
        code(&chain.cancel(&p.creator, schedule)),
        5,
        "the creator is not the cancel authority"
    );
    assert_eq!(code(&chain.cancel(&p.beneficiary, schedule)), 5);
    chain
        .cancel(&p.canceller, schedule)
        .expect("the cancel authority cancels");
    assert_eq!(
        chain.native(id(&p.creator)),
        TOTAL / 2,
        "refund to the source, not to the authority"
    );

    // MakeNonCancelable: the creator or the cancel authority, nobody else.
    let second = chain.schedule_of(&p.creator, 2);
    assert_eq!(code(&chain.make_non_cancelable(&p.stranger, second)), 4);
    assert_eq!(code(&chain.make_non_cancelable(&p.beneficiary, second)), 4);
    chain
        .make_non_cancelable(&p.canceller, second)
        .expect("the cancel authority may");
}

#[test]
fn soft2_milestone_authority() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let mut terms = milestones(&p.beneficiary, Asset::Native, &[TOTAL]);
    terms.milestone_authority = Some(cid(&p.signaller));
    chain.create(&p.creator, 1, terms, 0, treasury(&p)).expect("create");
    let schedule = chain.schedule_of(&p.creator, 1);
    assert_eq!(
        code(&chain.signal(&p.creator, schedule, 0)),
        6,
        "the creator is not the milestone authority"
    );
    assert_eq!(code(&chain.signal(&p.beneficiary, schedule, 0)), 6);
    chain
        .signal(&p.signaller, schedule, 0)
        .expect("the milestone authority signals");

    // A linear schedule has no milestones.
    chain.fund_native(id(&p.creator), TOTAL);
    chain
        .create(
            &p.creator,
            2,
            linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END),
            0,
            treasury(&p),
        )
        .expect("create 2");
    assert_eq!(code(&chain.signal(&p.creator, chain.schedule_of(&p.creator, 2), 0)), 20);
}

// ================================================================================================ creation refusals

#[test]
fn create_refusals_carry_their_codes() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), 10 * TOTAL);
    let t = treasury(&p);
    let base = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);

    let mut zero = base.clone();
    zero.total = 0;
    assert_eq!(code(&chain.create(&p.creator, 1, zero, 0, t)), 10);
    let bad_times = linear(&p.beneficiary, Asset::Native, TOTAL, END, END, END);
    assert_eq!(code(&chain.create(&p.creator, 1, bad_times, 0, t)), 11);
    let mut bad_sum = milestones(&p.beneficiary, Asset::Native, &[1, 2]);
    bad_sum.total = 4;
    assert_eq!(code(&chain.create(&p.creator, 1, bad_sum, 0, t)), 12);
    let too_many = milestones(&p.beneficiary, Asset::Native, &[1; 65]);
    assert_eq!(code(&chain.create(&p.creator, 1, too_many, 0, t)), 13);
    assert_eq!(
        code(&chain.create(&p.creator, 1, base.clone(), TOTAL / 100 + 1, t)),
        28,
        "above the 1% cap"
    );
    assert_eq!(chain.native(id(&p.creator)), 10 * TOTAL, "nothing moved");

    chain.create(&p.creator, 1, base.clone(), 0, t).expect("create");
    let r = chain.create(&p.creator, 1, base, 0, t);
    assert_eq!(code(&r), 7, "a (creator, schedule id) pair is never reused");
    assert_eq!(chain.native(id(&p.creator)), 9 * TOTAL);
}

// ================================================================================================ fees

#[test]
fn fee_zero_by_default_with_any_treasury() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), TOTAL);
    let terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);
    chain
        .create(&p.creator, 1, terms, 0, id(&p.stranger))
        .expect("fee 0 passes with any treasury");
    assert_eq!(chain.native(id(&p.creator)), 0);
}

#[test]
fn fee_mismatch_refused_by_the_fee_program() {
    let mut chain = chain!();
    let p = parties();
    chain.fund_native(id(&p.creator), 2 * TOTAL);
    let terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);
    // Rate 0: a quoted fee of 1 (under the cap) is refused by vesting_fees.
    assert_eq!(code(&chain.create(&p.creator, 1, terms.clone(), 1, treasury(&p))), 27);
    // Rate 1%: fee 0, a wrong amount, or the wrong treasury are refused.
    chain.set_fee_config(100, treasury(&p));
    assert_eq!(code(&chain.create(&p.creator, 1, terms.clone(), 0, treasury(&p))), 27);
    assert_eq!(
        code(&chain.create(&p.creator, 1, terms.clone(), TOTAL / 100 - 1, treasury(&p))),
        27
    );
    assert_eq!(
        code(&chain.create(&p.creator, 1, terms, TOTAL / 100, id(&p.stranger))),
        27
    );
    assert_eq!(chain.native(id(&p.creator)), 2 * TOTAL, "nothing moved");
}

#[test]
fn fee1_fee2_fee4_native_fee_paid_by_the_creator_at_creation_to_the_treasury() {
    let mut chain = chain!();
    let p = parties();
    chain.set_fee_config(100, treasury(&p));
    chain.fund_native(id(&p.creator), 2 * TOTAL);
    let terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);
    let out = chain
        .create(&p.creator, 1, terms, TOTAL / 100, treasury(&p))
        .expect("create with a fee");
    cycles("create_with_fee", "native", &out);
    assert_eq!(chain.native(treasury(&p)), TOTAL / 100);
    assert_eq!(chain.native(id(&p.creator)), 2 * TOTAL - TOTAL - TOTAL / 100);
    assert_eq!(
        chain.native(chain.schedule_of(&p.creator, 1)),
        TOTAL,
        "the escrow holds the full total"
    );

    // Batch: one fee on the sum of item totals.
    let out = chain
        .create_batch(
            &p.creator,
            batch_linear(Asset::Native),
            batch_items(10, 4, 10_000),
            400,
            treasury(&p),
            CYCLE_CAP,
        )
        .expect("batch with a fee");
    cycles("create_batch_4_with_fee", "native", &out);
    assert_eq!(chain.native(treasury(&p)), TOTAL / 100 + 400);
}

#[test]
fn fee1_fee2_fee4_token_fee_paid_by_the_creator_at_creation_to_the_treasury_ata() {
    let mut chain = chain!();
    let p = parties();
    let asset = token_asset(&mut chain, &p, 2 * TOTAL);
    let ata = chain.put_treasury_ata(treasury(&p), definition(asset));
    chain.set_fee_config(50, treasury(&p));
    let terms = linear(&p.beneficiary, asset, TOTAL, START, CLIFF, END);
    let fee = TOTAL * 50 / 10_000;
    let out = chain
        .create(&p.creator, 1, terms, fee, treasury(&p))
        .expect("token create with a fee");
    cycles("create_with_fee", "token", &out);
    assert_eq!(
        chain.token(ata),
        Some(fee),
        "the fee lands on the treasury's associated token account"
    );
    assert_eq!(chain.token(id(&p.creator)), Some(2 * TOTAL - TOTAL - fee));
    assert_eq!(chain.token(chain.schedule_of(&p.creator, 1)), Some(TOTAL));
}

#[test]
fn fee_check_passes_before_the_fee_program_is_initialised() {
    // An empty config (before `Init`) reads as rate 0: creation with fee 0 works, any other fee is refused.
    let Some(mut chain) = Chain::with_programs(T0, &[(VESTING, vesting_methods::VESTING_ELF)]) else {
        return;
    };
    let p = parties();
    chain.evidence.push(lee::PublicAccountEvidence::Pda {
        program: FEES_PROGRAM_ID,
        seed: FEES_CONFIG_SEED,
    });
    chain.fund_native(id(&p.creator), TOTAL);
    let terms = linear(&p.beneficiary, Asset::Native, TOTAL / 2, START, CLIFF, END);
    assert_eq!(code(&chain.create(&p.creator, 1, terms.clone(), 1, treasury(&p))), 27);
    chain
        .create(&p.creator, 1, terms, 0, treasury(&p))
        .expect("fee 0 before Init");
}

// ================================================================================================ attacks

#[test]
fn attack_stranger_claims() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    chain.tick(END);
    // Through its own actor: not the beneficiary.
    assert_eq!(code(&chain.claim(&p.stranger, schedule, id(&p.stranger))), 3);
    // Through the beneficiary's actor without the beneficiary's signature.
    let ben = id(&p.beneficiary);
    let declared = [
        chain.seat(ben),
        Actor::new(schedule, chain.vesting),
        clock_core::clock_actor(),
        Actor::native_balance(schedule),
        Actor::native_balance(id(&p.stranger)),
    ];
    let msg = Message::Claim {
        schedule: core_id(schedule),
        to: cid(&p.stranger),
    };
    let r = chain.submit(chain.seat(ben), &declared, &[&p.stranger], &msg);
    assert_eq!(code(&r), 1);
    // The creator and the cancel authority cannot claim either.
    assert_eq!(code(&chain.claim(&p.creator, schedule, id(&p.creator))), 3);
    assert_eq!(chain.native(schedule), TOTAL);
}

#[test]
fn attack_message_straight_to_the_schedule() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    chain.tick(END);
    let declared = [
        Actor::new(schedule, chain.vesting),
        clock_core::clock_actor(),
        Actor::native_balance(schedule),
        Actor::native_balance(id(&p.recipient)),
    ];
    let msg = Message::Claim {
        schedule: core_id(schedule),
        to: cid(&p.recipient),
    };
    let r = chain.submit(Actor::new(schedule, chain.vesting), &declared, &[&p.beneficiary], &msg);
    assert_eq!(code(&r), 23);
    // An internal message sent by a transaction is decoded as a user message and refused the same way.
    let r = chain.submit(
        Actor::new(schedule, chain.vesting),
        &declared,
        &[&p.beneficiary],
        &Internal::ClaimFor { to: cid(&p.recipient) },
    );
    assert_eq!(code(&r), 23);
    assert_eq!(chain.native(schedule), TOTAL);
}

#[test]
fn attack_forged_open() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    // Overwrite an existing schedule with a forged state (new beneficiary).
    let mut forged = chain.schedule(schedule).unwrap();
    forged.beneficiary = cid(&p.stranger);
    let open = Internal::Open { state: forged.clone() };
    let here = Actor::new(schedule, chain.vesting);
    let r = chain.submit(here, &[here], &[&p.stranger], &open);
    assert_eq!(code(&r), 23);
    assert_eq!(chain.schedule(schedule).unwrap().beneficiary, cid(&p.beneficiary));

    // Open a fresh schedule account with no escrow behind it: the seed is public, the program still refuses.
    forged.schedule_id = sid(99);
    let fresh = schedule_account(&chain.vesting, &forged.creator, &sid(99));
    let seed = vesting_program::schedule_seed(&forged.creator, &sid(99));
    chain.evidence.push(lee::PublicAccountEvidence::Pda {
        program: chain.vesting,
        seed,
    });
    let there = Actor::new(fresh, chain.vesting);
    let r = chain.submit(there, &[there], &[&p.stranger], &Internal::Open { state: forged });
    let c = code(&r);
    assert!(
        c == 2 || c == 1,
        "a transaction's Open decodes as a user message at an account it cannot sign for: {c}"
    );
    assert_eq!(chain.schedule(fresh), None);
}

#[test]
fn attack_wrong_program_copy() {
    // The same ELF deployed at a second id is a different program: its actor on the beneficiary's account forwards
    // to (schedule, copy), which holds no schedule.
    let Some(mut chain) = Chain::with_programs(
        T0,
        &[
            (VESTING, vesting_methods::VESTING_ELF),
            (COPY, vesting_methods::VESTING_ELF),
        ],
    ) else {
        return;
    };
    chain.set_fee_config(0, AccountId::new([0; 32]));
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    chain.tick(END);
    let ben = id(&p.beneficiary);
    let declared = [
        Actor::new(ben, COPY),
        Actor::new(schedule, COPY),
        Actor::new(schedule, VESTING),
        clock_core::clock_actor(),
        Actor::native_balance(schedule),
        Actor::native_balance(id(&p.recipient)),
    ];
    let msg = Message::Claim {
        schedule: core_id(schedule),
        to: cid(&p.recipient),
    };
    let r = chain.submit(Actor::new(ben, COPY), &declared, &[&p.beneficiary], &msg);
    assert_eq!(code(&r), 8);
    assert_eq!(chain.native(schedule), TOTAL);
}

#[test]
fn attack_as_of_in_the_future_refused() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    let r = chain.claim_as_of(&p.beneficiary, schedule, id(&p.recipient), END, T0 + DAY);
    assert!(matches!(r, Err(LeeError::OutOfValidityWindow)), "{r:?}");
    assert_eq!(chain.native(schedule), TOTAL);
    assert_eq!(chain.schedule(schedule).unwrap().claimed, 0);
}

#[test]
fn attack_payout_to_the_escrow_refused() {
    let mut chain = chain!();
    let p = parties();
    let schedule = native_schedule(&mut chain, &p, 1);
    chain.tick(END);
    assert_eq!(code(&chain.claim(&p.beneficiary, schedule, schedule)), 26);
    assert_eq!(
        code(&chain.claim_as_of(&p.beneficiary, schedule, schedule, END, END)),
        26
    );
    let mut terms = linear(&p.beneficiary, Asset::Native, TOTAL, START, CLIFF, END);
    terms.refund_to = Some(core_id(chain.schedule_of(&p.creator, 2)));
    assert_eq!(code(&chain.create(&p.creator, 2, terms, 0, treasury(&p))), 26);
    assert_eq!(chain.native(schedule), TOTAL);
}

#[test]
fn attack_unknown_schedule_and_garbage() {
    let mut chain = chain!();
    let p = parties();
    let _ = native_schedule(&mut chain, &p, 1);
    let nowhere = chain.schedule_of(&p.creator, 42);
    chain.evidence.push(lee::PublicAccountEvidence::Pda {
        program: chain.vesting,
        seed: vesting_program::schedule_seed(&cid(&p.creator), &sid(42)),
    });
    assert_eq!(code(&chain.make_non_cancelable(&p.creator, nowhere)), 8);
    // Bytes that decode as no message.
    let seat = chain.seat(id(&p.creator));
    let r = chain.submit(seat, &[seat], &[&p.creator], &[0xffu8; 3]);
    assert_eq!(code(&r), 2);
}
