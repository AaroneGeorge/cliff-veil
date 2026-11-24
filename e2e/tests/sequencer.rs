//! The harness itself against a standalone sequencer: reachability, isolated funded accounts, the clock, and a
//! refusal read before sending. These need no vesting program and run in the `e2e` CI job.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::Result;
use vesting_e2e::{Ctx, SendError};

#[test]
#[ignore = "needs a sequencer: e2e/run.sh"]
fn s2_sequencer_rpc_accepts_connections() {
    let url = vesting_e2e::rpc_url();
    let hp = vesting_e2e::host_port(&url).unwrap_or_else(|| panic!("CV_SEQ_RPC={url}: expected http://host:port"));
    let addr = hp
        .to_socket_addrs()
        .expect("resolve sequencer address")
        .next()
        .expect("at least one address");
    TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .unwrap_or_else(|e| panic!("sequencer RPC {url} not reachable: {e}"));
}

/// Two tests' accounts are funded in parallel, each with exactly the funding amount, and share nothing.
#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn s2_harness_funds_isolated_accounts_concurrently() -> Result<()> {
    let (a, b) = tokio::join!(Ctx::new("fund-a", &["x", "y"]), Ctx::new("fund-b", &["x"]));
    let (a, b) = (a?, b?);
    let fund = u128::from(
        std::env::var("CV_E2E_FUND")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1_000_000_000),
    );
    for (ctx, party) in [(&a, "x"), (&a, "y"), (&b, "x")] {
        assert_eq!(ctx.native(ctx.id(party)).await?, fund, "{} {party}", ctx.name);
    }
    assert_ne!(a.id("x"), b.id("x"), "fresh keys per test");
    Ok(())
}

/// The clock moves with the blocks, and `wait_clock` returns once a transaction would read the target time.
#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn s2_harness_clock_advances_with_blocks() -> Result<()> {
    let ctx = Ctx::new("clock", &[]).await?;
    let t0 = ctx.clock().await?;
    let reached = ctx.wait_clock(t0 + 1).await?;
    assert!(reached > t0, "clock {t0} -> {reached}");
    Ok(())
}

/// A transfer, included; then one the wallet's simulation refuses, so nothing is sent and nothing changes.
#[tokio::test]
#[ignore = "needs a sequencer: e2e/run.sh"]
async fn s2_harness_sends_and_reads_a_refusal_before_sending() -> Result<()> {
    let ctx = Ctx::new("send", &["from", "to"]).await?;
    let to = ctx.id("to");
    let before = ctx.native(to).await?;
    let included = ctx.transfer_native("from", to, 1_000).await?;
    assert!(included.block > 0);
    assert_eq!(ctx.native(to).await?, before + 1_000);

    let from_before = ctx.native(ctx.id("from")).await?;
    let err = ctx
        .transfer_native("from", to, from_before * 2)
        .await
        .expect_err("more than the balance");
    assert!(matches!(err, SendError::Refused { code: None, .. }), "{err}");
    assert_eq!(
        ctx.native(ctx.id("from")).await?,
        from_before,
        "a refused send costs nothing"
    );
    Ok(())
}
