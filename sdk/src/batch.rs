//! How many schedules fit in one `CreateBatch` transaction (SPEC 8).
//!
//! A batch runs in one transaction under the chain's execution budget (`MAX_GAS_EXEC`, 10,000,000 cycles in rc3);
//! a batch over it is refused whole. The program's caps (`MAX_BATCH_NATIVE`, `MAX_BATCH_TOKEN`) are set for linear
//! items, but an item's cost grows with its milestone count and a creation fee adds a transfer, so the heaviest
//! batch at the cap does not fit. The SDK therefore splits a recipient list by an estimated cycle cost:
//!
//! ```text
//! cycles(n) = base(asset, fee) + n * (item(asset) + milestones * per_milestone)
//! ```
//!
//! and fills each transaction up to [`TARGET_CYCLES`], 85% of the budget, never above the program's cap. The
//! coefficients are an upper fit of the executor measurements of the program (`program/tests/batch_limit.rs` and
//! the cycle lines of `program/tests/executor.rs`): the model is at or above every measured batch, and the sizes it
//! picks for the four measured shapes are the sizes the executor tests show to fit (see the tests below). The local
//! dry run of each transaction then measures the real cycles before anything is sent.

use vesting_core::{Asset, BatchShape, max_batch};

/// The chain's execution budget per transaction (rc3 fee market `MAX_GAS_EXEC`).
pub const MAX_GAS_EXEC: u64 = 10_000_000;

/// What the SDK fills a batch transaction up to: 85% of [`MAX_GAS_EXEC`], the margin the program's caps use.
pub const TARGET_CYCLES: u64 = MAX_GAS_EXEC / 100 * 85;

/// Cycle model of one asset kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostModel {
    /// Fixed part of a batch transaction with no fee.
    pub base: u64,
    /// Added once when the batch pays a creation fee (the fee transfer).
    pub fee: u64,
    /// Per item, for a linear item.
    pub item: u64,
    /// Per milestone of an item.
    pub per_milestone: u64,
}

/// Native schedules. Measured: 64 linear items 3,349,874 and 128 items 6,658,479 cycles (51,697 per item); 32 and
/// 48 items of 64 milestones with a fee 5,257,261 and 7,854,228 (162,310 per item).
pub const NATIVE: CostModel = CostModel {
    base: 45_000,
    fee: 25_000,
    item: 52_000,
    per_milestone: 1_750,
};

/// Token schedules. Measured: 64 and 67 linear items 9,471,530 and 9,912,873 cycles (147,114 per item); 32 and 38
/// items of 64 milestones with a fee 8,376,967 and 9,925,636 (258,111 per item).
pub const TOKEN: CostModel = CostModel {
    base: 60_000,
    fee: 65_000,
    item: 148_000,
    per_milestone: 1_750,
};

/// The model for `asset`.
#[must_use]
pub const fn model(asset: &Asset) -> CostModel {
    match asset {
        Asset::Native => NATIVE,
        Asset::Token { .. } => TOKEN,
    }
}

/// Milestones per item of a batch shape (0 for linear).
#[must_use]
pub fn milestones_of(shape: &BatchShape) -> u64 {
    match shape {
        BatchShape::Linear { .. } => 0,
        BatchShape::Milestones { bps } => bps.len() as u64,
    }
}

/// Estimated cycles of a `CreateBatch` of `n` items of `asset` with `milestones` each, with or without a fee.
#[must_use]
pub fn estimate_cycles(asset: &Asset, milestones: u64, with_fee: bool, n: u64) -> u64 {
    let m = model(asset);
    let per_item = m.item.saturating_add(milestones.saturating_mul(m.per_milestone));
    m.base
        .saturating_add(if with_fee { m.fee } else { 0 })
        .saturating_add(n.saturating_mul(per_item))
}

/// The most items of this shape one transaction takes: the largest `n` with
/// `estimate_cycles(n) <= TARGET_CYCLES`, at least 1, at most the program's cap.
#[must_use]
pub fn capacity(asset: &Asset, milestones: u64, with_fee: bool) -> usize {
    let m = model(asset);
    let fixed = m.base.saturating_add(if with_fee { m.fee } else { 0 });
    let per_item = m.item.saturating_add(milestones.saturating_mul(m.per_milestone));
    let fit = TARGET_CYCLES.saturating_sub(fixed) / per_item.max(1);
    usize::try_from(fit).unwrap_or(usize::MAX).clamp(1, max_batch(asset))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_ASSET: Asset = Asset::Token {
        definition: vesting_core::AccountId::new([1; 32]),
    };

    /// Every measured batch (program/tests/batch_limit.rs, program/tests/executor.rs): (asset, milestones, fee, n,
    /// measured cycles).
    const MEASURED: [(bool, u64, bool, u64, u64); 16] = [
        (false, 0, false, 64, 3_349_874),
        (false, 0, false, 128, 6_658_479),
        (false, 0, false, 164, 8_515_722),
        (false, 0, false, 192, 9_943_709),
        (false, 0, false, 193, 9_995_075),
        (false, 64, true, 32, 5_257_261),
        (false, 64, true, 51, 8_357_078),
        (false, 64, true, 60, 9_805_014),
        (false, 64, true, 61, 9_967_430),
        (true, 0, false, 3, 488_424),
        (true, 0, false, 57, 8_446_110),
        (true, 0, false, 64, 9_471_530),
        (true, 0, false, 67, 9_912_873),
        (true, 64, true, 32, 8_386_997),
        (true, 64, true, 36, 9_408_960),
        (true, 64, true, 38, 9_925_636),
    ];

    fn asset(token: bool) -> Asset {
        if token { TOKEN_ASSET } else { Asset::Native }
    }

    #[test]
    fn f5_model_is_at_or_above_every_measured_batch() {
        for (token, m, fee, n, measured) in MEASURED {
            let est = estimate_cycles(&asset(token), m, fee, n);
            assert!(
                est >= measured,
                "token={token} m={m} fee={fee} n={n}: model {est} < measured {measured}"
            );
            // And not wastefully far above it (within 5%).
            assert!(
                est <= measured + measured / 20,
                "token={token} m={m} fee={fee} n={n}: model {est} too high"
            );
        }
    }

    #[test]
    fn f5_capacity_picks_sizes_the_executor_shows_to_fit() {
        // Linear native: 162 (cap 164, measured maximum 193). Heaviest native: 51 (measured to fit, maximum 61).
        assert_eq!(capacity(&Asset::Native, 0, false), 162);
        assert_eq!(capacity(&Asset::Native, 64, true), 51);
        // Linear token: 57 (the cap; measured maximum 67). Heaviest token: 32 (measured to fit, maximum 38).
        assert_eq!(capacity(&TOKEN_ASSET, 0, false), 57);
        assert_eq!(capacity(&TOKEN_ASSET, 64, true), 32);
    }

    #[test]
    fn f5_every_capacity_fits_the_budget_with_margin() {
        for token in [false, true] {
            for m in [0, 1, 8, 32, 64] {
                for fee in [false, true] {
                    let a = asset(token);
                    let n = capacity(&a, m, fee);
                    assert!(n >= 1 && n <= max_batch(&a));
                    assert!(
                        estimate_cycles(&a, m, fee, n as u64) <= TARGET_CYCLES,
                        "token={token} m={m} fee={fee}"
                    );
                    // The next size up would exceed the target, unless the program's cap stopped first.
                    if n < max_batch(&a) {
                        assert!(estimate_cycles(&a, m, fee, n as u64 + 1) > TARGET_CYCLES);
                    }
                }
            }
        }
    }
}
