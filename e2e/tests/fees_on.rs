//! The fee switch turned on, on a standalone sequencer: a second `vesting_fees` + `vesting` pair deployed by
//! `e2e/deploy_fees.sh` (ids in `deploy/ids-local-dev-fees.toml`, or `CV_IDS_FEES`) with the rate at 50 basis points.
//!
//! - native creation pays the treasury exactly the quoted fee (Fee1, Fee2, Fee4);
//! - token creation pays the treasury's associated token account, made by the ATA program (Fee4);
//! - the admin role is handed to the test, a rate above the cap is refused (28), a stranger is refused by the admin
//!   library (E103), and after `Renounce` the rate is frozen (E102) while creation goes on (Fee3).
//!
//! The admin test renounces the pair's admin for good, so it runs once per pair: `e2e/deploy_fees.sh` makes a fresh
//! one. The other tests here only need the rate and treasury, which `Renounce` keeps.

use anyhow::{Context as _, Result, bail, ensure};
use lee_core::account::AccountId;
use lez_admin_authority::{AdminOp, AuthoritySlot, Holder, admin_op};
use vesting_core::Asset;
use vesting_e2e::vesting::{Programs, core_id, linear};
use vesting_e2e::{Ctx, Ids, SendError, record, token};
use vesting_fees::{FeeConfig, FeeMessage, wire};
use wallet::{AccountIdentity, AccountMention};

const BLOCK: u64 = 15_000;
const TOTAL: u128 = 1_000_000;
/// The rate `e2e/deploy_fees.sh` sets.
const RATE_BPS: u16 = 50;
const FEE_ABOVE_CAP: u32 = 28;
const FEE_MISMATCH: u32 = 27;

async fn ctx(name: &str, parties: &[&str]) -> Result<(Ctx, Programs, AccountId, AccountId)> {
    let path = Ids::fees_path();
    let ctx = Ctx::new_with_ids(&path, name, parties)
        .await
        .with_context(|| format!("the fee-enabled pair: run e2e/deploy_fees.sh ({})", path.display()))?;
    let p = ctx.programs()?;
    let (fees, config) = p.fees.context("no vesting_fees in the fee-enabled ids file")?;
    Ok((ctx, p, fees, config))
}

async fn config(ctx: &Ctx, fees: AccountId, config: AccountId) -> Result<FeeConfig> {
    Ok(vesting_fees::read_config(&ctx.actor_state(config, fees).await?))
}

fn config_actor(config: AccountId, fees: AccountId) -> AccountMention {
    AccountIdentity::PublicNoSign(config)
        .select_program_actor_state(fees)
        .without_authorization()
}

fn set_fee(rate_bps: u16, treasury: AccountId) -> Result<Vec<u8>> {
    Ok(borsh::to_vec(&wire(FeeMessage::SetFee {
        rate_bps,
        treasury: core_id(treasury),
    }))?)
}

fn admin(op: AdminOp) -> Result<Vec<u8>> {
    Ok(borsh::to_vec(&admin_op::<FeeMessage>(op))?)
}

fn refused_with_text(r: Result<vesting_e2e::Included, SendError>, text: &str) -> Result<()> {
    match r {
        Err(SendError::Refused { reason, .. }) => {
            ensure!(reason.contains(text), "expected `{text}`, refused with: {reason}");
            Ok(())
        }
        Err(e) => bail!("expected `{text}`, got {e}"),
        Ok(i) => bail!("expected `{text}`, but it was included in block {}", i.block),
    }
}

fn refused_with(r: Result<vesting_e2e::Included, SendError>, code: u32) -> Result<()> {
    match r {
        Err(e) if e.code() == Some(code) => Ok(()),
        Err(e) => bail!("expected code {code}, got {e}"),
        Ok(i) => bail!("expected code {code}, but it was included in block {}", i.block),
    }
}

#[tokio::test]
#[ignore = "needs a sequencer and the fee-enabled pair: e2e/deploy_fees.sh, e2e/run.sh"]
async fn fee1_fee4_native_fee_at_50_bps_is_paid_by_the_creator_to_the_treasury() -> Result<()> {
    let name = "fee1_fee4_native_fee_at_50_bps_is_paid_by_the_creator_to_the_treasury";
    let (mut ctx, p, fees, cfg) = ctx("feeon-native", &["creator", "beneficiary", "other"]).await?;
    let c = config(&ctx, fees, cfg).await?;
    assert_eq!(c.rate_bps, RATE_BPS, "rate set by e2e/deploy_fees.sh");
    let treasury = c.treasury;
    let fee = vesting_core::fee_for(TOTAL, RATE_BPS).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    assert_eq!(fee, 5_000, "50 bps of 1,000,000");

    // The SDK quotes the same fee and treasury.
    let sdk = ctx.sdk()?;
    assert_eq!(sdk.quote_fee(TOTAL, ctx.id("creator")).await?, (fee, treasury));

    let (creator, ben) = (ctx.id("creator"), ctx.id("beneficiary"));
    let now = ctx.clock().await?;
    let terms = || linear(ben, TOTAL, now, now, now + 10 * BLOCK);
    // A stale quote (the rate-0 fee) and a fee to another treasury are refused by `vesting_fees` (27).
    let id = ctx.fresh_id();
    refused_with(
        p.create_with_fee(creator, id, terms(), 0, treasury)?.send(&ctx).await,
        FEE_MISMATCH,
    )?;
    refused_with(
        p.create_with_fee(creator, id, terms(), fee, ctx.id("other"))?
            .send(&ctx)
            .await,
        FEE_MISMATCH,
    )?;

    let (creator_before, treasury_before, ben_before) = (
        ctx.native(creator).await?,
        ctx.native(treasury).await?,
        ctx.native(ben).await?,
    );
    let tx = p.create_with_fee(creator, id, terms(), fee, treasury)?;
    let schedule = tx.schedules[0];
    let included = tx.send(&ctx).await?;
    assert_eq!(ctx.native(schedule).await?, TOTAL, "the escrow holds the whole total");
    assert_eq!(
        ctx.native(treasury).await? - treasury_before,
        fee,
        "the treasury received exactly the fee"
    );
    let network_fee = creator_before - ctx.native(creator).await? - TOTAL - fee;
    let cap = included.max_fee.context("declared fee cap")?;
    assert!(
        network_fee > 0 && network_fee <= cap,
        "network fee {network_fee}, cap {cap}"
    );
    assert_eq!(ctx.native(ben).await?, ben_before, "the beneficiary pays nothing");
    record(name, "protocol_fee", fee);
    record(name, "tx", format!("{} block {}", included.hash, included.block));
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer and the fee-enabled pair: e2e/deploy_fees.sh, e2e/run.sh"]
async fn fee4_token_fee_at_50_bps_goes_to_the_treasury_ata_made_by_the_ata_program() -> Result<()> {
    let name = "fee4_token_fee_at_50_bps_goes_to_the_treasury_ata_made_by_the_ata_program";
    let (mut ctx, p, fees, cfg) = ctx("feeon-token", &["creator", "beneficiary", "definition", "anyone"]).await?;
    let treasury = config(&ctx, fees, cfg).await?.treasury;
    let tp = p.token.context("token program")?;
    let ata = p.ata.context("ata program")?;
    let supply = 10 * TOTAL;
    let definition = token::new_definition(&ctx, tp, "definition", "creator", supply).await?;
    let fee = vesting_core::fee_for(TOTAL, RATE_BPS).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let (creator, ben) = (ctx.id("creator"), ctx.id("beneficiary"));
    let now = ctx.clock().await?;
    let mut terms = linear(ben, TOTAL, now, now, now + 10 * BLOCK);
    terms.asset = Asset::Token {
        definition: core_id(definition),
    };

    // The treasury's ATA does not exist yet: anyone creates it through the ATA program, at the address `vesting`
    // derives.
    let holding = token::ata_account(ata, tp, treasury, definition);
    assert_eq!(token::balance(&ctx, tp, holding).await?, None, "no ATA yet");
    assert_eq!(
        token::create_ata(&ctx, ata, tp, "anyone", treasury, definition).await?,
        holding
    );

    let id = ctx.fresh_id();
    let tx = p.create_with_fee(creator, id, terms, fee, treasury)?;
    let schedule = tx.schedules[0];
    let included = tx.send(&ctx).await?;
    assert_eq!(
        token::balance(&ctx, tp, holding).await?,
        Some((definition, fee)),
        "the treasury's ATA received exactly the fee"
    );
    assert_eq!(token::balance(&ctx, tp, schedule).await?, Some((definition, TOTAL)));
    assert_eq!(
        token::balance(&ctx, tp, creator).await?,
        Some((definition, supply - TOTAL - fee)),
        "the creator paid the total and the fee"
    );
    record(name, "protocol_fee", fee);
    record(name, "tx", format!("{} block {}", included.hash, included.block));
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer and a fresh fee-enabled pair: e2e/deploy_fees.sh, e2e/run.sh"]
async fn fee3_admin_switch_refuses_above_the_cap_and_renounce_freezes_it_while_creation_goes_on() -> Result<()> {
    let (mut ctx, p, fees, cfg) = ctx(
        "feeon-admin",
        &["admin", "stranger", "creator", "beneficiary", "definition"],
    )
    .await?;
    let funder = vesting_e2e::funder_account()?;
    let before = config(&ctx, fees, cfg).await?;
    let treasury = before.treasury;
    match before.admin {
        AuthoritySlot::Renounced => bail!("this pair's admin is renounced: run e2e/deploy_fees.sh for a fresh pair"),
        AuthoritySlot::Held { nominee, .. } => ensure!(
            nominee == Some(funder),
            "the pair's admin has not nominated the test funder {funder} (e2e/deploy_fees.sh): {:?}",
            before.admin
        ),
        AuthoritySlot::Unset => bail!("vesting_fees is not initialised"),
    }

    // A stranger's SetFee is refused by the admin library.
    let me = |who: AccountId| AccountIdentity::Public(who).select_program_actor_state(fees);
    refused_with_text(
        ctx.send(
            vec![me(ctx.id("stranger")), config_actor(cfg, fees)],
            set_fee(RATE_BPS, ctx.id("stranger"))?,
        )
        .await,
        "lez-authority/admin E103",
    )?;

    // The nominee (the test funder) accepts, then hands the role to this test's own account in two steps.
    ctx.send_as_funder(
        |f| vec![me(f), config_actor(cfg, fees)],
        admin(AdminOp::Accept { pda_seed: None })?,
    )
    .await?;
    let admin_id = ctx.id("admin");
    ctx.send_as_funder(
        |f| vec![me(f), config_actor(cfg, fees)],
        admin(AdminOp::Nominate { next: admin_id })?,
    )
    .await?;
    ctx.send(
        vec![me(admin_id), config_actor(cfg, fees)],
        admin(AdminOp::Accept { pda_seed: None })?,
    )
    .await?;
    assert_eq!(
        config(&ctx, fees, cfg).await?.admin,
        AuthoritySlot::held(Holder {
            account: admin_id,
            via: None
        }),
        "the test's account holds the admin role"
    );

    // Above the 1% cap: refused (28), even for the admin. At the cap's edge of the current values: accepted.
    refused_with(
        ctx.send(
            vec![me(admin_id), config_actor(cfg, fees)],
            set_fee(vesting_core::MAX_FEE_BPS + 1, treasury)?,
        )
        .await,
        FEE_ABOVE_CAP,
    )?;
    ctx.send(
        vec![me(admin_id), config_actor(cfg, fees)],
        set_fee(RATE_BPS, treasury)?,
    )
    .await?;
    let after = config(&ctx, fees, cfg).await?;
    assert_eq!(
        (after.rate_bps, after.treasury),
        (RATE_BPS, treasury),
        "unchanged by the refusal"
    );

    // Renounce: the rate and treasury are frozen for good.
    ctx.send(vec![me(admin_id), config_actor(cfg, fees)], admin(AdminOp::Renounce)?)
        .await?;
    assert_eq!(config(&ctx, fees, cfg).await?.admin, AuthoritySlot::Renounced);
    refused_with_text(
        ctx.send(vec![me(admin_id), config_actor(cfg, fees)], set_fee(0, treasury)?)
            .await,
        "lez-authority/admin E102",
    )?;

    // Creation goes on at the frozen rate. A token schedule of its own definition, so its fee lands in an ATA no
    // other test pays into (the native test reads the treasury's native balance).
    let (creator, ben) = (ctx.id("creator"), ctx.id("beneficiary"));
    let tp = p.token.context("token program")?;
    let ata = p.ata.context("ata program")?;
    let definition = token::new_definition(&ctx, tp, "definition", "creator", 10 * TOTAL).await?;
    let holding = token::create_ata(&ctx, ata, tp, "creator", treasury, definition).await?;
    let fee = vesting_core::fee_for(TOTAL, RATE_BPS).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let now = ctx.clock().await?;
    let mut terms = linear(ben, TOTAL, now, now, now + BLOCK);
    terms.asset = Asset::Token {
        definition: core_id(definition),
    };
    let id = ctx.fresh_id();
    let tx = p.create_with_fee(creator, id, terms, fee, treasury)?;
    let schedule = tx.schedules[0];
    tx.send(&ctx).await?;
    assert_eq!(
        token::balance(&ctx, tp, schedule).await?,
        Some((definition, TOTAL)),
        "created after Renounce"
    );
    assert_eq!(
        token::balance(&ctx, tp, holding).await?,
        Some((definition, fee)),
        "fee at the frozen rate"
    );
    Ok(())
}
