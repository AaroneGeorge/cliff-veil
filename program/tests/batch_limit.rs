//! Measures the largest `CreateBatch` the chain accepts under its execution budget (SPEC 8): the batch of `N`
//! schedules is accepted under the 10,000,000-cycle cap and the batch of `N + 1` is refused with `OutOfGas`.
//!
//! The batch caps in `core` (`MAX_BATCH_NATIVE`, `MAX_BATCH_TOKEN`) sit below the measured maximum, so the
//! measurement needs a build whose caps are raised above it (for example both set to 2000 in `core/src/create.rs`);
//! with the released caps a search that reaches the cap stops and says so. Run:
//!
//! ```text
//! source scripts/env.sh && cargo test -p vesting_methods --test batch_limit -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Two shapes: linear items with fee 0 (`common::batch_linear`), and the heaviest item, 64 milestones, with a 1% fee
//! on the batch (`common::batch_heaviest`); one beneficiary per item. Output lines:
//! `batch <asset> n=<N> cycles=<C> accepted|refused`, then `max <asset> accepted=<N> refused=<N+1>`.

mod common;

use common::*;
use lee::error::LeeError;
use vesting_core::{Asset, max_batch};

const TOTAL: u128 = 1_000;

/// The two batch shapes measured.
#[derive(Clone, Copy)]
enum Shape {
    /// Linear items, fee 0.
    Linear,
    /// The heaviest items: 64 milestones each, and a 1% fee on the batch.
    Heaviest,
}

/// Runs a batch of `n` on a fresh chain with `budget`; `Ok(cycles)` or the error.
fn batch(token: bool, shape: Shape, n: u32, budget: u64) -> Option<Result<u64, LeeError>> {
    let mut chain = Chain::new(T0)?;
    let p = parties();
    let treasury = id(&p.treasury);
    let supply = 2 * u128::from(n) * TOTAL;
    let asset = if token {
        let def = chain.new_token(&key(0x20), &p.creator, supply);
        chain.put_treasury_ata(treasury, def);
        Asset::Token {
            definition: core_id(def),
        }
    } else {
        chain.fund_native(id(&p.creator), supply);
        Asset::Native
    };
    let (common, fee) = match shape {
        Shape::Linear => (batch_linear(asset), 0),
        Shape::Heaviest => {
            chain.set_fee_config(100, treasury);
            (batch_heaviest(asset), u128::from(n) * TOTAL / 100)
        }
    };
    let sent = chain.create_batch(&p.creator, common, batch_items(0, n, TOTAL), fee, treasury, budget);
    Some(sent.map(|out| out.cycles))
}

fn accepted(token: bool, shape: Shape, n: u32, name: &str) -> Option<bool> {
    let ok = match batch(token, shape, n, CYCLE_CAP)? {
        Ok(cycles) => {
            println!("batch {name} n={n} cycles={cycles} accepted");
            true
        }
        Err(LeeError::OutOfGas { .. }) => {
            // The same batch with no budget, to record what it would cost.
            let cycles = batch(token, shape, n, u64::MAX / 4)?.map_or_else(|e| format!("({e})"), |c| c.to_string());
            println!("batch {name} n={n} cycles={cycles} refused (out of gas)");
            false
        }
        Err(other) => panic!("batch {name} n={n} failed for another reason: {other}"),
    };
    Some(ok)
}

fn search(token: bool, shape: Shape) {
    let name = match (token, shape) {
        (false, Shape::Linear) => "native",
        (true, Shape::Linear) => "token",
        (false, Shape::Heaviest) => "native-64-milestones-with-fee",
        (true, Shape::Heaviest) => "token-64-milestones-with-fee",
    };
    let asset = if token {
        Asset::Token {
            definition: vesting_core::AccountId::new([0; 32]),
        }
    } else {
        Asset::Native
    };
    let cap = u32::try_from(max_batch(&asset)).expect("cap fits u32");
    // lo is accepted, hi is refused.
    let (mut lo, mut hi) = (1u32, 64u32.min(cap));
    loop {
        let Some(ok) = accepted(token, shape, hi, name) else {
            return;
        };
        if !ok {
            break;
        }
        assert!(
            hi < cap,
            "a batch at the cap {cap} fits for {name}: raise the caps in core/src/create.rs to measure the maximum"
        );
        lo = hi;
        hi = (hi * 2).min(cap);
    }
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let Some(ok) = accepted(token, shape, mid, name) else {
            return;
        };
        if ok {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    println!("max {name} accepted={lo} refused={hi} budget={CYCLE_CAP}");
}

#[test]
#[ignore = "measurement: needs a build with raised batch caps (see the module documentation)"]
fn measure_batch_max_native() {
    search(false, Shape::Linear);
}

#[test]
#[ignore = "measurement: needs a build with raised batch caps (see the module documentation)"]
fn measure_batch_max_token() {
    search(true, Shape::Linear);
}

#[test]
#[ignore = "measurement: needs a build with raised batch caps (see the module documentation)"]
fn measure_batch_max_native_heaviest() {
    search(false, Shape::Heaviest);
}

#[test]
#[ignore = "measurement: needs a build with raised batch caps (see the module documentation)"]
fn measure_batch_max_token_heaviest() {
    search(true, Shape::Heaviest);
}
