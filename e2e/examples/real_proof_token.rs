//! A token claim to a private holding, with every privacy-preserving transaction proved for real when
//! `RISC0_DEV_MODE` is unset: the second part of `tools/real_proof/run.sh` (see tools/real_proof/README.md).
//!
//!   CV_SEQ_RPC=http://127.0.0.1:3051 CV_IDS=deploy/ids-local-pub.toml \
//!     cargo run --locked -p vesting_e2e --example real_proof_token
//!
//! Steps, as the end-to-end test `f2_token_claim_private_to_an_owned_private_holding` takes them (SPEC 9):
//!   1. a fixed-supply token whose whole supply sits in the creator's holding (public);
//!   2. a schedule of that token for the beneficiary, fully vested one block after its start (public);
//!   3. a private fee payer funded from the beneficiary's public balance (proved), since a token claim brings no
//!      native balance to pay its fee with;
//!   4. the destination private account initialises its own holding of the token, fee paid by the payer (proved);
//!   5. the SDK's private claim (`ClaimAsOf` to a one-time blinded address of the destination), fee paid by the
//!      payer (proved). The SDK names `vesting` and the token program to the prover (`Tx::prover_programs`).
//!
//! Output: tab-separated lines starting with `rp`, which run.sh turns into JSON:
//!   rp  step   <name>  <tx hash>  <block>  <seconds from send to inclusion, proof included>
//!   rp  value  <key>   <value>
//!   rp  error  <message>                      (and exit status 1)

use std::collections::BTreeSet;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow, ensure};
use cliff_veil_sdk::{ClaimTarget, Destination, Outcome};
use vesting_core::Asset;
use vesting_e2e::vesting::{core_id, linear};
use vesting_e2e::{Ctx, private};
use wallet::program_facades::CreditDelivery;
use wallet::program_facades::native_token_transfer::NativeTokenTransfer;
use wallet::{AccountIdentity, AccountMention, AsyncDelivery};

/// Block time of the standalone sequencer (ms).
const BLOCK: u64 = 15_000;
/// Tokens in the schedule; the token's supply is ten times that.
const TOTAL: u128 = 1_000_000;
/// Native units moved to the private fee payer: three private fees with room for base-fee rises.
const PAYER_FUNDS: u128 = 50_000_000;

fn step(name: &str, hash: &str, block: u64, started: Instant) {
    println!("rp\tstep\t{name}\t{hash}\t{block}\t{}", started.elapsed().as_secs());
}

fn value(key: &str, value: impl std::fmt::Display) {
    println!("rp\tvalue\t{key}\t{value}");
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            println!("rp\terror\t{}", format!("{e:#}").replace(['\t', '\n'], " "));
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    ensure!(
        !cliff_veil_sdk::dev_mode(),
        "RISC0_DEV_MODE is set: this run is meant to prove for real"
    );
    let mut ctx = Ctx::new("real-proof-token", &["creator", "beneficiary", "definition"]).await?;
    let p = ctx.programs()?;
    let tp = p.token.context("no token program in the ids file")?;
    let (creator, ben, definition) = (ctx.id("creator"), ctx.id("beneficiary"), ctx.id("definition"));
    value("token_program", tp);
    value("creator", creator);
    value("beneficiary", ben);
    value("definition", definition);

    // 1. The token (public).
    let started = Instant::now();
    let message = token_core::Message::NewDefinition {
        name: "VEST".to_owned(),
        total_supply: 10 * TOTAL,
        mint_authority: None,
        holding: creator,
    };
    let included = ctx
        .send(
            vec![ctx.signer("definition", tp), ctx.signer("creator", tp)],
            borsh::to_vec(&message)?,
        )
        .await
        .map_err(|e| anyhow!("token definition: {e}"))?;
    step("token-definition", &included.hash.to_string(), included.block, started);

    // 2. The schedule (public), fully vested one block after its start.
    let now = ctx.clock().await?;
    let mut terms = linear(ben, TOTAL, now, now, now + BLOCK);
    terms.asset = Asset::Token {
        definition: core_id(definition),
    };
    let started = Instant::now();
    let schedule_id = ctx.fresh_id();
    let tx = p.create(creator, schedule_id, terms)?;
    let schedule = tx.schedules[0];
    let included = tx.send(&ctx).await.map_err(|e| anyhow!("create: {e}"))?;
    step("create", &included.hash.to_string(), included.block, started);
    value("schedule", schedule);

    // 3. The private fee payer, funded from the beneficiary's public balance (proved).
    let mut sdk = ctx.sdk()?;
    let wallet = sdk.backend_mut().wallet_mut();
    let payer = private::new_private(wallet)?;
    let dest = private::new_private(wallet)?;
    let started = Instant::now();
    let (hash, _) = NativeTokenTransfer(wallet)
        .transfer(
            AccountIdentity::Public(ben),
            AccountIdentity::PrivateOwned(payer),
            PAYER_FUNDS,
            CreditDelivery::Automatic,
        )
        .await
        .map_err(|e| anyhow!("public to private transfer: {e:#}"))?;
    let block = private::settle(wallet, hash).await?;
    step("fund-private-fee-payer", &hash.to_string(), block, started);
    let funded = private::private_native_balance(wallet, payer).context("the payer is not synced")?;
    ensure!(funded > 0, "the private fee payer holds no native balance");
    value("payer_funded", funded);

    // 4. The destination initialises its own private holding of the token, the payer pays (proved).
    let catalog = private::catalog(wallet, &[tp]).await?;
    let accounts = vec![
        AccountMention {
            identity: AccountIdentity::PrivateOwned(dest),
            program_account_id: tp,
            authorizes: true,
            address_openings: BTreeSet::new(),
        },
        AccountIdentity::PublicNoSign(definition)
            .select_program_actor_state(tp)
            .without_authorization(),
    ];
    let message = borsh::to_vec(&token_core::Message::InitializeHolding { definition })?;
    let started = Instant::now();
    let (hash, _) = wallet
        .send_tx_with_pre_check(
            accounts,
            0,
            message,
            &catalog,
            None,
            AsyncDelivery::default(),
            Some(payer),
            |_| Ok(()),
        )
        .await
        .map_err(|e| anyhow!("private InitializeHolding: {e:#}"))?;
    let block = private::settle(wallet, hash).await?;
    step("init-private-holding", &hash.to_string(), block, started);
    ensure!(
        private::private_token_balance(wallet, tp, dest) == Some((definition, 0)),
        "the private holding is not initialised"
    );

    // 5. The private claim through the SDK (proved).
    ctx.wait_clock(now + BLOCK).await?;
    let prepared = sdk
        .prepare_claim(
            ben,
            schedule,
            ClaimTarget::Private {
                account: Some(dest),
                fee_payer: Some(payer),
            },
        )
        .await?;
    ensure!(
        prepared.tx.prover_programs().contains(&tp),
        "the token program is not declared to the prover"
    );
    let Outcome::Claim(summary) = &prepared.outcome else {
        anyhow::bail!("not a claim outcome")
    };
    let Destination::Private { blinded, .. } = summary.destination else {
        anyhow::bail!("not a private destination")
    };
    ensure!(summary.fee_payer == payer, "the named payer does not pay");
    let started = Instant::now();
    let receipt = sdk.send(&prepared).await?;
    step("private-token-claim", &receipt.hash, receipt.block, started);

    // Results.
    let state = ctx.schedule(&p, schedule).await?.context("schedule gone")?;
    let wallet = sdk.backend().wallet();
    let received = private::private_token_balance(wallet, tp, dest).map(|(_, b)| b);
    let payer_after = private::private_native_balance(wallet, payer).unwrap_or(0);
    value("claim_amount", summary.amount);
    value("schedule_claimed", state.claimed);
    value(
        "private_holding_balance",
        received.map_or_else(|| "none".to_owned(), |b| b.to_string()),
    );
    value("payer_balance_after", payer_after);
    value("fee_paid_by_private_payer", funded.saturating_sub(payer_after));
    value("destination_private_account", dest);
    value("private_fee_payer", payer);
    value("one_time_address", blinded);
    ensure!(state.claimed == TOTAL, "claimed {} of {TOTAL}", state.claimed);
    ensure!(received == Some(TOTAL), "the private holding holds {received:?}");
    Ok(())
}
