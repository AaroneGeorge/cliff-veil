//! Composition on a standalone sequencer: the `launchpad_escrow` example (examples/launchpad_escrow), deployed by
//! `e2e/deploy_compose.sh` against the `vesting` of deploy/ids-local-dev.toml (ids in
//! `deploy/ids-local-dev-compose.toml`, or `CV_IDS_COMPOSE`).
//!
//! The project's collateral schedule is created by the launch program's own account (a PDA), which is also its
//! milestone and cancel authority. The project's direct signal and cancel are refused by `vesting` with their codes;
//! a milestone is signalled only by the second of three approvers, and the schedule is cancelled only by the second
//! cancel vote. docs/COMPOSITION.md lists the transactions.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail, ensure};
use launchpad_escrow::{LaunchError, LaunchMessage, launch_account};
use lee_core::account::AccountId;
use lee_core::native_token::NATIVE_TOKEN_PROGRAM_ID;
use vesting_core::{Asset, Kind};
use vesting_e2e::vesting::{Programs, core_id};
use vesting_e2e::{Ctx, Included, SendError, record, repo_root};
use wallet::{AccountIdentity, AccountMention};

const AMOUNTS: [u128; 3] = [300_000, 300_000, 400_000];
const TOTAL: u128 = 1_000_000;
const THRESHOLD: u8 = 2;
/// vesting's codes (SPEC 5).
const NOT_CANCEL_AUTHORITY: u32 = 5;
const NOT_MILESTONE_AUTHORITY: u32 = 6;
const NOTHING_CLAIMABLE: u32 = 14;

fn ids_path() -> PathBuf {
    std::env::var("CV_IDS_COMPOSE")
        .map_or_else(|_| repo_root().join("deploy/ids-local-dev-compose.toml"), PathBuf::from)
}

/// A test's chain view, the programs, and the launchpad's id.
async fn ctx(name: &str) -> Result<(Ctx, Programs, AccountId)> {
    let path = ids_path();
    let ctx = Ctx::new_with_ids(
        &path,
        name,
        &["project", "approver-a", "approver-b", "approver-c", "refund", "dest"],
    )
    .await
    .with_context(|| format!("the launchpad example: run e2e/deploy_compose.sh ({})", path.display()))?;
    let p = ctx.programs()?;
    let launchpad = ctx.ids.require("launchpad_escrow")?;
    Ok((ctx, p, launchpad))
}

/// One launch on the sequencer and the transactions of docs/COMPOSITION.md.
struct Launch {
    launchpad: AccountId,
    p: Programs,
    launch: AccountId,
    schedule: AccountId,
    refund_to: AccountId,
}

impl Launch {
    fn new(ctx: &mut Ctx, p: Programs, launchpad: AccountId) -> (Self, [u8; 32]) {
        let launch_id = ctx.fresh_id();
        let launch = launch_account(&launchpad, &ctx.id("project"), &launch_id);
        let l = Self {
            launchpad,
            p,
            launch,
            // The schedule `vesting` derives for creator `launch` and schedule id `launch_id` (SPEC 1.2).
            schedule: p.schedule_account(launch, &launch_id),
            refund_to: ctx.id("refund"),
        };
        (l, launch_id)
    }

    fn not_signed(account: AccountId, program: AccountId) -> AccountMention {
        AccountIdentity::PublicNoSign(account).select_program_actor_state(program)
    }

    /// The caller's seat under the launchpad, then the launch, its seat under `vesting` and the schedule.
    fn actors(&self, caller: AccountId) -> Vec<AccountMention> {
        vec![
            AccountIdentity::Public(caller).select_program_actor_state(self.launchpad),
            Self::not_signed(self.launch, self.launchpad),
            Self::not_signed(self.launch, self.p.vesting),
            Self::not_signed(self.schedule, self.p.vesting),
        ]
    }

    async fn open(&self, ctx: &Ctx, launch_id: [u8; 32]) -> Result<Included, SendError> {
        let project = ctx.id("project");
        let mut accounts = self.actors(project);
        accounts.push(AccountIdentity::Public(project).balance());
        accounts.push(Self::not_signed(self.schedule, NATIVE_TOKEN_PROGRAM_ID));
        let (fees, config) = self
            .p
            .fees
            .ok_or_else(|| SendError::Other(anyhow::anyhow!("no vesting_fees")))?;
        accounts.push(Self::not_signed(config, fees));
        let message = LaunchMessage::Open {
            launch_id,
            amounts: AMOUNTS.to_vec(),
            approvers: ["approver-a", "approver-b", "approver-c"].map(|a| ctx.id(a)).to_vec(),
            threshold: THRESHOLD,
            refund_to: self.refund_to,
            fee: 0,
            treasury: project,
        };
        ctx.send(accounts, borsh::to_vec(&message).expect("encodes")).await
    }

    async fn approve(&self, ctx: &Ctx, who: &str, index: u8) -> Result<Included, SendError> {
        let message = LaunchMessage::Approve {
            launch: self.launch,
            index,
        };
        ctx.send(self.actors(ctx.id(who)), borsh::to_vec(&message).expect("encodes"))
            .await
    }

    async fn vote_cancel(&self, ctx: &Ctx, who: &str) -> Result<Included, SendError> {
        let mut accounts = self.actors(ctx.id(who));
        accounts.push(Self::not_signed(
            clock_core::CLOCK_PROGRAM_ACCOUNT_ID,
            clock_core::clock_account_id(),
        ));
        accounts.push(Self::not_signed(self.schedule, NATIVE_TOKEN_PROGRAM_ID));
        accounts.push(Self::not_signed(self.refund_to, NATIVE_TOKEN_PROGRAM_ID));
        let message = LaunchMessage::VoteCancel { launch: self.launch };
        ctx.send(accounts, borsh::to_vec(&message).expect("encodes")).await
    }
}

fn refused_by_vesting(r: Result<Included, SendError>, code: u32) -> Result<()> {
    match r {
        Err(e) if e.code() == Some(code) => Ok(()),
        Err(e) => bail!("expected vesting code {code}, got {e}"),
        Ok(i) => bail!("expected vesting code {code}, but it was included in block {}", i.block),
    }
}

fn refused_by_launchpad(r: Result<Included, SendError>, error: LaunchError) -> Result<()> {
    match r {
        Err(SendError::Refused { reason, .. }) if launchpad_escrow::parse_code(&reason) == Some(error.code()) => Ok(()),
        Err(e) => bail!("expected launchpad code {} ({error:?}), got {e}", error.code()),
        Ok(i) => bail!(
            "expected launchpad code {}, but it was included in block {}",
            error.code(),
            i.block
        ),
    }
}

fn tx(i: &Included) -> String {
    format!("{} block {}", i.hash, i.block)
}

#[tokio::test]
#[ignore = "needs a sequencer and the launchpad example: e2e/deploy_compose.sh, e2e/run.sh"]
async fn soft2_compose_launch_pda_holds_the_milestone_authority_and_m_of_n_approvals_pay_the_project() -> Result<()> {
    let name = "soft2_compose_launch_pda_holds_the_milestone_authority_and_m_of_n_approvals_pay_the_project";
    let (mut ctx, p, launchpad) = ctx("compose-signal").await?;
    let (l, launch_id) = Launch::new(&mut ctx, p, launchpad);
    let project = ctx.id("project");

    let project_before = ctx.native(project).await?;
    let opened = l.open(&ctx, launch_id).await?;
    record(name, "open_tx", tx(&opened));
    let s = ctx.schedule(&p, l.schedule).await?.context("the schedule exists")?;
    ensure!(
        s.creator == core_id(l.launch),
        "the schedule's creator is the launch program's account"
    );
    ensure!(s.milestone_authority == core_id(l.launch) && s.cancel_authority == core_id(l.launch));
    ensure!(s.beneficiary == core_id(project), "the project is the beneficiary");
    ensure!(
        ctx.native(l.schedule).await? == TOTAL,
        "the escrow holds the collateral"
    );
    ensure!(
        project_before - ctx.native(project).await? >= TOTAL,
        "funded by the project"
    );
    record(name, "launch", l.launch);
    record(name, "schedule", l.schedule);

    // The project cannot signal its own milestone, straight to vesting or through the launchpad.
    refused_by_vesting(
        p.signal(project, l.schedule, 0).send(&ctx).await,
        NOT_MILESTONE_AUTHORITY,
    )?;
    refused_by_launchpad(l.approve(&ctx, "project", 0).await, LaunchError::NotApprover)?;

    // One approval of two: nothing signalled, nothing claimable.
    let first = l.approve(&ctx, "approver-a", 0).await?;
    record(name, "approval_1_tx", tx(&first));
    let s = ctx.schedule(&p, l.schedule).await?.context("schedule")?;
    ensure!(
        s.kind
            == Kind::Milestones {
                amounts: AMOUNTS.to_vec(),
                signalled: 0
            },
        "{:?}",
        s.kind
    );
    let dest = ctx.id("dest");
    refused_by_vesting(
        p.claim(project, l.schedule, dest, &Asset::Native)?.send(&ctx).await,
        NOTHING_CLAIMABLE,
    )?;

    // The second approval signals milestone 0 in the same transaction; the project claims it.
    let second = l.approve(&ctx, "approver-c", 0).await?;
    record(name, "approval_2_signal_tx", tx(&second));
    let s = ctx.schedule(&p, l.schedule).await?.context("schedule")?;
    ensure!(
        s.kind
            == Kind::Milestones {
                amounts: AMOUNTS.to_vec(),
                signalled: 1
            },
        "{:?}",
        s.kind
    );
    let dest_before = ctx.native(dest).await?;
    let claim = p.claim(project, l.schedule, dest, &Asset::Native)?.send(&ctx).await?;
    record(name, "claim_tx", tx(&claim));
    ensure!(
        ctx.native(dest).await? - dest_before == AMOUNTS[0],
        "the project claimed milestone 0"
    );
    ensure!(ctx.native(l.schedule).await? == TOTAL - AMOUNTS[0]);
    Ok(())
}

#[tokio::test]
#[ignore = "needs a sequencer and the launchpad example: e2e/deploy_compose.sh, e2e/run.sh"]
async fn soft1_compose_only_the_launch_rule_cancels_and_refunds_the_unvested_collateral() -> Result<()> {
    let name = "soft1_compose_only_the_launch_rule_cancels_and_refunds_the_unvested_collateral";
    let (mut ctx, p, launchpad) = ctx("compose-cancel").await?;
    let (l, launch_id) = Launch::new(&mut ctx, p, launchpad);
    let project = ctx.id("project");
    l.open(&ctx, launch_id).await?;
    l.approve(&ctx, "approver-a", 0).await?;
    l.approve(&ctx, "approver-b", 0).await?;

    // The project cannot cancel; one vote of two cancels nothing.
    let s = ctx.schedule(&p, l.schedule).await?.context("schedule")?;
    refused_by_vesting(
        p.cancel(project, l.schedule, &s)?.send(&ctx).await,
        NOT_CANCEL_AUTHORITY,
    )?;
    refused_by_launchpad(l.vote_cancel(&ctx, "project").await, LaunchError::NotApprover)?;
    l.vote_cancel(&ctx, "approver-b").await?;
    ensure!(
        ctx.schedule(&p, l.schedule)
            .await?
            .context("schedule")?
            .cancelled
            .is_none()
    );

    // The second vote cancels: the unvested part goes to the refund account, milestone 0 stays the project's.
    let refund_before = ctx.native(l.refund_to).await?;
    let cancel = l.vote_cancel(&ctx, "approver-c").await?;
    record(name, "vote_2_cancel_tx", tx(&cancel));
    let s = ctx.schedule(&p, l.schedule).await?.context("schedule")?;
    let c = s.cancelled.context("cancelled")?;
    ensure!(
        c.vested_at_cancel == AMOUNTS[0] && c.refunded == TOTAL - AMOUNTS[0],
        "{c:?}"
    );
    ensure!(
        ctx.native(l.refund_to).await? - refund_before == TOTAL - AMOUNTS[0],
        "refund to the refund account"
    );
    record(name, "refunded", c.refunded);
    let dest = ctx.id("dest");
    let dest_before = ctx.native(dest).await?;
    p.claim(project, l.schedule, dest, &Asset::Native)?.send(&ctx).await?;
    ensure!(ctx.native(dest).await? - dest_before == AMOUNTS[0]);
    ensure!(ctx.native(l.schedule).await? == 0, "the escrow is empty");
    refused_by_launchpad(l.approve(&ctx, "approver-a", 1).await, LaunchError::LaunchCancelled)?;
    Ok(())
}
