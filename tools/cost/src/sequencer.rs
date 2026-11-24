//! Sequencer mode: the operations sent to a running sequencer through the end-to-end harness, with the fee each
//! was charged. Start the sequencer with dev mode unset (`scripts/seq.sh pub start`) for real execution.

use anyhow::Result;
use lee_core::account::AccountId;
use vesting_core::Asset;
use vesting_e2e::{
    Ctx, SendError,
    vesting::{linear, milestones},
};

use crate::{Row, Run};

/// Fee charged to `payer` by `op`: the payer's balance change less what the operation itself moved out of it
/// (`f` returns that amount; negative for an operation that pays the payer, such as a cancel refunding it).
async fn measure<F, Fut>(ctx: &Ctx, rows: &mut Vec<Row>, op: &str, payer: AccountId, f: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<i128, SendError>>,
{
    let mut row = Row::new(op, "native", 1);
    let before = ctx.native(payer).await.unwrap_or(0);
    match f().await {
        Ok(moved) => {
            let after = ctx.native(payer).await.unwrap_or(0);
            let fee = i128::try_from(before).unwrap_or(i128::MAX) - i128::try_from(after).unwrap_or(i128::MAX) - moved;
            row.fee = u128::try_from(fee).ok();
            row.status = "ok".into();
        }
        Err(e) => row.status = format!("failed: {}", e.to_string().lines().next().unwrap_or("")),
    }
    rows.push(row);
}

/// Runs the operations against the sequencer at `rpc`.
pub fn run(rpc: &str) -> Result<Run> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(run_async(rpc))
}

async fn run_async(rpc: &str) -> Result<Run> {
    let mut ctx = Ctx::with_rpc(rpc, "cost", &["creator", "beneficiary", "to"]).await?;
    let (creator, ben, to) = (ctx.id("creator"), ctx.id("beneficiary"), ctx.id("to"));
    let mut rows = Vec::new();
    measure(&ctx, &mut rows, "native_transfer", creator, || async {
        ctx.transfer_native("creator", to, 1).await.map(|_| 1)
    })
    .await;
    if let Some(r) = rows.last_mut() {
        r.asset = "-".into();
    }

    match ctx.programs() {
        Err(e) => {
            for op in [
                "create",
                "claim",
                "cancel",
                "make_non_cancelable",
                "signal_milestone",
                "transfer_beneficiary",
            ] {
                let mut r = Row::new(op, "native", 1);
                r.status = format!("not built ({e})");
                rows.push(r);
            }
        }
        Ok(p) => {
            let now = ctx.clock().await?;
            let total = 1_000_000;
            let mut terms = linear(ben, total, now, now, now + 1);
            terms.transferable = true;
            let id = ctx.fresh_id();
            let tx = p.create(creator, id, terms)?;
            let s = tx.schedules[0];
            measure(&ctx, &mut rows, "create", creator, || async {
                tx.send(&ctx).await.map(|_| i128::try_from(total).unwrap_or(0))
            })
            .await;
            ctx.wait_clock(now + 1).await?;
            let claim = p.claim(ben, s, to, &Asset::Native)?;
            measure(&ctx, &mut rows, "claim", ben, || async {
                claim.send(&ctx).await.map(|_| 0)
            })
            .await;

            let now = ctx.clock().await?;
            let s2 = ctx
                .create(&p, "creator", linear(ben, total, now, now, now + 3_600_000))
                .await?;
            let nc = p.make_non_cancelable(creator, s2);
            measure(&ctx, &mut rows, "make_non_cancelable", creator, || async {
                nc.send(&ctx).await.map(|_| 0)
            })
            .await;
            let s3 = ctx
                .create(&p, "creator", linear(ben, total, now, now, now + 3_600_000))
                .await?;
            let state = ctx
                .schedule(&p, s3)
                .await?
                .ok_or_else(|| anyhow::anyhow!("schedule missing"))?;
            let cancel = p.cancel(creator, s3, &state)?;
            measure(&ctx, &mut rows, "cancel", creator, || async {
                cancel.send(&ctx).await?;
                let refunded = ctx
                    .schedule(&p, s3)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|s| s.cancelled)
                    .map_or(0, |c| c.refunded);
                Ok(-i128::try_from(refunded).unwrap_or(0))
            })
            .await;
            let s4 = ctx.create(&p, "creator", milestones(ben, vec![total])).await?;
            let sig = p.signal(creator, s4, 0);
            measure(&ctx, &mut rows, "signal_milestone", creator, || async {
                sig.send(&ctx).await.map(|_| 0)
            })
            .await;
            let mut t = linear(ben, total, now, now, now + 3_600_000);
            t.transferable = true;
            let s5 = ctx.create(&p, "creator", t).await?;
            let tr = p.transfer(ben, s5, to);
            measure(&ctx, &mut rows, "transfer_beneficiary", ben, || async {
                tr.send(&ctx).await.map(|_| 0)
            })
            .await;
        }
    }
    let dev = std::env::var("RISC0_DEV_MODE").unwrap_or_default();
    Ok(Run {
        mode: "sequencer".into(),
        lez: "v0.4.0-rc3".into(),
        inputs: vec![
            ("rpc".into(), rpc.to_owned()),
            (
                "RISC0_DEV_MODE of this client".into(),
                if dev.is_empty() { "unset".into() } else { dev },
            ),
        ],
        rows,
    })
}
