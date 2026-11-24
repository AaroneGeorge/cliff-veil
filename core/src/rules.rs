//! The vesting arithmetic and every state transition (SPEC section 4 and the effects of section 3).
//!
//! Each `apply_*` function either changes the schedule and returns what happened, or returns an error and leaves
//! the schedule exactly as it was. The program writes the changed state back and moves the tokens the outcome
//! names; the SDK runs the same function on a copy to predict the result.
//!
//! How the program uses them, per message:
//!
//! | Message at the schedule | Calls |
//! |---|---|
//! | `ClaimFor { to }` | [`authorise`](ScheduleV1::authorise) (beneficiary), [`check_destination`] (`to`), [`begin`](ScheduleV1::begin) (`Pending::Claim`); then on the clock reply [`finish`](ScheduleV1::finish), [`apply_claim`](ScheduleV1::apply_claim)`(now)` |
//! | `ClaimForAsOf { to, as_of }` | `authorise`, `check_destination`, [`ensure_idle`](ScheduleV1::ensure_idle), `apply_claim(as_of)` |
//! | `CancelFor` | `authorise` (cancel authority), [`check_cancel`](ScheduleV1::check_cancel), `begin` (`Pending::Cancel`); then `finish`, [`apply_cancel`](ScheduleV1::apply_cancel)`(now)` |
//! | `MakeNonCancelableFor` | `authorise`, `ensure_idle`, [`apply_make_non_cancelable`](ScheduleV1::apply_make_non_cancelable) |
//! | `SignalFor { index }` | `authorise`, `ensure_idle`, [`apply_signal_milestone`](ScheduleV1::apply_signal_milestone) |
//! | `TransferFor { new_beneficiary }` | `authorise`, `ensure_idle`, [`apply_transfer_beneficiary`](ScheduleV1::apply_transfer_beneficiary) |
//!
//! # One time source per schedule
//!
//! A public claim and a cancel read the clock, which inside a block reports the previous block's timestamp; a
//! `ClaimAsOf` may be computed at any `as_of` up to the including block's own timestamp. Mixing the two would let a
//! claim run ahead of a later cancel in the same block. So every claim and cancel is computed at the effective time
//! `max(t, last_time)` ([`ScheduleV1::effective_time`]) and stores it in `last_time`: the time a schedule sees never
//! goes backwards, `claimed <= vested(effective_time(t))` holds for every `t`, and a cancel never refunds more than
//! the escrow holds.

use crate::events::{Claimed, MilestoneSignalled};
use crate::math::mul_div_floor;
use crate::message::Role;
use crate::state::{AccountId, Cancellation, Kind, Pending, ScheduleV1};
use crate::{NextUnlock, Result, VestingError};

/// What a claim paid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimOutcome {
    /// Paid out by this claim; greater than zero.
    pub amount: u128,
    /// The effective time the claim was computed at: `max(t, last_time)` for the clock time or `as_of` `t`.
    pub at: u64,
    /// `claimed` after this claim.
    pub claimed_total: u128,
}

impl ClaimOutcome {
    /// The `Claimed` event of this claim by `beneficiary` on the schedule at account `schedule`.
    #[must_use]
    pub const fn event(&self, schedule: AccountId, beneficiary: AccountId) -> Claimed {
        Claimed {
            schedule,
            beneficiary,
            amount: self.amount,
            at: self.at,
            claimed_total: self.claimed_total,
        }
    }
}

/// What a milestone signal unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalOutcome {
    /// The milestone index.
    pub index: u8,
    /// The amount this milestone unlocked.
    pub amount: u128,
    /// The `signalled` bitmap after the signal.
    pub signalled: u64,
}

impl SignalOutcome {
    /// The `MilestoneSignalled` event of this signal on the schedule at account `schedule`.
    #[must_use]
    pub const fn event(&self, schedule: AccountId) -> MilestoneSignalled {
        MilestoneSignalled {
            schedule,
            index: self.index,
            amount: self.amount,
            signalled: self.signalled,
        }
    }
}

/// Checks a payout account against the schedule account: a claim destination `to` or a refund account must not be
/// the schedule's own account, whose escrow it would pay back into (the amount would leave the schedule's books
/// and stay locked in its escrow for good).
///
/// # Errors
/// [`VestingError::PayoutToEscrow`] (26) when `payout == schedule`.
pub fn check_destination(schedule: &AccountId, payout: &AccountId) -> Result<()> {
    if payout == schedule {
        Err(VestingError::PayoutToEscrow)
    } else {
        Ok(())
    }
}

impl ScheduleV1 {
    /// The amount the curve has vested at time `t`, ignoring any cancel (SPEC `vested_raw`).
    ///
    /// Linear: 0 before the cliff, `total` from `end` on, `floor(total * (t - start) / (end - start))` between.
    /// Milestones: the sum of the signalled amounts.
    ///
    /// # Errors
    /// [`VestingError::Overflow`] (29) only for a state that breaks the creation rules (`start >= end`, or milestone
    /// amounts that overflow `u128`); no state built by [`crate::Terms::into_state`] can.
    pub fn vested_raw(&self, t: u64) -> Result<u128> {
        match &self.kind {
            &Kind::Linear { start, cliff, end } => {
                if t < cliff {
                    Ok(0)
                } else if t >= end {
                    Ok(self.total)
                } else {
                    // start <= cliff <= t < end, so neither subtraction wraps; checked anyway for foreign states.
                    let elapsed = t.checked_sub(start).ok_or(VestingError::Overflow)?;
                    let span = end.checked_sub(start).ok_or(VestingError::Overflow)?;
                    mul_div_floor(self.total, elapsed, span).ok_or(VestingError::Overflow)
                }
            }
            Kind::Milestones { amounts, signalled } => amounts
                .iter()
                .enumerate()
                .take(64)
                .filter(|(i, _)| signalled >> i & 1 == 1)
                .try_fold(0u128, |sum, (_, amount)| {
                    sum.checked_add(*amount).ok_or(VestingError::Overflow)
                }),
        }
    }

    /// The vested amount at time `t`: frozen at `vested_at_cancel` once cancelled, else [`Self::vested_raw`].
    /// Non-decreasing in `t`, and at most `total`.
    ///
    /// # Errors
    /// As [`Self::vested_raw`].
    pub fn vested(&self, t: u64) -> Result<u128> {
        match &self.cancelled {
            Some(c) => Ok(c.vested_at_cancel),
            None => self.vested_raw(t),
        }
    }

    /// The time a claim or cancel at time `t` is computed at: `max(t, last_time)`. Never earlier than any claim or
    /// cancel before it.
    #[must_use]
    pub const fn effective_time(&self, t: u64) -> u64 {
        if t > self.last_time { t } else { self.last_time }
    }

    /// What a claim at time `t` would pay: `vested(effective_time(t)) - claimed`, floored at 0.
    ///
    /// The floor is never reached while the invariants hold (`claimed <= vested(last_time)` and `vested` is
    /// non-decreasing); it is kept so no input can make the subtraction wrap. A `ClaimAsOf` with an `as_of` older
    /// than `last_time` is computed at `last_time`, so it pays what is claimable then, never less than 0 and never
    /// more than a claim at `last_time` would.
    ///
    /// # Errors
    /// As [`Self::vested_raw`].
    pub fn claimable(&self, t: u64) -> Result<u128> {
        Ok(self.vested(self.effective_time(t))?.saturating_sub(self.claimed))
    }

    /// When more becomes claimable after time `t`. Computed at `te = effective_time(t)`.
    ///
    /// - Cancelled, or everything vested: [`NextUnlock::Never`].
    /// - Milestones with an unsignalled milestone: [`NextUnlock::WhenMilestoneSignalled`].
    /// - Linear: [`NextUnlock::At`] the smallest `t' > te` with `vested_raw(t') > max(claimed, vested_raw(te))`,
    ///   that is, the first moment the claimable amount grows past what is claimable at `te`. When nothing is
    ///   claimable at `te` (the case of error 14) this is the smallest `t' > te` with `vested_raw(t') > claimed`.
    ///   Found by binary search on the monotone curve over `(te, end]` (at most 64 steps), which is exact for every
    ///   total and span, including a tiny total where the first unit vests after the cliff.
    ///
    /// # Errors
    /// As [`Self::vested_raw`].
    pub fn next_unlock(&self, t: u64) -> Result<NextUnlock> {
        if self.cancelled.is_some() {
            return Ok(NextUnlock::Never);
        }
        let t = self.effective_time(t);
        let base = self.claimed.max(self.vested_raw(t)?);
        if base >= self.total {
            return Ok(NextUnlock::Never);
        }
        match self.kind {
            Kind::Milestones { .. } => Ok(NextUnlock::WhenMilestoneSignalled),
            Kind::Linear { end, .. } => {
                // base < total, so t < end (vested_raw is total from end on), and vested_raw(end) = total > base.
                // Invariant: vested_raw(lo) <= base < vested_raw(hi).
                let (mut lo, mut hi) = (t, end);
                while hi - lo > 1 {
                    let mid = lo + (hi - lo) / 2;
                    if self.vested_raw(mid)? > base {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                Ok(NextUnlock::At(hi))
            }
        }
    }

    /// The account that holds `role` on this schedule.
    #[must_use]
    pub const fn holder(&self, role: Role) -> AccountId {
        match role {
            Role::Beneficiary => self.beneficiary,
            Role::Creator => self.creator,
            Role::CancelAuthority => self.cancel_authority,
            Role::MilestoneAuthority => self.milestone_authority,
        }
    }

    /// Checks that `caller` (the account of the `vesting` actor that sent the message) may act as `role`.
    /// [`Role::Creator`] also admits the cancel authority, as `MakeNonCancelable` does.
    ///
    /// # Errors
    /// The code of the role: [`VestingError::NotBeneficiary`] (3), [`VestingError::NotCreator`] (4),
    /// [`VestingError::NotCancelAuthority`] (5), [`VestingError::NotMilestoneAuthority`] (6).
    pub fn authorise(&self, role: Role, caller: &AccountId) -> Result<()> {
        let allowed = match role {
            Role::Creator => *caller == self.creator || *caller == self.cancel_authority,
            other => *caller == self.holder(other),
        };
        if allowed { Ok(()) } else { Err(role.refusal()) }
    }

    /// Checks that no operation is waiting for the clock.
    ///
    /// # Errors
    /// [`VestingError::OperationPending`] (21).
    pub fn ensure_idle(&self) -> Result<()> {
        if self.pending.is_some() {
            Err(VestingError::OperationPending)
        } else {
            Ok(())
        }
    }

    /// Records `pending` before the program asks the clock for the time.
    ///
    /// # Errors
    /// [`VestingError::OperationPending`] (21) when another operation is already waiting.
    pub fn begin(&mut self, pending: Pending) -> Result<()> {
        self.ensure_idle()?;
        self.pending = Some(pending);
        Ok(())
    }

    /// Takes the waiting operation when the clock replies.
    ///
    /// # Errors
    /// [`VestingError::UnexpectedClockReply`] (22) when nothing is waiting.
    pub fn finish(&mut self) -> Result<Pending> {
        self.pending.take().ok_or(VestingError::UnexpectedClockReply)
    }

    /// Pays everything claimable at time `t` (the clock time, or the `as_of` of a `ClaimAsOf`), computed at
    /// `at = effective_time(t)`, and records `last_time = at`.
    ///
    /// Rounding is down; because the curve reaches `total` exactly at `end`, the last claim pays every unit lost
    /// to rounding.
    ///
    /// # Errors
    /// [`VestingError::NothingClaimable`] (14) with the next unlock when nothing is claimable at `at`;
    /// [`VestingError::Overflow`] (29) as in [`Self::vested_raw`]. A refused claim changes nothing.
    pub fn apply_claim(&mut self, t: u64) -> Result<ClaimOutcome> {
        let at = self.effective_time(t);
        let amount = self.claimable(at)?;
        if amount == 0 {
            return Err(VestingError::NothingClaimable {
                next: self.next_unlock(at)?,
            });
        }
        let claimed_total = self.claimed.checked_add(amount).ok_or(VestingError::Overflow)?;
        self.claimed = claimed_total;
        self.last_time = at;
        Ok(ClaimOutcome {
            amount,
            at,
            claimed_total,
        })
    }

    /// Checks that the schedule may be cancelled now. The program calls it before it asks the clock, so a refused
    /// cancel costs no clock read; [`Self::apply_cancel`] checks again.
    ///
    /// # Errors
    /// [`VestingError::AlreadyCancelled`] (16), then [`VestingError::NotCancelable`] (15).
    pub fn check_cancel(&self) -> Result<()> {
        if self.cancelled.is_some() {
            return Err(VestingError::AlreadyCancelled);
        }
        if !self.cancelable {
            return Err(VestingError::NotCancelable);
        }
        Ok(())
    }

    /// Cancels at clock time `now`, computed at `at = effective_time(now)`: freezes the vested amount, records
    /// `last_time = at` and returns what to refund.
    ///
    /// `vested_at_cancel = max(vested_raw(at), claimed)` and `refunded = total - vested_at_cancel`. The escrow then
    /// holds `vested_at_cancel - claimed`, all of it still claimable by the beneficiary. With the effective time,
    /// `vested_raw(at) >= claimed` already holds (a `ClaimAsOf` earlier in the same block moved `last_time` forward);
    /// the `max` is a second guard so the refund can never exceed what the escrow holds. A `refunded` of 0 is a
    /// valid cancel: the program then makes no transfer.
    ///
    /// # Errors
    /// [`VestingError::AlreadyCancelled`] (16), [`VestingError::NotCancelable`] (15),
    /// [`VestingError::Overflow`] (29) as in [`Self::vested_raw`]. A refused cancel changes nothing.
    pub fn apply_cancel(&mut self, now: u64) -> Result<Cancellation> {
        self.check_cancel()?;
        let at = self.effective_time(now);
        let vested_at_cancel = self.vested_raw(at)?.max(self.claimed);
        let refunded = self.total.checked_sub(vested_at_cancel).ok_or(VestingError::Overflow)?;
        let cancellation = Cancellation {
            at,
            vested_at_cancel,
            refunded,
        };
        self.cancelled = Some(cancellation);
        self.last_time = at;
        Ok(cancellation)
    }

    /// Makes the schedule non-cancelable, for good.
    ///
    /// # Errors
    /// [`VestingError::AlreadyCancelled`] (16), then [`VestingError::NotCancelable`] (15) when it already is
    /// non-cancelable.
    pub fn apply_make_non_cancelable(&mut self) -> Result<()> {
        self.check_cancel()?;
        self.cancelable = false;
        Ok(())
    }

    /// Signals milestone `index`, unlocking its amount. Moves no tokens.
    ///
    /// A second signal of the same index is refused and changes nothing, so a milestone can never unlock twice.
    ///
    /// # Errors
    /// Checked in this order: [`VestingError::NotMilestoneSchedule`] (20), [`VestingError::AlreadyCancelled`] (16),
    /// [`VestingError::MilestoneOutOfRange`] (17), [`VestingError::MilestoneAlreadySignalled`] (18).
    pub fn apply_signal_milestone(&mut self, index: u8) -> Result<SignalOutcome> {
        let Kind::Milestones { amounts, signalled } = &mut self.kind else {
            return Err(VestingError::NotMilestoneSchedule);
        };
        if self.cancelled.is_some() {
            return Err(VestingError::AlreadyCancelled);
        }
        let amount = *amounts
            .get(usize::from(index))
            .ok_or(VestingError::MilestoneOutOfRange)?;
        // amounts has at most 64 entries in a valid state; a longer foreign state cannot address bit 64+.
        let bit = 1u64
            .checked_shl(u32::from(index))
            .ok_or(VestingError::MilestoneOutOfRange)?;
        if *signalled & bit != 0 {
            return Err(VestingError::MilestoneAlreadySignalled);
        }
        *signalled |= bit;
        Ok(SignalOutcome {
            index,
            amount,
            signalled: *signalled,
        })
    }

    /// Hands the schedule to `new_beneficiary`. Returns the previous beneficiary. Whatever is vested and unclaimed
    /// goes with it.
    ///
    /// # Errors
    /// [`VestingError::NotTransferable`] (19).
    pub fn apply_transfer_beneficiary(&mut self, new_beneficiary: AccountId) -> Result<AccountId> {
        if !self.transferable {
            return Err(VestingError::NotTransferable);
        }
        Ok(core::mem::replace(&mut self.beneficiary, new_beneficiary))
    }

    /// `true` once everything that will ever vest has been paid out (after a cancel: `vested_at_cancel`).
    #[must_use]
    pub fn is_settled(&self) -> bool {
        let ceiling = self.cancelled.map_or(self.total, |c| c.vested_at_cancel);
        self.claimed >= ceiling
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::state::Asset;

    const CREATOR: AccountId = AccountId::new([1; 32]);
    const BENEFICIARY: AccountId = AccountId::new([2; 32]);
    const CANCELLER: AccountId = AccountId::new([3; 32]);
    const SIGNALLER: AccountId = AccountId::new([4; 32]);
    const OTHER: AccountId = AccountId::new([9; 32]);

    fn schedule(total: u128, kind: Kind) -> ScheduleV1 {
        ScheduleV1 {
            schedule_id: [7; 32],
            creator: CREATOR,
            beneficiary: BENEFICIARY,
            asset: Asset::Native,
            total,
            claimed: 0,
            last_time: 0,
            kind,
            cancelable: true,
            transferable: false,
            cancel_authority: CANCELLER,
            milestone_authority: SIGNALLER,
            refund_to: CREATOR,
            cancelled: None,
            pending: None,
        }
    }

    fn linear(total: u128, start: u64, cliff: u64, end: u64) -> ScheduleV1 {
        schedule(total, Kind::Linear { start, cliff, end })
    }

    fn milestones(amounts: &[u128]) -> ScheduleV1 {
        schedule(
            amounts.iter().sum(),
            Kind::Milestones {
                amounts: amounts.to_vec(),
                signalled: 0,
            },
        )
    }

    // ---- F1a: cliff then linear ----

    #[test]
    fn f1a_nothing_vests_before_the_cliff() {
        let s = linear(1_200, 1_000, 4_000, 13_000);
        assert_eq!(s.vested(0).unwrap(), 0);
        assert_eq!(s.vested(3_999).unwrap(), 0);
        assert_eq!(s.claimable(3_999).unwrap(), 0);
    }

    #[test]
    fn f1a_cliff_then_linear() {
        // 1200 over 12 s from t=1000, cliff at 3 s: 300 unlock at the cliff, then 100 per second.
        let s = linear(1_200, 1_000, 4_000, 13_000);
        assert_eq!(s.vested(4_000).unwrap(), 300);
        assert_eq!(s.vested(5_000).unwrap(), 400);
        assert_eq!(s.vested(5_009).unwrap(), 400, "floor rounding: 400.9 -> 400");
        assert_eq!(s.vested(5_010).unwrap(), 401);
        assert_eq!(s.vested(12_999).unwrap(), 1_199);
        assert_eq!(s.vested(13_000).unwrap(), 1_200);
        assert_eq!(s.vested(u64::MAX).unwrap(), 1_200);
    }

    #[test]
    fn f1a_pure_cliff_pays_everything_at_end() {
        let s = linear(500, 0, 10, 10);
        assert_eq!(s.vested(9).unwrap(), 0);
        assert_eq!(s.vested(10).unwrap(), 500);
    }

    #[test]
    fn f1a_last_claim_pays_the_rounding_remainder() {
        // 10 over 3 ms: every intermediate claim rounds down.
        let mut s = linear(10, 0, 0, 3);
        assert_eq!(s.apply_claim(1).unwrap().amount, 3);
        assert_eq!(s.apply_claim(2).unwrap().amount, 3);
        let last = s.apply_claim(3).unwrap();
        assert_eq!(last.amount, 4, "the remainder lost to rounding is paid at the end");
        assert_eq!(last.claimed_total, 10);
        assert!(s.is_settled());
    }

    #[test]
    fn f1a_claim_records_time_and_running_total() {
        let mut s = linear(1_200, 1_000, 4_000, 13_000);
        let first = s.apply_claim(5_000).unwrap();
        assert_eq!(
            first,
            ClaimOutcome {
                amount: 400,
                at: 5_000,
                claimed_total: 400
            }
        );
        let second = s.apply_claim(6_000).unwrap();
        assert_eq!(
            second,
            ClaimOutcome {
                amount: 100,
                at: 6_000,
                claimed_total: 500
            }
        );
        assert_eq!((s.claimed, s.last_time), (500, 6_000));
        let event = second.event(OTHER, BENEFICIARY);
        assert_eq!(
            (
                event.schedule,
                event.beneficiary,
                event.amount,
                event.at,
                event.claimed_total
            ),
            (OTHER, BENEFICIARY, 100, 6_000, 500)
        );
    }

    #[test]
    fn f1a_max_total_and_max_times_do_not_overflow() {
        let s = linear(u128::MAX, 0, 0, u64::MAX);
        assert_eq!(
            s.vested(u64::MAX - 1).unwrap(),
            u128::MAX - (u128::MAX / u128::from(u64::MAX))
        );
        assert_eq!(s.vested(u64::MAX).unwrap(), u128::MAX);
    }

    // ---- F1b: fully linear ----

    #[test]
    fn f1b_fully_linear_starts_accruing_at_start() {
        let s = linear(1_000, 100, 100, 1_100);
        assert_eq!(s.vested(99).unwrap(), 0);
        assert_eq!(s.vested(100).unwrap(), 0);
        assert_eq!(s.vested(101).unwrap(), 1);
        assert_eq!(s.vested(600).unwrap(), 500);
        assert_eq!(s.vested(1_100).unwrap(), 1_000);
    }

    // ---- F1c: milestones ----

    #[test]
    fn f1c_milestones_unlock_their_amounts_in_any_order() {
        let mut s = milestones(&[100, 200, 700]);
        assert_eq!(s.vested(u64::MAX).unwrap(), 0);
        let out = s.apply_signal_milestone(2).unwrap();
        assert_eq!(
            out,
            SignalOutcome {
                index: 2,
                amount: 700,
                signalled: 0b100
            }
        );
        assert_eq!(s.vested(0).unwrap(), 700, "time does not matter for milestones");
        s.apply_signal_milestone(0).unwrap();
        assert_eq!(s.vested(0).unwrap(), 800);
        assert_eq!(s.apply_claim(0).unwrap().amount, 800);
        s.apply_signal_milestone(1).unwrap();
        assert_eq!(s.apply_claim(0).unwrap().amount, 200);
        assert!(s.is_settled());
    }

    #[test]
    fn f1c_sixty_four_milestones_use_every_bit() {
        let amounts: Vec<u128> = (1..=64).collect();
        let mut s = milestones(&amounts);
        for i in 0..64u8 {
            s.apply_signal_milestone(i).unwrap();
        }
        assert_eq!(s.vested(0).unwrap(), s.total);
        assert_eq!(
            s.kind,
            Kind::Milestones {
                amounts,
                signalled: u64::MAX
            }
        );
    }

    #[test]
    fn f6_signal_outcome_builds_its_event() {
        let mut s = milestones(&[5, 6]);
        let event = s.apply_signal_milestone(1).unwrap().event(OTHER);
        assert_eq!(
            (event.schedule, event.index, event.amount, event.signalled),
            (OTHER, 1, 6, 0b10)
        );
    }

    // ---- R4: milestone signal is idempotent per index ----

    #[test]
    fn r4_milestone_signal_idempotent() {
        let mut s = milestones(&[100, 200]);
        s.apply_signal_milestone(1).unwrap();
        let before = s.clone();
        assert_eq!(
            s.apply_signal_milestone(1),
            Err(VestingError::MilestoneAlreadySignalled)
        );
        assert_eq!(s, before, "a refused signal changes nothing");
        assert_eq!(s.vested(0).unwrap(), 200, "the amount unlocked once");
        assert_eq!(VestingError::MilestoneAlreadySignalled.code(), 18);
    }

    #[test]
    fn r4_signal_out_of_range_is_17() {
        let mut s = milestones(&[100, 200]);
        assert_eq!(s.apply_signal_milestone(2), Err(VestingError::MilestoneOutOfRange));
        assert_eq!(s.apply_signal_milestone(255), Err(VestingError::MilestoneOutOfRange));
        assert_eq!(VestingError::MilestoneOutOfRange.code(), 17);
    }

    #[test]
    fn r4_signal_on_linear_schedule_is_20() {
        let mut s = linear(10, 0, 0, 10);
        assert_eq!(s.apply_signal_milestone(0), Err(VestingError::NotMilestoneSchedule));
        assert_eq!(VestingError::NotMilestoneSchedule.code(), 20);
    }

    #[test]
    fn r4_signal_after_cancel_is_16() {
        let mut s = milestones(&[100, 200]);
        s.apply_cancel(0).unwrap();
        assert_eq!(s.apply_signal_milestone(0), Err(VestingError::AlreadyCancelled));
    }

    #[test]
    fn r4_signal_error_order_is_fixed() {
        // A cancelled linear schedule: not a milestone schedule comes first.
        let mut s = linear(10, 0, 0, 10);
        s.apply_cancel(5).unwrap();
        assert_eq!(s.apply_signal_milestone(9), Err(VestingError::NotMilestoneSchedule));
        // A cancelled milestone schedule with an out-of-range index: cancelled comes first.
        let mut m = milestones(&[1]);
        m.apply_cancel(0).unwrap();
        assert_eq!(m.apply_signal_milestone(9), Err(VestingError::AlreadyCancelled));
    }

    // ---- U7: nothing claimable, with the next unlock ----

    #[test]
    fn u7_nothing_claimable_before_cliff_names_the_cliff() {
        let mut s = linear(1_200, 1_000, 4_000, 13_000);
        let before = s.clone();
        assert_eq!(
            s.apply_claim(3_000),
            Err(VestingError::NothingClaimable {
                next: NextUnlock::At(4_000)
            })
        );
        assert_eq!(s, before);
    }

    #[test]
    fn u7_nothing_claimable_between_units_names_the_next_unit() {
        let mut s = linear(1_200, 1_000, 4_000, 13_000);
        s.apply_claim(5_000).unwrap(); // claimed 400
        // 401 vests at t = 1000 + ceil(401 * 12000 / 1200) = 5010.
        assert_eq!(
            s.apply_claim(5_005),
            Err(VestingError::NothingClaimable {
                next: NextUnlock::At(5_010)
            })
        );
        assert_eq!(s.next_unlock(5_009).unwrap(), NextUnlock::At(5_010));
    }

    #[test]
    fn u7_next_unlock_of_tiny_fully_linear_total_is_after_start() {
        // 2 units over 1000 ms from 0: the first unit vests at 500, not at the cliff (= start).
        let s = linear(2, 0, 0, 1_000);
        assert_eq!(s.next_unlock(0).unwrap(), NextUnlock::At(500));
        let before_start = linear(2, 100, 100, 1_100);
        assert_eq!(before_start.next_unlock(0).unwrap(), NextUnlock::At(600));
    }

    #[test]
    fn u7_next_unlock_when_something_is_claimable_is_the_next_increase() {
        let s = linear(1_200, 1_000, 4_000, 13_000);
        // Unclaimed 400 at 5000; the next increase is to 401 at 5010.
        assert_eq!(s.next_unlock(5_000).unwrap(), NextUnlock::At(5_010));
    }

    #[test]
    fn u7_nothing_claimable_on_milestones_waits_for_a_signal() {
        let mut s = milestones(&[100, 200]);
        assert_eq!(
            s.apply_claim(0),
            Err(VestingError::NothingClaimable {
                next: NextUnlock::WhenMilestoneSignalled
            })
        );
    }

    #[test]
    fn u7_nothing_left_after_everything_is_claimed() {
        let mut s = linear(10, 0, 0, 10);
        s.apply_claim(10).unwrap();
        assert_eq!(
            s.apply_claim(20),
            Err(VestingError::NothingClaimable {
                next: NextUnlock::Never
            })
        );
        let mut m = milestones(&[3]);
        m.apply_signal_milestone(0).unwrap();
        m.apply_claim(0).unwrap();
        assert_eq!(
            m.apply_claim(0),
            Err(VestingError::NothingClaimable {
                next: NextUnlock::Never
            })
        );
    }

    #[test]
    fn u7_nothing_left_after_cancel_once_vested_part_is_claimed() {
        let mut s = linear(100, 0, 0, 100);
        s.apply_cancel(30).unwrap();
        assert_eq!(s.apply_claim(50).unwrap().amount, 30);
        assert_eq!(
            s.apply_claim(90),
            Err(VestingError::NothingClaimable {
                next: NextUnlock::Never
            })
        );
    }

    // ---- ClaimAsOf ----

    #[test]
    fn f2_claim_as_of_an_earlier_time_is_computed_at_the_last_time() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.apply_claim(600).unwrap();
        // An as_of earlier than the last claim time is computed at 600: nothing more, no underflow.
        assert_eq!(s.effective_time(300), 600);
        assert_eq!(s.claimable(300).unwrap(), 0);
        let before = s.clone();
        assert_eq!(
            s.apply_claim(300),
            Err(VestingError::NothingClaimable {
                next: NextUnlock::At(601)
            })
        );
        assert_eq!(s, before, "a refused claim leaves last_time as it was");
        assert_eq!(s.next_unlock(0).unwrap(), NextUnlock::At(601));
    }

    #[test]
    fn f2_claim_as_of_then_cancel_in_the_same_block() {
        // Inside block n the clock reports block n-1's timestamp (10_000); a ClaimAsOf in block n may use as_of up
        // to block n's own timestamp (11_000), one block ahead of the clock.
        let (clock, block_time) = (10_000, 11_000);
        let mut s = linear(100_000, 0, 0, 100_000);
        let claim = s.apply_claim(block_time).unwrap();
        assert_eq!((claim.amount, claim.at), (11_000, block_time));
        assert_eq!(s.last_time, block_time);

        // Cancel later in the same block reads the older clock time, and is computed at 11_000.
        let c = s.apply_cancel(clock).unwrap();
        assert_eq!(
            c,
            Cancellation {
                at: block_time,
                vested_at_cancel: 11_000,
                refunded: 89_000
            }
        );
        let left_in_escrow = s.total - s.claimed - c.refunded;
        assert_eq!(left_in_escrow, 0);
        assert_eq!(s.claimed + c.refunded + left_in_escrow, s.total);
        assert!(s.is_settled());
        assert_eq!(
            s.apply_claim(clock),
            Err(VestingError::NothingClaimable {
                next: NextUnlock::Never
            })
        );
    }

    #[test]
    fn f2_claim_after_cancel_reports_the_cancel_time() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.apply_cancel(800).unwrap();
        let late = s.apply_claim(100).unwrap();
        assert_eq!(
            (late.amount, late.at),
            (800, 800),
            "an older as_of is reported at the effective time"
        );
        assert_eq!(s.last_time, 800);
    }

    #[test]
    fn payout_to_the_schedule_account_is_26() {
        check_destination(&OTHER, &BENEFICIARY).unwrap();
        assert_eq!(check_destination(&OTHER, &OTHER), Err(VestingError::PayoutToEscrow));
        assert_eq!(VestingError::PayoutToEscrow.code(), 26);
    }

    // ---- F3: cancel ----

    #[test]
    fn f3_cancel_returns_unvested() {
        let mut s = linear(1_000, 0, 0, 1_000);
        let c = s.apply_cancel(250).unwrap();
        assert_eq!(
            c,
            Cancellation {
                at: 250,
                vested_at_cancel: 250,
                refunded: 750
            }
        );
        assert_eq!(s.cancelled, Some(c));
    }

    #[test]
    fn f3_claim_after_cancel_pays_vested_unclaimed() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.apply_claim(100).unwrap();
        let c = s.apply_cancel(400).unwrap();
        assert_eq!(c.refunded, 600);
        assert_eq!(s.claimable(400).unwrap(), 300);
        assert_eq!(s.claimable(u64::MAX).unwrap(), 300, "accrual is frozen at the cancel");
        assert_eq!(s.apply_claim(900).unwrap().amount, 300);
        assert_eq!(s.claimed + c.refunded, s.total);
    }

    #[test]
    fn f3_cancel_before_cliff_refunds_everything() {
        let mut s = linear(1_000, 0, 500, 1_000);
        assert_eq!(s.apply_cancel(499).unwrap().refunded, 1_000);
        assert_eq!(
            s.apply_claim(1_000),
            Err(VestingError::NothingClaimable {
                next: NextUnlock::Never
            })
        );
    }

    #[test]
    fn f3_cancel_after_end_refunds_nothing() {
        let mut s = linear(1_000, 0, 0, 1_000);
        assert_eq!(
            s.apply_cancel(5_000).unwrap(),
            Cancellation {
                at: 5_000,
                vested_at_cancel: 1_000,
                refunded: 0
            }
        );
        assert_eq!(s.claimable(0).unwrap(), 1_000);
    }

    #[test]
    fn f3_cancel_milestones_refunds_unsignalled() {
        let mut s = milestones(&[100, 200, 300]);
        s.apply_signal_milestone(1).unwrap();
        let c = s.apply_cancel(0).unwrap();
        assert_eq!((c.vested_at_cancel, c.refunded), (200, 400));
    }

    #[test]
    fn f3_cancel_twice_is_16() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.apply_cancel(1).unwrap();
        let before = s.clone();
        assert_eq!(s.apply_cancel(2), Err(VestingError::AlreadyCancelled));
        assert_eq!(s, before);
        assert_eq!(VestingError::AlreadyCancelled.code(), 16);
    }

    #[test]
    fn f3_cancel_non_cancelable_is_15() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.cancelable = false;
        assert_eq!(s.check_cancel(), Err(VestingError::NotCancelable));
        assert_eq!(s.apply_cancel(1), Err(VestingError::NotCancelable));
        assert_eq!(s.cancelled, None);
        assert_eq!(VestingError::NotCancelable.code(), 15);
    }

    #[test]
    fn f3_cancel_never_refunds_claimed_tokens() {
        // A claim computed at 800 followed by a cancel at clock time 500: the cancel is computed at 800.
        let mut s = linear(1_000, 0, 0, 1_000);
        s.apply_claim(800).unwrap();
        let c = s.apply_cancel(500).unwrap();
        assert_eq!((c.at, c.vested_at_cancel, c.refunded), (800, 800, 200));
        assert_eq!(s.claimed + c.refunded, s.total);
    }

    #[test]
    fn f3_cancel_with_nothing_unvested_refunds_zero() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.apply_claim(1_000).unwrap();
        let c = s.apply_cancel(2_000).unwrap();
        assert_eq!(
            (c.vested_at_cancel, c.refunded),
            (1_000, 0),
            "allowed; the program makes no transfer"
        );
    }

    #[test]
    fn f3_non_cancelable_is_one_way() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.apply_make_non_cancelable().unwrap();
        assert!(!s.cancelable);
        assert_eq!(s.apply_make_non_cancelable(), Err(VestingError::NotCancelable));
        assert_eq!(s.apply_cancel(10), Err(VestingError::NotCancelable));
        assert!(!s.cancelable, "no operation turns it back");
    }

    #[test]
    fn f3_make_non_cancelable_after_cancel_is_16() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.apply_cancel(10).unwrap();
        assert_eq!(s.apply_make_non_cancelable(), Err(VestingError::AlreadyCancelled));
        assert!(s.cancelable);
    }

    // ---- F4: transfer ----

    #[test]
    fn f4_transfer_by_holder_only() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.transferable = true;
        assert_eq!(
            s.authorise(Role::Beneficiary, &OTHER),
            Err(VestingError::NotBeneficiary)
        );
        s.authorise(Role::Beneficiary, &BENEFICIARY).unwrap();
        assert_eq!(s.apply_transfer_beneficiary(OTHER), Ok(BENEFICIARY));
        assert_eq!(s.beneficiary, OTHER);
        assert_eq!(
            s.authorise(Role::Beneficiary, &BENEFICIARY),
            Err(VestingError::NotBeneficiary)
        );
        s.authorise(Role::Beneficiary, &OTHER).unwrap();
    }

    #[test]
    fn f4_not_transferable_is_19() {
        let mut s = linear(1_000, 0, 0, 1_000);
        assert_eq!(s.apply_transfer_beneficiary(OTHER), Err(VestingError::NotTransferable));
        assert_eq!(s.beneficiary, BENEFICIARY);
        assert_eq!(VestingError::NotTransferable.code(), 19);
    }

    #[test]
    fn f4_transfer_moves_vested_unclaimed_with_it() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.transferable = true;
        s.apply_claim(300).unwrap();
        s.apply_transfer_beneficiary(OTHER).unwrap();
        assert_eq!(
            s.claimable(500).unwrap(),
            200,
            "the new beneficiary claims from where the old one stopped"
        );
        assert!(s.transferable, "the flag itself never changes");
    }

    // ---- Roles (soft requirements: separate authorities) ----

    #[test]
    fn soft_cancel_authority_is_separate_from_creator() {
        let s = linear(1_000, 0, 0, 1_000);
        s.authorise(Role::CancelAuthority, &CANCELLER).unwrap();
        assert_eq!(
            s.authorise(Role::CancelAuthority, &CREATOR),
            Err(VestingError::NotCancelAuthority)
        );
        assert_eq!(
            s.authorise(Role::CancelAuthority, &BENEFICIARY),
            Err(VestingError::NotCancelAuthority)
        );
        assert_eq!(VestingError::NotCancelAuthority.code(), 5);
    }

    #[test]
    fn soft_milestone_authority_is_separate_from_creator() {
        let s = milestones(&[1]);
        s.authorise(Role::MilestoneAuthority, &SIGNALLER).unwrap();
        assert_eq!(
            s.authorise(Role::MilestoneAuthority, &CREATOR),
            Err(VestingError::NotMilestoneAuthority)
        );
        assert_eq!(VestingError::NotMilestoneAuthority.code(), 6);
    }

    #[test]
    fn f3_make_non_cancelable_by_creator_or_cancel_authority() {
        let s = linear(1_000, 0, 0, 1_000);
        s.authorise(Role::Creator, &CREATOR).unwrap();
        s.authorise(Role::Creator, &CANCELLER).unwrap();
        assert_eq!(s.authorise(Role::Creator, &BENEFICIARY), Err(VestingError::NotCreator));
        assert_eq!(VestingError::NotCreator.code(), 4);
    }

    #[test]
    fn holder_names_each_role() {
        let s = linear(1_000, 0, 0, 1_000);
        assert_eq!(s.holder(Role::Beneficiary), BENEFICIARY);
        assert_eq!(s.holder(Role::Creator), CREATOR);
        assert_eq!(s.holder(Role::CancelAuthority), CANCELLER);
        assert_eq!(s.holder(Role::MilestoneAuthority), SIGNALLER);
    }

    // ---- Pending (clock round trip) ----

    #[test]
    fn r1_second_operation_while_pending_is_21() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.begin(Pending::Claim { to: OTHER }).unwrap();
        assert_eq!(s.begin(Pending::Cancel), Err(VestingError::OperationPending));
        assert_eq!(s.ensure_idle(), Err(VestingError::OperationPending));
        assert_eq!(s.pending, Some(Pending::Claim { to: OTHER }));
        assert_eq!(VestingError::OperationPending.code(), 21);
    }

    #[test]
    fn r1_clock_reply_takes_the_pending_operation_once() {
        let mut s = linear(1_000, 0, 0, 1_000);
        s.begin(Pending::Cancel).unwrap();
        assert_eq!(s.finish(), Ok(Pending::Cancel));
        assert_eq!(s.pending, None);
        assert_eq!(s.finish(), Err(VestingError::UnexpectedClockReply));
        assert_eq!(VestingError::UnexpectedClockReply.code(), 22);
    }

    #[test]
    fn r1_refused_claim_changes_nothing_and_can_be_retried() {
        let mut s = linear(1_000, 0, 500, 1_000);
        let before = s.clone();
        assert!(s.apply_claim(100).is_err());
        assert_eq!(s, before);
        assert_eq!(s.apply_claim(600).unwrap().amount, 600);
    }

    // ---- Foreign states never panic ----

    #[test]
    fn broken_linear_state_is_an_overflow_not_a_panic() {
        // end < start breaks the creation rules; the arithmetic still refuses cleanly.
        let s = linear(10, 50, 0, 40);
        assert_eq!(s.vested(10), Err(VestingError::Overflow));
    }

    #[test]
    fn broken_milestone_sum_is_an_overflow_not_a_panic() {
        let s = schedule(
            1,
            Kind::Milestones {
                amounts: vec![u128::MAX, 1],
                signalled: 0b11,
            },
        );
        assert_eq!(s.vested(0), Err(VestingError::Overflow));
        assert_eq!(VestingError::Overflow.code(), 29);
    }
}
