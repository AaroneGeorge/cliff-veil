//! End-to-end tests of the vesting program on a standalone LEZ sequencer, one or more per requirement of RFP-017.
//! Test names start with the requirement id (docs/requirements.toml); `cargo run -p reqmap` maps them.
//!
//! Each test opens its own [`Ctx`] (fresh funded accounts, fresh schedule ids), so they run concurrently against
//! one sequencer. Times are clock milliseconds; blocks are 15 s apart on the local network, so schedules here span
//! a few blocks. Tests named `u1_`, `pr1_`, `pr3_` and `f2_claim_private_` go through the SDK (`Ctx::sdk`); a private
//! claim is proved in dev mode (`RISC0_DEV_MODE=1`).

use anyhow::{Context as _, Result, ensure};
use cliff_veil_sdk::{ClaimTarget, Destination, Error as SdkError, Outcome, Standing, linear_terms, milestone_terms};
use vesting_core::{Asset, BatchItem, BatchShape, BatchTerms};
use vesting_e2e::token;
use vesting_e2e::vesting::{core_id, linear, milestones};
use vesting_e2e::{Ctx, SendError};

/// One block on the local network, in clock milliseconds.
const BLOCK: u64 = 15_000;
const TOTAL: u128 = 1_000_000;
/// Total of a schedule claimed privately: a fresh private destination pays the network fee out of the claim (SPEC 9),
/// so the claim must exceed that fee (about 5.5 million units on the local network).
const PRIVATE_TOTAL: u128 = 100_000_000;

/// Error codes of SPEC section 5 used below.
mod code {
    pub const NOT_BENEFICIARY: u32 = 3;
    pub const NOT_CANCEL_AUTHORITY: u32 = 5;
    pub const NOT_MILESTONE_AUTHORITY: u32 = 6;
    pub const NOTHING_CLAIMABLE: u32 = 14;
    pub const NOT_CANCELABLE: u32 = 15;
    pub const ALREADY_CANCELLED: u32 = 16;
    pub const MILESTONE_ALREADY_SIGNALLED: u32 = 18;
    pub const NOT_TRANSFERABLE: u32 = 19;
    pub const BATCH_TOO_LARGE: u32 = 25;
    pub const FEE_MISMATCH: u32 = 27;
    pub const FEE_ABOVE_CAP: u32 = 28;
}

/// The `vesting_fees` config actor, not signed (it is a PDA of `vesting_fees`).
fn wallet_config_actor(
    config: lee_core::account::AccountId,
    fees: lee_core::account::AccountId,
) -> wallet::AccountMention {
    wallet::AccountIdentity::PublicNoSign(config).select_program_actor_state(fees)
}

fn refused_with(r: Result<vesting_e2e::Included, SendError>, expected: u32) -> Result<()> {
    match r {
        Err(e) => {
            ensure!(e.code() == Some(expected), "expected code {expected}, got {e}");
            Ok(())
        }
        Ok(i) => anyhow::bail!(
            "expected code {expected}, but the transaction was included in block {}",
            i.block
        ),
    }
}

// ---------------------------------------------------------------- F1: escrow and the three schedule kinds

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f1_escrow_native_holds_exactly_the_total() -> Result<()> {
    let mut ctx = Ctx::new("f1-escrow", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    let creator_before = ctx.native(ctx.id("creator")).await?;
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(
            &p,
            "creator",
            linear(ctx.id("beneficiary"), TOTAL, now, now, now + 100 * BLOCK),
        )
        .await?;
    assert_eq!(ctx.native(schedule).await?, TOTAL, "escrow holds the total");
    let state = ctx.schedule(&p, schedule).await?.expect("schedule written");
    assert_eq!((state.total, state.claimed), (TOTAL, 0));
    assert!(
        ctx.native(ctx.id("creator")).await? <= creator_before - TOTAL,
        "creator paid the total (and the fee)"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f1_escrow_token_holds_exactly_the_total() -> Result<()> {
    let mut ctx = Ctx::new("f1-token", &["creator", "beneficiary", "definition"]).await?;
    let p = ctx.programs()?;
    let token_program = p.token.context("token program not deployed on this sequencer")?;
    let definition = token::new_definition(&ctx, token_program, "definition", "creator", 10 * TOTAL).await?;
    let now = ctx.clock().await?;
    let mut terms = linear(ctx.id("beneficiary"), TOTAL, now, now, now + 100 * BLOCK);
    terms.asset = Asset::Token {
        definition: core_id(definition),
    };
    let schedule = ctx.create(&p, "creator", terms).await?;
    assert_eq!(
        token::balance(&ctx, token_program, schedule).await?,
        Some((definition, TOTAL)),
        "the (schedule, token) holding holds exactly the total"
    );
    assert_eq!(
        token::balance(&ctx, token_program, ctx.id("creator")).await?,
        Some((definition, 9 * TOTAL))
    );
    let state = ctx.schedule(&p, schedule).await?.expect("schedule written");
    assert_eq!((state.total, state.claimed), (TOTAL, 0));
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f1a_cliff_then_linear() -> Result<()> {
    let mut ctx = Ctx::new("f1a", &["creator", "beneficiary", "to"]).await?;
    let p = ctx.programs()?;
    let (ben, to) = (ctx.id("beneficiary"), ctx.id("to"));
    let now = ctx.clock().await?;
    let (start, cliff, end) = (now, now + 3 * BLOCK, now + 8 * BLOCK);
    let schedule = ctx.create(&p, "creator", linear(ben, TOTAL, start, cliff, end)).await?;

    // Before the cliff: nothing.
    refused_with(
        p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await,
        code::NOTHING_CLAIMABLE,
    )?;

    // At or after the cliff: the lump accrued since start, then linear.
    let t = ctx.wait_clock(cliff).await?;
    let to_before = ctx.native(to).await?;
    p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    let state = ctx.schedule(&p, schedule).await?.expect("schedule");
    let expected = state.vested(state.last_time)?;
    assert!(state.last_time >= t);
    assert_eq!(state.claimed, expected, "paid exactly vested(t)");
    assert!(
        state.claimed >= vesting_core::mul_div_floor(TOTAL, cliff - start, end - start).expect("fits"),
        "cliff lump"
    );
    assert_eq!(ctx.native(to).await? - to_before, state.claimed);

    // After the end: the rest, to the unit.
    ctx.wait_clock(end).await?;
    p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    assert_eq!(ctx.schedule(&p, schedule).await?.expect("schedule").claimed, TOTAL);
    assert_eq!(ctx.native(schedule).await?, 0, "escrow empty");
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f1b_fully_linear_accrues_from_start() -> Result<()> {
    let mut ctx = Ctx::new("f1b", &["creator", "beneficiary", "to"]).await?;
    let p = ctx.programs()?;
    let (ben, to) = (ctx.id("beneficiary"), ctx.id("to"));
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, now, now + 10 * BLOCK))
        .await?;
    ctx.wait_clock(now + 2 * BLOCK).await?;
    p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    let s = ctx.schedule(&p, schedule).await?.expect("schedule");
    assert!(s.claimed > 0 && s.claimed < TOTAL, "partial accrual {}", s.claimed);
    assert_eq!(s.claimed, s.vested(s.last_time)?);
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f1c_each_signal_unlocks_its_amount() -> Result<()> {
    let mut ctx = Ctx::new("f1c", &["creator", "beneficiary", "to"]).await?;
    let p = ctx.programs()?;
    let (creator, ben, to) = (ctx.id("creator"), ctx.id("beneficiary"), ctx.id("to"));
    let schedule = ctx
        .create(&p, "creator", milestones(ben, vec![100_000, 300_000, 600_000]))
        .await?;
    refused_with(
        p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await,
        code::NOTHING_CLAIMABLE,
    )?;
    p.signal(creator, schedule, 1).send(&ctx).await?;
    let before = ctx.native(to).await?;
    p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    assert_eq!(ctx.native(to).await? - before, 300_000, "exactly milestone 1");
    p.signal(creator, schedule, 0).send(&ctx).await?;
    p.signal(creator, schedule, 2).send(&ctx).await?;
    p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    assert_eq!(ctx.native(to).await? - before, TOTAL);
    Ok(())
}

// ---------------------------------------------------------------- F2: beneficiary, public and private claims

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f2_claim_public_by_the_beneficiary_only() -> Result<()> {
    let mut ctx = Ctx::new("f2-pub", &["creator", "beneficiary", "stranger", "to"]).await?;
    let p = ctx.programs()?;
    let (ben, stranger, to) = (ctx.id("beneficiary"), ctx.id("stranger"), ctx.id("to"));
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, now, now + BLOCK))
        .await?;
    ctx.wait_clock(now + BLOCK).await?;
    refused_with(
        p.claim(stranger, schedule, to, &Asset::Native)?.send(&ctx).await,
        code::NOT_BENEFICIARY,
    )?;
    let before = ctx.native(to).await?;
    p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    assert_eq!(ctx.native(to).await? - before, TOTAL);
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f2_claim_private_to_a_blinded_destination() -> Result<()> {
    let mut ctx = Ctx::new("f2-priv", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    let ben = ctx.id("beneficiary");
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, PRIVATE_TOTAL, now, now, now + BLOCK))
        .await?;
    ctx.wait_clock(now + BLOCK).await?;
    let mut sdk = ctx.sdk()?;
    let prepared = sdk
        .prepare_claim(
            ben,
            schedule,
            ClaimTarget::Private {
                account: None,
                fee_payer: None,
            },
        )
        .await?;
    let Outcome::Claim(summary) = prepared.outcome.clone() else {
        anyhow::bail!("not a claim")
    };
    let Destination::Private { account, blinded, .. } = summary.destination else {
        anyhow::bail!("not a private destination")
    };
    let receipt = sdk.send(&prepared).await?;
    let state = ctx.schedule(&p, schedule).await?.expect("schedule");
    assert_eq!(state.claimed, summary.amount, "claimed by the private claim");
    assert_eq!(
        ctx.native(schedule).await?,
        PRIVATE_TOTAL - summary.amount,
        "escrow debited by the claim"
    );
    // The transaction as anyone reads it from the chain: the blinded address is named, the canonical id is not.
    let bytes = ctx.transaction_bytes(&receipt.hash).await?;
    let has = |id: lee_core::account::AccountId| bytes.windows(32).any(|w| w == id.value());
    assert!(has(blinded), "the blinded destination is in the transaction");
    assert!(
        !has(account),
        "the canonical private id {account} occurs in the transaction"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f2_destination_is_not_in_schedule_state() -> Result<()> {
    let mut ctx = Ctx::new("f2-state", &["creator", "beneficiary", "to"]).await?;
    let p = ctx.programs()?;
    let (ben, to) = (ctx.id("beneficiary"), ctx.id("to"));
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, now, now + BLOCK))
        .await?;
    ctx.wait_clock(now + BLOCK).await?;
    p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    let bytes = ctx.actor_state(schedule, p.vesting).await?;
    assert!(
        !bytes.windows(32).any(|w| w == to.value()),
        "destination stored in the schedule"
    );
    Ok(())
}

// ---------------------------------------------------------------- F3: cancellation

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f3_cancel_refunds_exactly_the_unvested_part() -> Result<()> {
    let mut ctx = Ctx::new("f3-refund", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    let (creator, ben) = (ctx.id("creator"), ctx.id("beneficiary"));
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, now, now + 10 * BLOCK))
        .await?;
    ctx.wait_clock(now + 2 * BLOCK).await?;
    let state = ctx.schedule(&p, schedule).await?.expect("schedule");
    p.cancel(creator, schedule, &state)?.send(&ctx).await?;
    let s = ctx.schedule(&p, schedule).await?.expect("schedule");
    let c = s.cancelled.expect("cancelled");
    assert_eq!(c.refunded, TOTAL - c.vested_at_cancel);
    assert!(c.vested_at_cancel > 0 && c.refunded > 0);
    assert_eq!(
        ctx.native(schedule).await?,
        c.vested_at_cancel - s.claimed,
        "vested stays in escrow"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f3_vested_stays_claimable_after_cancel() -> Result<()> {
    let mut ctx = Ctx::new("f3-claim", &["creator", "beneficiary", "to"]).await?;
    let p = ctx.programs()?;
    let (creator, ben, to) = (ctx.id("creator"), ctx.id("beneficiary"), ctx.id("to"));
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, now, now + 10 * BLOCK))
        .await?;
    ctx.wait_clock(now + 2 * BLOCK).await?;
    let state = ctx.schedule(&p, schedule).await?.expect("schedule");
    p.cancel(creator, schedule, &state)?.send(&ctx).await?;
    let vested = ctx
        .schedule(&p, schedule)
        .await?
        .and_then(|s| s.cancelled)
        .expect("cancelled")
        .vested_at_cancel;
    let before = ctx.native(to).await?;
    p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    assert_eq!(ctx.native(to).await? - before, vested);
    assert_eq!(ctx.native(schedule).await?, 0);
    refused_with(
        p.cancel(creator, schedule, &state)?.send(&ctx).await,
        code::ALREADY_CANCELLED,
    )?;
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f3_non_cancelable_is_one_way_and_defaults_to_cancelable() -> Result<()> {
    let mut ctx = Ctx::new("f3-nc", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    let (creator, ben) = (ctx.id("creator"), ctx.id("beneficiary"));
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, now, now + 10 * BLOCK))
        .await?;
    let state = ctx.schedule(&p, schedule).await?.expect("schedule");
    assert!(state.cancelable, "default is cancelable");
    p.make_non_cancelable(creator, schedule).send(&ctx).await?;
    assert!(!ctx.schedule(&p, schedule).await?.expect("schedule").cancelable);
    refused_with(
        p.make_non_cancelable(creator, schedule).send(&ctx).await,
        code::NOT_CANCELABLE,
    )?;
    refused_with(
        p.cancel(creator, schedule, &state)?.send(&ctx).await,
        code::NOT_CANCELABLE,
    )?;
    assert_eq!(ctx.native(schedule).await?, TOTAL, "nothing moved");
    Ok(())
}

// ---------------------------------------------------------------- F4: transferable positions

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f4_transfer_by_the_holder_only_and_the_new_holder_claims() -> Result<()> {
    let mut ctx = Ctx::new("f4-transfer", &["creator", "beneficiary", "heir", "to"]).await?;
    let p = ctx.programs()?;
    let (creator, ben, heir, to) = (ctx.id("creator"), ctx.id("beneficiary"), ctx.id("heir"), ctx.id("to"));
    let now = ctx.clock().await?;
    let mut terms = linear(ben, TOTAL, now, now, now + BLOCK);
    terms.transferable = true;
    let schedule = ctx.create(&p, "creator", terms).await?;
    refused_with(
        p.transfer(creator, schedule, heir).send(&ctx).await,
        code::NOT_BENEFICIARY,
    )?;
    p.transfer(ben, schedule, heir).send(&ctx).await?;
    assert_eq!(
        ctx.schedule(&p, schedule).await?.expect("schedule").beneficiary,
        core_id(heir)
    );
    ctx.wait_clock(now + BLOCK).await?;
    refused_with(
        p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await,
        code::NOT_BENEFICIARY,
    )?;
    p.claim(heir, schedule, to, &Asset::Native)?.send(&ctx).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f4_non_transferable_is_refused_and_the_flag_never_changes() -> Result<()> {
    let mut ctx = Ctx::new("f4-flag", &["creator", "beneficiary", "heir"]).await?;
    let p = ctx.programs()?;
    let (ben, heir) = (ctx.id("beneficiary"), ctx.id("heir"));
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, now, now + BLOCK))
        .await?;
    refused_with(p.transfer(ben, schedule, heir).send(&ctx).await, code::NOT_TRANSFERABLE)?;
    assert!(!ctx.schedule(&p, schedule).await?.expect("schedule").transferable);
    Ok(())
}

// ---------------------------------------------------------------- F5: batch creation

fn batch_items(ctx: &mut Ctx, n: usize, beneficiary: lee_core::account::AccountId) -> Vec<BatchItem> {
    (0..n)
        .map(|i| BatchItem {
            schedule_id: ctx.fresh_id(),
            beneficiary: core_id(beneficiary),
            total: 1_000 + i as u128,
        })
        .collect()
}

fn batch_common(now: u64) -> BatchTerms {
    BatchTerms {
        asset: Asset::Native,
        shape: BatchShape::Linear {
            start: now,
            cliff: now,
            end: now + 10 * BLOCK,
        },
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    }
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f5_batch_of_8_opens_8_funded_schedules_in_one_transaction() -> Result<()> {
    let mut ctx = Ctx::new("f5-8", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    let now = ctx.clock().await?;
    let ben = ctx.id("beneficiary");
    let items = batch_items(&mut ctx, 8, ben);
    let totals: Vec<u128> = items.iter().map(|i| i.total).collect();
    let tx = p.create_batch(ctx.id("creator"), batch_common(now), items)?;
    let schedules = tx.schedules.clone();
    tx.send(&ctx).await?;
    for (schedule, total) in schedules.iter().zip(totals) {
        assert_eq!(ctx.native(*schedule).await?, total);
        assert_eq!(ctx.schedule(&p, *schedule).await?.expect("schedule").total, total);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn f5_batch_at_the_cap_is_accepted_and_one_more_is_refused() -> Result<()> {
    let mut ctx = Ctx::new("f5-max", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    let now = ctx.clock().await?;
    let ben = ctx.id("beneficiary");
    let over = batch_items(&mut ctx, vesting_core::MAX_BATCH_NATIVE + 1, ben);
    refused_with(
        p.create_batch(ctx.id("creator"), batch_common(now), over)?
            .send(&ctx)
            .await,
        code::BATCH_TOO_LARGE,
    )?;
    let at_cap = batch_items(&mut ctx, vesting_core::MAX_BATCH_NATIVE, ben);
    p.create_batch(ctx.id("creator"), batch_common(now), at_cap)?
        .send(&ctx)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------- U7: actionable claim errors

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn u7_nothing_claimable_reports_the_next_unlock_time() -> Result<()> {
    let mut ctx = Ctx::new("u7", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    let ben = ctx.id("beneficiary");
    let now = ctx.clock().await?;
    let cliff = now + 20 * BLOCK;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, cliff, now + 40 * BLOCK))
        .await?;
    let err = p
        .claim(ben, schedule, ben, &Asset::Native)?
        .send(&ctx)
        .await
        .expect_err("before the cliff");
    assert_eq!(err.code(), Some(code::NOTHING_CLAIMABLE), "{err}");
    let SendError::Refused { reason, .. } = err else {
        unreachable!()
    };
    assert!(reason.contains(&cliff.to_string()), "next unlock {cliff} in: {reason}");
    Ok(())
}

// ---------------------------------------------------------------- R1-R4: reliability

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn r1_failed_claim_on_chain_changes_nothing_then_retry_pays() -> Result<()> {
    let mut ctx = Ctx::new("r1", &["creator", "beneficiary", "to"]).await?;
    let p = ctx.programs()?;
    let (ben, to) = (ctx.id("beneficiary"), ctx.id("to"));
    let now = ctx.clock().await?;
    // The cliff leaves room for the creation and the failing claim to be included before it.
    let cliff = now + 8 * BLOCK;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, cliff, now + 10 * BLOCK))
        .await?;
    // Sent anyway and included as a reverted transaction: nothing but its fee is consumed.
    p.claim(ben, schedule, to, &Asset::Native)?
        .send_even_if_reverting(&mut ctx)
        .await?;
    let s = ctx.schedule(&p, schedule).await?.expect("schedule");
    assert_eq!(
        (s.claimed, ctx.native(schedule).await?),
        (0, TOTAL),
        "a failed claim consumes nothing"
    );
    ctx.wait_clock(cliff).await?;
    p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    assert!(
        ctx.schedule(&p, schedule).await?.expect("schedule").claimed > 0,
        "retry pays"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn r2_failed_cancel_on_chain_keeps_the_prior_state() -> Result<()> {
    let mut ctx = Ctx::new("r2", &["creator", "beneficiary", "stranger"]).await?;
    let p = ctx.programs()?;
    let (ben, stranger) = (ctx.id("beneficiary"), ctx.id("stranger"));
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, now, now + 10 * BLOCK))
        .await?;
    let before = ctx.actor_state(schedule, p.vesting).await?;
    let state = ctx.schedule(&p, schedule).await?.expect("schedule");
    refused_with(
        p.cancel(stranger, schedule, &state)?.send(&ctx).await,
        code::NOT_CANCEL_AUTHORITY,
    )?;
    p.cancel(stranger, schedule, &state)?
        .send_even_if_reverting(&mut ctx)
        .await?;
    assert_eq!(
        ctx.actor_state(schedule, p.vesting).await?,
        before,
        "state byte-identical"
    );
    assert_eq!(ctx.native(schedule).await?, TOTAL);
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn r3_concurrent_claims_on_independent_schedules() -> Result<()> {
    const N: usize = 6;
    let names: Vec<String> = (0..N).flat_map(|i| [format!("ben{i}"), format!("to{i}")]).collect();
    let mut parties: Vec<&str> = names.iter().map(String::as_str).collect();
    parties.push("creator");
    let mut ctx = Ctx::new("r3", &parties).await?;
    let p = ctx.programs()?;
    let now = ctx.clock().await?;
    let mut schedules = Vec::new();
    for i in 0..N {
        let terms = linear(ctx.id(&format!("ben{i}")), TOTAL + i as u128, now, now, now + BLOCK);
        schedules.push(ctx.create(&p, "creator", terms).await?);
    }
    ctx.wait_clock(now + BLOCK).await?;
    // All claims are sent at once and land in the same block(s).
    let claims = (0..N).map(|i| {
        let tx = p.claim(
            ctx.id(&format!("ben{i}")),
            schedules[i],
            ctx.id(&format!("to{i}")),
            &Asset::Native,
        );
        let ctx = &ctx;
        async move { tx?.send(ctx).await.map_err(anyhow::Error::from) }
    });
    let included = futures_join_all(claims).await;
    for r in included {
        r?;
    }
    for (i, schedule) in schedules.iter().enumerate() {
        let s = ctx.schedule(&p, *schedule).await?.expect("schedule");
        assert_eq!(s.claimed, TOTAL + i as u128, "schedule {i} paid in full, once");
        assert_eq!(ctx.native(*schedule).await?, 0);
    }
    Ok(())
}

/// Polls every future to completion concurrently (no extra dependency).
async fn futures_join_all<F: std::future::Future>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    let mut pending: Vec<_> = futures.into_iter().map(|f| Some(Box::pin(f))).collect();
    let mut results: Vec<Option<F::Output>> = pending.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut done = true;
        for (slot, result) in pending.iter_mut().zip(results.iter_mut()) {
            if let Some(f) = slot {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => {
                        *result = Some(v);
                        *slot = None;
                    }
                    std::task::Poll::Pending => done = false,
                }
            }
        }
        if done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    results.into_iter().map(|r| r.expect("completed")).collect()
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn r4_second_signal_of_a_milestone_is_refused_and_unlocks_nothing() -> Result<()> {
    let mut ctx = Ctx::new("r4", &["creator", "beneficiary", "to"]).await?;
    let p = ctx.programs()?;
    let (creator, ben, to) = (ctx.id("creator"), ctx.id("beneficiary"), ctx.id("to"));
    let schedule = ctx
        .create(&p, "creator", milestones(ben, vec![400_000, 600_000]))
        .await?;
    p.signal(creator, schedule, 0).send(&ctx).await?;
    refused_with(
        p.signal(creator, schedule, 0).send(&ctx).await,
        code::MILESTONE_ALREADY_SIGNALLED,
    )?;
    let before = ctx.native(to).await?;
    p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    assert_eq!(ctx.native(to).await? - before, 400_000, "no double unlock");
    Ok(())
}

// ---------------------------------------------------------------- P1: a claim is one transaction

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn p1_a_claim_completes_in_one_transaction() -> Result<()> {
    let mut ctx = Ctx::new("p1", &["creator", "beneficiary", "to"]).await?;
    let p = ctx.programs()?;
    let (ben, to) = (ctx.id("beneficiary"), ctx.id("to"));
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, now, now + BLOCK))
        .await?;
    ctx.wait_clock(now + BLOCK).await?;
    let before = ctx.native(to).await?;
    let included = p.claim(ben, schedule, to, &Asset::Native)?.send(&ctx).await?;
    // State, escrow and destination are all settled once that one transaction is in its block.
    assert_eq!(
        ctx.native(to).await? - before,
        TOTAL,
        "credited by tx {} in block {}",
        included.hash,
        included.block
    );
    assert_eq!(ctx.schedule(&p, schedule).await?.expect("schedule").claimed, TOTAL);
    assert_eq!(ctx.native(schedule).await?, 0);
    Ok(())
}

// ---------------------------------------------------------------- U1, U3, Pr1: SDK and CLI on the sequencer

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn u1_sdk_full_lifecycle_on_the_sequencer() -> Result<()> {
    let mut ctx = Ctx::new("u1", &["creator", "beneficiary", "heir"]).await?;
    let (creator, ben, heir) = (ctx.id("creator"), ctx.id("beneficiary"), ctx.id("heir"));
    let mut sdk = ctx.sdk()?;
    let now = ctx.clock().await?;

    // Create a transferable linear schedule, read it back.
    let id = sdk.fresh_schedule_id();
    let mut terms = linear_terms(ben, Asset::Native, TOTAL, now, now, now + 6 * BLOCK);
    terms.transferable = true;
    let create = sdk.prepare_create(creator, id, terms).await?;
    sdk.send(&create).await?;
    let schedule = sdk.programs().schedule_account(creator, &id);
    let view = sdk.status(schedule).await?;
    assert_eq!((view.escrow, view.state.claimed), (Some(TOTAL), 0));

    // Claimable, then claim to the beneficiary's own public account.
    ctx.wait_clock(now + BLOCK).await?;
    assert!(sdk.claimable(schedule).await? > 0, "something vested");
    let claim = sdk.prepare_claim(ben, schedule, ClaimTarget::Public(ben)).await?;
    sdk.send(&claim).await?;
    let claimed = sdk.status(schedule).await?.state.claimed;
    assert!(claimed > 0 && claimed < TOTAL, "partial claim {claimed}");

    // Transfer the position, then cancel: the vested part stays claimable by the new holder.
    let transfer = sdk.prepare_transfer(ben, schedule, heir).await?;
    sdk.send(&transfer).await?;
    assert_eq!(sdk.status(schedule).await?.state.beneficiary, core_id(heir));
    let cancel = sdk.prepare_cancel(creator, schedule).await?;
    sdk.send(&cancel).await?;
    let view = sdk.status(schedule).await?;
    assert_eq!(view.standing, Standing::Cancelled);
    let c = view.state.cancelled.expect("cancelled");
    assert_eq!(c.refunded, TOTAL - c.vested_at_cancel);
    if view.claimable > 0 {
        let rest = sdk.prepare_claim(heir, schedule, ClaimTarget::Public(heir)).await?;
        sdk.send(&rest).await?;
    }
    assert_eq!(sdk.status(schedule).await?.standing, Standing::Settled);

    // A milestone schedule: signal, then claim exactly that milestone.
    let id = sdk.fresh_schedule_id();
    let create = sdk
        .prepare_create(creator, id, milestone_terms(ben, Asset::Native, vec![400_000, 600_000]))
        .await?;
    sdk.send(&create).await?;
    let schedule = sdk.programs().schedule_account(creator, &id);
    let signal = sdk.prepare_signal(creator, schedule, 1).await?;
    sdk.send(&signal).await?;
    let claim = sdk.prepare_claim(ben, schedule, ClaimTarget::Public(ben)).await?;
    sdk.send(&claim).await?;
    assert_eq!(sdk.status(schedule).await?.state.claimed, 600_000);
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn u3_cli_create_claim_cancel_signal_and_query_on_the_sequencer() -> Result<()> {
    let mut ctx = Ctx::new("u3", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    let creator = ctx.id("creator");
    let home = ctx.wallet_home()?;
    let tag = hex::encode(&ctx.fresh_id()[..6]);
    let cli = |args: &[&str]| -> Result<String> {
        let out = std::process::Command::new(vesting_e2e::cli_bin())
            .env("LEE_WALLET_HOME_DIR", &home)
            .env("LEE_WALLET_PASSWORD", "")
            .arg("--ids")
            .arg(vesting_e2e::Ids::path())
            .arg("--yes")
            .args(args)
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        ensure!(out.status.success(), "cliff-veil {args:?} failed:\n{text}");
        Ok(text)
    };

    // A linear schedule created by the CLI, claimed in part, then cancelled.
    let linear_label = format!("u3-linear-{tag}");
    cli(&[
        "create",
        "--from",
        "creator",
        "--beneficiary",
        "beneficiary",
        "--total",
        "1000000",
        "--start",
        "now",
        "--end",
        "+90s",
        "--label",
        &linear_label,
    ])?;
    let linear = p.schedule_account(creator, &cliff_veil_sdk::schedule_id_from_label(&linear_label));
    assert_eq!(ctx.native(linear).await?, TOTAL, "escrow funded by the CLI create");
    ctx.wait_blocks(2).await?;
    let shown = cli(&["claimable", &linear.to_string()])?;
    assert!(!shown.trim().is_empty());
    cli(&["claim", &linear.to_string(), "--as", "beneficiary"])?;
    let claimed = ctx.schedule(&p, linear).await?.expect("schedule").claimed;
    assert!(claimed > 0, "the CLI claim paid");
    assert_eq!(
        ctx.native(linear).await?,
        TOTAL - claimed,
        "escrow debited by the CLI claim"
    );
    cli(&["cancel", &linear.to_string(), "--as", "creator"])?;
    assert!(ctx.schedule(&p, linear).await?.expect("schedule").cancelled.is_some());

    // A milestone schedule: the CLI signals milestone 0, then the beneficiary claims it.
    let ms_label = format!("u3-milestones-{tag}");
    cli(&[
        "create",
        "--from",
        "creator",
        "--beneficiary",
        "beneficiary",
        "--milestones",
        "250000,750000",
        "--label",
        &ms_label,
    ])?;
    let ms = p.schedule_account(creator, &cliff_veil_sdk::schedule_id_from_label(&ms_label));
    cli(&["signal", &ms.to_string(), "--index", "0", "--as", "creator"])?;
    cli(&["claim", &ms.to_string(), "--as", "beneficiary"])?;
    assert_eq!(ctx.schedule(&p, ms).await?.expect("schedule").claimed, 250_000);
    let json = cli(&["--json", "show", &ms.to_string()])?;
    assert!(json.contains("250000"), "show reports the claimed amount: {json}");
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn pr1_sdk_claims_to_a_public_and_to_a_private_account() -> Result<()> {
    let mut ctx = Ctx::new("pr1", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    let ben = ctx.id("beneficiary");
    let now = ctx.clock().await?;
    let end = now + 12 * BLOCK;
    let schedule = ctx
        .create(&p, "creator", linear(ben, PRIVATE_TOTAL, now, now, end))
        .await?;
    let mut sdk = ctx.sdk()?;

    let public = sdk.prepare_claim(ben, schedule, ClaimTarget::Public(ben)).await?;
    sdk.send(&public).await?;
    let first = ctx.schedule(&p, schedule).await?.expect("schedule").claimed;
    assert!(first > 0);
    assert_eq!(
        ctx.native(schedule).await?,
        PRIVATE_TOTAL - first,
        "public claim debited the escrow"
    );

    ctx.wait_clock(end).await?;
    let private = sdk
        .prepare_claim(
            ben,
            schedule,
            ClaimTarget::Private {
                account: None,
                fee_payer: None,
            },
        )
        .await?;
    sdk.send(&private).await?;
    assert_eq!(
        ctx.schedule(&p, schedule).await?.expect("schedule").claimed,
        PRIVATE_TOTAL
    );
    assert_eq!(ctx.native(schedule).await?, 0, "private claim debited the rest");
    Ok(())
}

// ---------------------------------------------------------------- Pr3: private target check

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn pr3_private_claim_to_a_public_account_is_refused_before_sending() -> Result<()> {
    let mut ctx = Ctx::new("pr3", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    let ben = ctx.id("beneficiary");
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(&p, "creator", linear(ben, TOTAL, now, now, now + BLOCK))
        .await?;
    ctx.wait_clock(now + BLOCK).await?;
    let before = (ctx.native(ben).await?, ctx.actor_state(schedule, p.vesting).await?);
    let mut sdk = ctx.sdk()?;
    let err = sdk
        .prepare_claim(
            ben,
            schedule,
            ClaimTarget::Private {
                account: Some(ben),
                fee_payer: None,
            },
        )
        .await
        .expect_err("a public account as private destination");
    assert!(matches!(err, SdkError::NotPrivateAccount { .. }), "{err}");
    assert_eq!(
        (ctx.native(ben).await?, ctx.actor_state(schedule, p.vesting).await?),
        before,
        "nothing was sent"
    );
    assert_eq!(ctx.native(schedule).await?, TOTAL);
    Ok(())
}

// ---------------------------------------------------------------- Soft requirements

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn soft_cancel_authority_cancels_and_the_creator_cannot() -> Result<()> {
    let mut ctx = Ctx::new("soft1", &["creator", "beneficiary", "dao"]).await?;
    let p = ctx.programs()?;
    let (creator, ben, dao) = (ctx.id("creator"), ctx.id("beneficiary"), ctx.id("dao"));
    let now = ctx.clock().await?;
    let mut terms = linear(ben, TOTAL, now, now, now + 10 * BLOCK);
    terms.cancel_authority = Some(core_id(dao));
    let schedule = ctx.create(&p, "creator", terms).await?;
    let state = ctx.schedule(&p, schedule).await?.expect("schedule");
    refused_with(
        p.cancel(creator, schedule, &state)?.send(&ctx).await,
        code::NOT_CANCEL_AUTHORITY,
    )?;
    p.cancel(dao, schedule, &state)?.send(&ctx).await?;
    assert!(ctx.schedule(&p, schedule).await?.expect("schedule").cancelled.is_some());
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn soft_milestone_authority_signals_and_the_creator_cannot() -> Result<()> {
    let mut ctx = Ctx::new("soft2", &["creator", "beneficiary", "council"]).await?;
    let p = ctx.programs()?;
    let (creator, ben, council) = (ctx.id("creator"), ctx.id("beneficiary"), ctx.id("council"));
    let mut terms = milestones(ben, vec![TOTAL]);
    terms.milestone_authority = Some(core_id(council));
    let schedule = ctx.create(&p, "creator", terms).await?;
    refused_with(
        p.signal(creator, schedule, 0).send(&ctx).await,
        code::NOT_MILESTONE_AUTHORITY,
    )?;
    p.signal(council, schedule, 0).send(&ctx).await?;
    Ok(())
}

// ---------------------------------------------------------------- Fees

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn fee1_fee2_zero_rate_charges_the_creator_nothing_at_creation() -> Result<()> {
    let mut ctx = Ctx::new("fee0", &["creator", "beneficiary"]).await?;
    let p = ctx.programs()?;
    ensure!(p.fees.is_some(), "vesting_fees not deployed");
    let now = ctx.clock().await?;
    let schedule = ctx
        .create(
            &p,
            "creator",
            linear(ctx.id("beneficiary"), TOTAL, now, now, now + BLOCK),
        )
        .await?;
    assert_eq!(
        ctx.native(schedule).await?,
        TOTAL,
        "the escrow gets the whole total; no protocol fee"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn fee3_fee4_switch_is_admin_only_capped_and_a_stale_quote_is_refused() -> Result<()> {
    let mut ctx = Ctx::new("fee-switch", &["creator", "beneficiary", "stranger", "treasury"]).await?;
    let p = ctx.programs()?;
    let (fees, config) = p.fees.context("vesting_fees not deployed")?;
    // Only the admin may set the fee: a stranger's SetFee is refused by the admin authority.
    let set_fee = vesting_fees::wire(vesting_fees::FeeMessage::SetFee {
        rate_bps: 50,
        treasury: core_id(ctx.id("stranger")),
    });
    let r = ctx
        .send(
            vec![ctx.signer("stranger", fees), wallet_config_actor(config, fees)],
            borsh::to_vec(&set_fee)?,
        )
        .await;
    ensure!(
        matches!(r, Err(SendError::Refused { .. })),
        "SetFee by a non-admin: {r:?}"
    );
    let ben = ctx.id("beneficiary");
    let treasury = ctx.id("treasury");
    let treasury_before = ctx.native(treasury).await?;
    let now = ctx.clock().await?;
    let terms = || linear(ben, TOTAL, now, now, now + BLOCK);
    // A quoted fee above MAX_FEE_BPS is refused by `vesting` itself (28), before it asks `vesting_fees`.
    let cap = vesting_core::fee_for(TOTAL, vesting_core::MAX_FEE_BPS).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let id = ctx.fresh_id();
    refused_with(
        p.create_with_fee(ctx.id("creator"), id, terms(), cap + 1, treasury)?
            .send(&ctx)
            .await,
        code::FEE_ABOVE_CAP,
    )?;
    // At the configured rate 0, a non-zero quote is stale: `vesting_fees` refuses it (27) and nothing moves.
    let id = ctx.fresh_id();
    refused_with(
        p.create_with_fee(ctx.id("creator"), id, terms(), 1, treasury)?
            .send(&ctx)
            .await,
        code::FEE_MISMATCH,
    )?;
    assert_eq!(ctx.native(treasury).await?, treasury_before, "treasury untouched");
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn fee4_token_fee_destination_is_the_treasury_ata_created_by_the_ata_program() -> Result<()> {
    // A non-zero token fee is a token Transfer from the creator's holding to the treasury's ATA (SPEC 6). This runs
    // that leg on the chain: the ATA program creates the holding at the address `vesting` derives, then the
    // creator's holding pays into it.
    let ctx = Ctx::new("fee4-ata", &["creator", "definition", "treasury", "anyone"]).await?;
    let p = ctx.programs()?;
    let token_program = p.token.context("token program not deployed")?;
    let ata = ctx
        .ids
        .get("ata")
        .context("ata program not deployed on this sequencer")?;
    let definition = token::new_definition(&ctx, token_program, "definition", "creator", TOTAL).await?;
    let treasury = ctx.id("treasury");
    let expected = token::ata_account(ata, token_program, treasury, definition);
    assert_eq!(token::balance(&ctx, token_program, expected).await?, None, "no ATA yet");
    let holding = token::create_ata(&ctx, ata, token_program, "anyone", treasury, definition).await?;
    assert_eq!(holding, expected);
    assert_eq!(
        token::balance(&ctx, token_program, holding).await?,
        Some((definition, 0))
    );
    token::transfer(&ctx, token_program, "creator", holding, 1_000).await?;
    // Created again by anyone: the balance stays.
    token::create_ata(&ctx, ata, token_program, "anyone", treasury, definition).await?;
    assert_eq!(
        token::balance(&ctx, token_program, holding).await?,
        Some((definition, 1_000))
    );
    Ok(())
}
