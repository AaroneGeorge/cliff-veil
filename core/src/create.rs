//! Creation: terms, their validation, batches and the schedule seed (SPEC sections 1.2, 3.1, 4 and 8).

use alloc::vec::Vec;

use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Digest, Sha256};

use crate::math::mul_div_floor;
use crate::state::{AccountId, Asset, Kind, MAX_MILESTONES, ScheduleV1};
use crate::{Result, VestingError};

/// Most schedules in one native batch (SPEC section 8). The chain accepts a batch of 193 linear native schedules
/// under its 10,000,000-cycle execution cap and refuses 194 (`program/tests/batch_limit.rs`); the cap is 85% of
/// that. Heavier items fit fewer: with 64 milestones per item and a fee, 61 fit and 62 do not. A batch over the
/// budget is refused as a whole by the chain (out of gas) and changes nothing; the SDK sizes batches by a dry run.
pub const MAX_BATCH_NATIVE: usize = 164;
/// Most schedules in one token batch (SPEC section 8). Measured as [`MAX_BATCH_NATIVE`]: 67 linear token schedules
/// fit and 68 do not, so the cap is 85% of 67. With 64 milestones per item and a fee: 38 fit, 39 do not.
pub const MAX_BATCH_TOKEN: usize = 57;
/// Basis points in a whole (milestone weights of a batch sum to this).
const BPS_WHOLE: u64 = 10_000;

/// The 22-byte prefix of the schedule seed preimage.
pub const SCHEDULE_SEED_PREFIX: &[u8; 22] = b"cliff-veil/schedule/v1";

/// The bytes hashed into the schedule seed: `"cliff-veil/schedule/v1" ‖ creator ‖ schedule_id` (86 bytes).
///
/// Exposed so the program can hash it with the zkVM's accelerated SHA-256 instead of [`schedule_seed`].
#[must_use]
pub fn schedule_seed_preimage(creator: &AccountId, schedule_id: &[u8; 32]) -> [u8; 86] {
    let mut out = [0u8; 86];
    out[..22].copy_from_slice(SCHEDULE_SEED_PREFIX);
    out[22..54].copy_from_slice(creator.value());
    out[54..].copy_from_slice(schedule_id);
    out
}

/// The schedule seed: `SHA-256("cliff-veil/schedule/v1" ‖ creator ‖ schedule_id)`. The schedule account is
/// `AccountId::for_public_pda(vesting, PdaSeed::new(seed))` (LEZ), which this crate does not compute.
#[must_use]
pub fn schedule_seed(creator: &AccountId, schedule_id: &[u8; 32]) -> [u8; 32] {
    Sha256::digest(schedule_seed_preimage(creator, schedule_id)).into()
}

/// The terms of one schedule, as `Create` carries them (SPEC 3.1).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Terms {
    /// Who may claim.
    pub beneficiary: AccountId,
    /// What is vested.
    pub asset: Asset,
    /// Amount escrowed; greater than zero.
    pub total: u128,
    /// The vesting curve. `Milestones.signalled` must be 0.
    pub kind: Kind,
    /// SDK default: `true`.
    pub cancelable: bool,
    /// SDK default: `false`.
    pub transferable: bool,
    /// `None`: the creator.
    pub cancel_authority: Option<AccountId>,
    /// `None`: the creator.
    pub milestone_authority: Option<AccountId>,
    /// `None`: the funding account (`source` of the creation message).
    pub refund_to: Option<AccountId>,
}

impl Terms {
    /// Builds the initial state of a schedule created by `creator` with `schedule_id` and funded from `source`,
    /// filling the defaults (authorities default to the creator, the refund account to `source`: the account that
    /// funded the escrow is the one known to accept the asset back), after checking the terms.
    ///
    /// # Errors
    /// As [`ScheduleV1::validate_new`]: 10, 11, 12, 13 or 29.
    pub fn into_state(self, creator: AccountId, schedule_id: [u8; 32], source: AccountId) -> Result<ScheduleV1> {
        let state = ScheduleV1 {
            schedule_id,
            creator,
            beneficiary: self.beneficiary,
            asset: self.asset,
            total: self.total,
            claimed: 0,
            last_time: 0,
            kind: self.kind,
            cancelable: self.cancelable,
            transferable: self.transferable,
            cancel_authority: self.cancel_authority.unwrap_or(creator),
            milestone_authority: self.milestone_authority.unwrap_or(creator),
            refund_to: self.refund_to.unwrap_or(source),
            cancelled: None,
            pending: None,
        };
        state.validate_new()?;
        Ok(state)
    }
}

/// Checks a vesting curve against `total`. Shared by single and batch creation.
fn validate_kind(total: u128, kind: &Kind) -> Result<()> {
    match kind {
        &Kind::Linear { start, cliff, end } => {
            if start <= cliff && cliff <= end && start < end {
                Ok(())
            } else {
                Err(VestingError::BadTimes)
            }
        }
        Kind::Milestones { amounts, signalled } => {
            if amounts.len() > MAX_MILESTONES {
                return Err(VestingError::TooManyMilestones);
            }
            if amounts.is_empty() || *signalled != 0 || amounts.contains(&0) {
                return Err(VestingError::BadMilestones);
            }
            let sum = amounts
                .iter()
                .try_fold(0u128, |sum, a| sum.checked_add(*a))
                .ok_or(VestingError::Overflow)?;
            if sum == total {
                Ok(())
            } else {
                Err(VestingError::BadMilestones)
            }
        }
    }
}

impl ScheduleV1 {
    /// Checks a state about to be written by `Open`: the terms rules of SPEC 2 and 3.1, and a fresh schedule
    /// (nothing claimed, `last_time` 0, not cancelled, nothing pending, no milestone signalled). The program also
    /// checks `refund_to` against the receiving account with [`crate::rules::check_destination`] (26).
    ///
    /// # Errors
    /// In this order: [`VestingError::ZeroTotal`] (10); [`VestingError::BadTimes`] (11) unless
    /// `start <= cliff <= end` and `start < end`; [`VestingError::TooManyMilestones`] (13) above 64 amounts;
    /// [`VestingError::BadMilestones`] (12) for no amounts, a zero amount, a signalled bit, or a sum other than
    /// `total`; [`VestingError::Overflow`] (29) when the amounts overflow `u128`; [`VestingError::InvalidMessage`]
    /// (2) when the state is not fresh (`claimed`, `last_time`, `cancelled` or `pending` set), which only a
    /// hand-made `Open` could carry.
    pub fn validate_new(&self) -> Result<()> {
        if self.total == 0 {
            return Err(VestingError::ZeroTotal);
        }
        validate_kind(self.total, &self.kind)?;
        if self.claimed != 0 || self.last_time != 0 || self.cancelled.is_some() || self.pending.is_some() {
            return Err(VestingError::InvalidMessage);
        }
        Ok(())
    }
}

/// Terms shared by every item of a `CreateBatch` (SPEC 3.1).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BatchTerms {
    /// What is vested.
    pub asset: Asset,
    /// The curve every item follows.
    pub shape: BatchShape,
    /// As [`Terms::cancelable`].
    pub cancelable: bool,
    /// As [`Terms::transferable`].
    pub transferable: bool,
    /// As [`Terms::cancel_authority`].
    pub cancel_authority: Option<AccountId>,
    /// As [`Terms::milestone_authority`].
    pub milestone_authority: Option<AccountId>,
    /// As [`Terms::refund_to`].
    pub refund_to: Option<AccountId>,
}

/// The curve of a batch.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum BatchShape {
    /// Tag 0: the same times for every item.
    Linear {
        /// As [`Kind::Linear`].
        start: u64,
        /// As [`Kind::Linear`].
        cliff: u64,
        /// As [`Kind::Linear`].
        end: u64,
    },
    /// Tag 1: milestone weights in basis points, summing to 10 000. Item amounts are
    /// `floor(total * bps / 10_000)`; the last milestone takes the remainder ([`batch_milestone_amounts`]).
    Milestones {
        /// One weight per milestone.
        bps: Vec<u16>,
    },
}

/// One schedule of a batch.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BatchItem {
    /// As in `Create`.
    pub schedule_id: [u8; 32],
    /// Who may claim.
    pub beneficiary: AccountId,
    /// Amount escrowed for this item.
    pub total: u128,
}

/// Splits `total` into milestone amounts by basis-point weights: `floor(total * bps[i] / 10_000)` for every
/// milestone but the last, which takes `total - sum(others)`. The amounts always sum to `total` exactly.
///
/// # Errors
/// [`VestingError::TooManyMilestones`] (13) above 64 weights; [`VestingError::BadMilestones`] (12) for no weights,
/// a zero weight, weights not summing to 10 000, or a total so small that a milestone gets 0;
/// [`VestingError::ZeroTotal`] (10) for a zero total.
pub fn batch_milestone_amounts(total: u128, bps: &[u16]) -> Result<Vec<u128>> {
    if total == 0 {
        return Err(VestingError::ZeroTotal);
    }
    if bps.len() > MAX_MILESTONES {
        return Err(VestingError::TooManyMilestones);
    }
    let weight_sum: u64 = bps.iter().map(|b| u64::from(*b)).sum();
    if bps.is_empty() || bps.contains(&0) || weight_sum != BPS_WHOLE {
        return Err(VestingError::BadMilestones);
    }
    let Some((_, init)) = bps.split_last() else {
        return Err(VestingError::BadMilestones);
    };
    let mut amounts = Vec::with_capacity(bps.len());
    let mut assigned: u128 = 0;
    for b in init {
        // bps <= 10_000 here, so the amount is at most total and the running sum at most total.
        let amount = mul_div_floor(total, u64::from(*b), BPS_WHOLE).ok_or(VestingError::Overflow)?;
        assigned = assigned.checked_add(amount).ok_or(VestingError::Overflow)?;
        amounts.push(amount);
    }
    amounts.push(total.checked_sub(assigned).ok_or(VestingError::Overflow)?);
    if amounts.contains(&0) {
        return Err(VestingError::BadMilestones);
    }
    Ok(amounts)
}

impl BatchTerms {
    /// The vesting curve of an item with `total`.
    ///
    /// # Errors
    /// As [`batch_milestone_amounts`] for milestones; none for linear (times are checked by
    /// [`ScheduleV1::validate_new`]).
    pub fn kind_for(&self, total: u128) -> Result<Kind> {
        match &self.shape {
            &BatchShape::Linear { start, cliff, end } => Ok(Kind::Linear { start, cliff, end }),
            BatchShape::Milestones { bps } => Ok(Kind::Milestones {
                amounts: batch_milestone_amounts(total, bps)?,
                signalled: 0,
            }),
        }
    }

    /// The full terms of one item.
    ///
    /// # Errors
    /// As [`Self::kind_for`].
    pub fn terms_for(&self, item: &BatchItem) -> Result<Terms> {
        Ok(Terms {
            beneficiary: item.beneficiary,
            asset: self.asset,
            total: item.total,
            kind: self.kind_for(item.total)?,
            cancelable: self.cancelable,
            transferable: self.transferable,
            cancel_authority: self.cancel_authority,
            milestone_authority: self.milestone_authority,
            refund_to: self.refund_to,
        })
    }
}

/// The batch cap for `asset`: [`MAX_BATCH_NATIVE`] or [`MAX_BATCH_TOKEN`].
#[must_use]
pub const fn max_batch(asset: &Asset) -> usize {
    match asset {
        Asset::Native => MAX_BATCH_NATIVE,
        Asset::Token { .. } => MAX_BATCH_TOKEN,
    }
}

/// Validates a whole batch created by `creator` and funded from `source` before anything is opened, and builds
/// every item's initial state. Returns the states in item order and the sum of item totals (the base of the batch
/// fee).
///
/// # Errors
/// [`VestingError::EmptyBatch`] (24); [`VestingError::BatchTooLarge`] (25) above [`max_batch`];
/// [`VestingError::ScheduleExists`] (7) when two items share a `schedule_id` (the second `Open` would be refused
/// anyway; refusing up front names the cause); any error of [`Terms::into_state`] for the first bad item;
/// [`VestingError::Overflow`] (29) when the totals overflow `u128`.
pub fn validate_batch(
    creator: AccountId,
    source: AccountId,
    common: &BatchTerms,
    items: &[BatchItem],
) -> Result<(Vec<ScheduleV1>, u128)> {
    if items.is_empty() {
        return Err(VestingError::EmptyBatch);
    }
    if items.len() > max_batch(&common.asset) {
        return Err(VestingError::BatchTooLarge);
    }
    let mut ids: Vec<&[u8; 32]> = items.iter().map(|i| &i.schedule_id).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|w| w[0] == w[1]) {
        return Err(VestingError::ScheduleExists);
    }
    let mut states = Vec::with_capacity(items.len());
    let mut sum: u128 = 0;
    for item in items {
        states.push(common.terms_for(item)?.into_state(creator, item.schedule_id, source)?);
        sum = sum.checked_add(item.total).ok_or(VestingError::Overflow)?;
    }
    Ok((states, sum))
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    const CREATOR: AccountId = AccountId::new([1; 32]);
    const BENEFICIARY: AccountId = AccountId::new([2; 32]);
    const OTHER: AccountId = AccountId::new([3; 32]);

    fn terms(total: u128, kind: Kind) -> Terms {
        Terms {
            beneficiary: BENEFICIARY,
            asset: Asset::Native,
            total,
            kind,
            cancelable: true,
            transferable: false,
            cancel_authority: None,
            milestone_authority: None,
            refund_to: None,
        }
    }

    fn linear(start: u64, cliff: u64, end: u64) -> Kind {
        Kind::Linear { start, cliff, end }
    }

    fn ms(amounts: &[u128]) -> Kind {
        Kind::Milestones {
            amounts: amounts.to_vec(),
            signalled: 0,
        }
    }

    fn common(asset: Asset, shape: BatchShape) -> BatchTerms {
        BatchTerms {
            asset,
            shape,
            cancelable: true,
            transferable: false,
            cancel_authority: None,
            milestone_authority: None,
            refund_to: None,
        }
    }

    fn items(n: usize) -> Vec<BatchItem> {
        (0..n)
            .map(|i| {
                let mut id = [0u8; 32];
                id[..8].copy_from_slice(&(i as u64).to_le_bytes());
                BatchItem {
                    schedule_id: id,
                    beneficiary: BENEFICIARY,
                    total: 1_000 + i as u128,
                }
            })
            .collect()
    }

    // ---- defaults ----

    #[test]
    fn refund_defaults_to_the_funding_account() {
        let s = terms(10, linear(0, 0, 10)).into_state(CREATOR, [5; 32], OTHER).unwrap();
        assert_eq!(s.refund_to, OTHER, "funded from another account: refunds go back to it");
        assert_eq!((s.cancel_authority, s.milestone_authority), (CREATOR, CREATOR));
        let c = common(
            Asset::Native,
            BatchShape::Linear {
                start: 0,
                cliff: 0,
                end: 10,
            },
        );
        let (states, _) = validate_batch(CREATOR, OTHER, &c, &items(2)).unwrap();
        assert!(states.iter().all(|s| s.refund_to == OTHER));
    }

    #[test]
    fn f3_defaults_when_the_creator_funds_the_schedule() {
        let s = terms(10, linear(0, 0, 10))
            .into_state(CREATOR, [5; 32], CREATOR)
            .unwrap();
        assert_eq!(
            (s.cancel_authority, s.milestone_authority, s.refund_to),
            (CREATOR, CREATOR, CREATOR)
        );
        assert_eq!((s.claimed, s.cancelled, s.pending), (0, None, None));
        assert_eq!(
            (s.creator, s.schedule_id, s.beneficiary),
            (CREATOR, [5; 32], BENEFICIARY)
        );
    }

    #[test]
    fn soft_separate_authorities_are_kept() {
        let mut t = terms(10, ms(&[10]));
        t.cancel_authority = Some(OTHER);
        t.milestone_authority = Some(BENEFICIARY);
        t.refund_to = Some(OTHER);
        let s = t.into_state(CREATOR, [5; 32], CREATOR).unwrap();
        assert_eq!(
            (s.cancel_authority, s.milestone_authority, s.refund_to),
            (OTHER, BENEFICIARY, OTHER)
        );
    }

    #[test]
    fn f4_transferable_flag_is_written_at_creation() {
        let mut t = terms(10, linear(0, 0, 10));
        t.transferable = true;
        assert!(t.into_state(CREATOR, [5; 32], CREATOR).unwrap().transferable);
    }

    // ---- validation, one test per code ----

    #[test]
    fn zero_total_is_10() {
        assert_eq!(
            terms(0, linear(0, 0, 10)).into_state(CREATOR, [0; 32], CREATOR),
            Err(VestingError::ZeroTotal)
        );
        assert_eq!(VestingError::ZeroTotal.code(), 10);
    }

    #[test]
    fn bad_times_are_11() {
        for kind in [
            linear(10, 10, 10),
            linear(10, 5, 20),
            linear(10, 25, 20),
            linear(20, 20, 10),
            linear(5, 0, 0),
        ] {
            assert_eq!(
                terms(1, kind.clone()).into_state(CREATOR, [0; 32], CREATOR),
                Err(VestingError::BadTimes),
                "{kind:?}"
            );
        }
        assert_eq!(VestingError::BadTimes.code(), 11);
    }

    #[test]
    fn f1a_f1b_good_times_are_accepted() {
        for kind in [
            linear(0, 0, 1),
            linear(0, 1, 1),
            linear(10, 15, 20),
            linear(0, 0, u64::MAX),
        ] {
            terms(1, kind).into_state(CREATOR, [0; 32], CREATOR).unwrap();
        }
    }

    #[test]
    fn bad_milestones_are_12() {
        for kind in [
            ms(&[]),
            ms(&[5, 0, 5]),
            ms(&[4, 5]),
            ms(&[6, 5]),
            Kind::Milestones {
                amounts: vec![10],
                signalled: 1,
            },
        ] {
            assert_eq!(
                terms(10, kind.clone()).into_state(CREATOR, [0; 32], CREATOR),
                Err(VestingError::BadMilestones),
                "{kind:?}"
            );
        }
        assert_eq!(VestingError::BadMilestones.code(), 12);
    }

    #[test]
    fn more_than_64_milestones_is_13() {
        let amounts = vec![1u128; 65];
        assert_eq!(
            terms(65, ms(&amounts)).into_state(CREATOR, [0; 32], CREATOR),
            Err(VestingError::TooManyMilestones)
        );
        terms(64, ms(&amounts[..64]))
            .into_state(CREATOR, [0; 32], CREATOR)
            .unwrap();
        assert_eq!(VestingError::TooManyMilestones.code(), 13);
    }

    #[test]
    fn milestone_sum_overflow_is_29_not_a_panic() {
        assert_eq!(
            terms(u128::MAX, ms(&[u128::MAX, 1])).into_state(CREATOR, [0; 32], CREATOR),
            Err(VestingError::Overflow)
        );
    }

    #[test]
    fn open_refuses_a_state_that_is_not_fresh() {
        let fresh = terms(10, linear(0, 0, 10))
            .into_state(CREATOR, [0; 32], CREATOR)
            .unwrap();
        let mut claimed = fresh.clone();
        claimed.claimed = 1;
        let mut pending = fresh.clone();
        pending.pending = Some(crate::Pending::Cancel);
        let mut timed = fresh.clone();
        timed.last_time = 1;
        let mut cancelled = fresh.clone();
        cancelled.cancelled = Some(crate::Cancellation {
            at: 0,
            vested_at_cancel: 0,
            refunded: 10,
        });
        for s in [claimed, timed, pending, cancelled] {
            assert_eq!(s.validate_new(), Err(VestingError::InvalidMessage));
        }
        fresh.validate_new().unwrap();
    }

    // ---- seed ----

    #[test]
    fn seed_preimage_layout() {
        let p = schedule_seed_preimage(&CREATOR, &[9; 32]);
        assert_eq!(&p[..22], b"cliff-veil/schedule/v1");
        assert_eq!(&p[22..54], &[1; 32]);
        assert_eq!(&p[54..], &[9; 32]);
    }

    #[test]
    fn seed_is_sha256_of_preimage_and_pinned() {
        let seed = schedule_seed(&AccountId::new([0; 32]), &[0; 32]);
        // SHA-256 of the 22-byte prefix followed by 64 zero bytes, computed independently.
        assert_eq!(seed[..4], [0xc4, 0x37, 0x39, 0xa8]);
        assert_eq!(seed[28..], [0xbd, 0x5f, 0x12, 0x0d]);
        assert_ne!(
            seed,
            schedule_seed(&AccountId::new([0; 32]), &[1; 32]),
            "the id matters"
        );
        assert_ne!(
            seed,
            schedule_seed(&AccountId::new([1; 32]), &[0; 32]),
            "the creator matters"
        );
    }

    // ---- F5: batch ----

    #[test]
    fn f5_batch_builds_one_state_per_item_and_sums_totals() {
        let c = common(
            Asset::Native,
            BatchShape::Linear {
                start: 0,
                cliff: 5,
                end: 10,
            },
        );
        let (states, sum) = validate_batch(CREATOR, CREATOR, &c, &items(8)).unwrap();
        assert_eq!(states.len(), 8);
        assert_eq!(sum, (0..8).map(|i| 1_000 + i).sum::<u128>());
        for (s, item) in states.iter().zip(items(8)) {
            assert_eq!(
                (s.schedule_id, s.total, s.creator),
                (item.schedule_id, item.total, CREATOR)
            );
        }
    }

    #[test]
    fn f5_empty_batch_is_24() {
        let c = common(
            Asset::Native,
            BatchShape::Linear {
                start: 0,
                cliff: 0,
                end: 10,
            },
        );
        assert_eq!(validate_batch(CREATOR, CREATOR, &c, &[]), Err(VestingError::EmptyBatch));
        assert_eq!(VestingError::EmptyBatch.code(), 24);
    }

    #[test]
    fn f5_batch_max_and_max_plus_one() {
        let native = common(
            Asset::Native,
            BatchShape::Linear {
                start: 0,
                cliff: 0,
                end: 10,
            },
        );
        assert!(validate_batch(CREATOR, CREATOR, &native, &items(MAX_BATCH_NATIVE)).is_ok());
        assert_eq!(
            validate_batch(CREATOR, CREATOR, &native, &items(MAX_BATCH_NATIVE + 1)),
            Err(VestingError::BatchTooLarge)
        );
        let token = common(
            Asset::Token { definition: OTHER },
            BatchShape::Linear {
                start: 0,
                cliff: 0,
                end: 10,
            },
        );
        assert!(validate_batch(CREATOR, CREATOR, &token, &items(MAX_BATCH_TOKEN)).is_ok());
        assert_eq!(
            validate_batch(CREATOR, CREATOR, &token, &items(MAX_BATCH_TOKEN + 1)),
            Err(VestingError::BatchTooLarge)
        );
        assert_eq!(VestingError::BatchTooLarge.code(), 25);
    }

    #[test]
    fn f5_duplicate_schedule_id_in_batch_is_7() {
        let c = common(
            Asset::Native,
            BatchShape::Linear {
                start: 0,
                cliff: 0,
                end: 10,
            },
        );
        let mut list = items(3);
        list[2].schedule_id = list[0].schedule_id;
        assert_eq!(
            validate_batch(CREATOR, CREATOR, &c, &list),
            Err(VestingError::ScheduleExists)
        );
        assert_eq!(VestingError::ScheduleExists.code(), 7);
    }

    #[test]
    fn f5_bad_item_refuses_the_whole_batch() {
        let c = common(
            Asset::Native,
            BatchShape::Linear {
                start: 0,
                cliff: 0,
                end: 10,
            },
        );
        let mut list = items(3);
        list[1].total = 0;
        assert_eq!(
            validate_batch(CREATOR, CREATOR, &c, &list),
            Err(VestingError::ZeroTotal)
        );
        let bad_times = common(
            Asset::Native,
            BatchShape::Linear {
                start: 9,
                cliff: 0,
                end: 10,
            },
        );
        assert_eq!(
            validate_batch(CREATOR, CREATOR, &bad_times, &items(1)),
            Err(VestingError::BadTimes)
        );
    }

    #[test]
    fn f5_batch_total_overflow_is_29() {
        let c = common(
            Asset::Native,
            BatchShape::Linear {
                start: 0,
                cliff: 0,
                end: 10,
            },
        );
        let mut list = items(2);
        list[0].total = u128::MAX;
        assert_eq!(validate_batch(CREATOR, CREATOR, &c, &list), Err(VestingError::Overflow));
    }

    #[test]
    fn f5_batch_milestones_split_by_bps_with_remainder_last() {
        assert_eq!(
            batch_milestone_amounts(1_001, &[2_500, 2_500, 5_000]).unwrap(),
            vec![250, 250, 501]
        );
        assert_eq!(
            batch_milestone_amounts(7, &[3_333, 3_333, 3_334]).unwrap(),
            vec![2, 2, 3]
        );
        assert_eq!(batch_milestone_amounts(u128::MAX, &[10_000]).unwrap(), vec![u128::MAX]);
        let c = common(
            Asset::Native,
            BatchShape::Milestones {
                bps: vec![4_000, 6_000],
            },
        );
        let (states, _) = validate_batch(CREATOR, CREATOR, &c, &items(2)).unwrap();
        assert_eq!(
            states[0].kind,
            Kind::Milestones {
                amounts: vec![400, 600],
                signalled: 0
            }
        );
    }

    #[test]
    fn f5_batch_milestone_weights_are_checked() {
        assert_eq!(batch_milestone_amounts(100, &[]), Err(VestingError::BadMilestones));
        assert_eq!(
            batch_milestone_amounts(100, &[5_000, 4_999]),
            Err(VestingError::BadMilestones)
        );
        assert_eq!(
            batch_milestone_amounts(100, &[10_000, 0]),
            Err(VestingError::BadMilestones)
        );
        assert_eq!(
            batch_milestone_amounts(100, &[u16::MAX, u16::MAX]),
            Err(VestingError::BadMilestones)
        );
        assert_eq!(
            batch_milestone_amounts(1, &[5_000, 5_000]),
            Err(VestingError::BadMilestones),
            "first gets 0"
        );
        assert_eq!(batch_milestone_amounts(0, &[10_000]), Err(VestingError::ZeroTotal));
        assert_eq!(
            batch_milestone_amounts(100, &[1u16; 65]),
            Err(VestingError::TooManyMilestones)
        );
    }

    #[test]
    fn batch_terms_roundtrip_with_spec_tags() {
        let c = common(Asset::Native, BatchShape::Milestones { bps: vec![10_000] });
        let bytes = borsh::to_vec(&c).unwrap();
        assert_eq!(bytes[0], 0, "asset native");
        assert_eq!(bytes[1], 1, "shape milestones is tag 1");
        assert_eq!(borsh::from_slice::<BatchTerms>(&bytes).unwrap(), c);
        assert_eq!(
            borsh::to_vec(&BatchShape::Linear {
                start: 0,
                cliff: 0,
                end: 1
            })
            .unwrap()[0],
            0
        );
    }
}
