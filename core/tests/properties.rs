//! Property tests of the vesting rules (SPEC section 4).
//!
//! Each property runs on random schedules: tiny totals and spans (where rounding bites), and amounts and times up to
//! the type limits (where overflow would bite).

use proptest::collection::vec;
use proptest::prelude::*;
use vesting_core::{
    AccountId, Asset, Internal, Kind, Message, NextUnlock, Pending, ScheduleState, ScheduleV1, VestingError,
    batch_milestone_amounts, fee_for, mul_div_floor,
};

const CREATOR: AccountId = AccountId::new([1; 32]);
const BENEFICIARY: AccountId = AccountId::new([2; 32]);

fn schedule(total: u128, kind: Kind) -> ScheduleV1 {
    ScheduleV1 {
        schedule_id: [0; 32],
        creator: CREATOR,
        beneficiary: BENEFICIARY,
        asset: Asset::Native,
        total,
        claimed: 0,
        last_time: 0,
        kind,
        cancelable: true,
        transferable: true,
        cancel_authority: CREATOR,
        milestone_authority: CREATOR,
        refund_to: CREATOR,
        cancelled: None,
        pending: None,
    }
}

/// A valid linear schedule: small or huge totals, short or long spans.
fn linear() -> impl Strategy<Value = ScheduleV1> {
    let total = prop_oneof![1u128..=1_000, 1u128..=u128::MAX];
    let times = prop_oneof![
        (0u64..1_000, 1u64..1_000).prop_flat_map(|(s, span)| (Just(s), 0..=span, Just(span))),
        (0u64..u64::MAX / 2, 1u64..u64::MAX / 2).prop_flat_map(|(s, span)| (Just(s), 0..=span, Just(span))),
    ];
    (total, times).prop_map(|(total, (start, cliff_off, span))| {
        schedule(
            total,
            Kind::Linear {
                start,
                cliff: start + cliff_off,
                end: start + span,
            },
        )
    })
}

/// A valid milestone schedule: 1..=64 non-zero amounts that cannot overflow when summed.
fn milestones() -> impl Strategy<Value = ScheduleV1> {
    vec(prop_oneof![1u128..=1_000, 1u128..=u128::MAX / 64], 1..=64).prop_map(|amounts| {
        let total = amounts.iter().sum();
        schedule(total, Kind::Milestones { amounts, signalled: 0 })
    })
}

fn any_schedule() -> impl Strategy<Value = ScheduleV1> {
    prop_oneof![linear(), milestones()]
}

/// Times spread over the interesting range of a schedule: around start, cliff and end, and anywhere.
fn time_for(s: &ScheduleV1) -> BoxedStrategy<u64> {
    match s.kind {
        Kind::Linear { start, end, .. } => {
            prop_oneof![start.saturating_sub(5)..=end.saturating_add(5), any::<u64>()].boxed()
        }
        Kind::Milestones { .. } => any::<u64>().boxed(),
    }
}

fn milestone_count(s: &ScheduleV1) -> u8 {
    match &s.kind {
        Kind::Milestones { amounts, .. } => u8::try_from(amounts.len()).unwrap(),
        Kind::Linear { .. } => 0,
    }
}

/// Claims at `t`; "nothing claimable" counts as a zero payout, any other error fails the test.
fn claim(s: &mut ScheduleV1, t: u64) -> u128 {
    match s.apply_claim(t) {
        Ok(out) => out.amount,
        Err(VestingError::NothingClaimable { .. }) => 0,
        Err(e) => panic!("unexpected {e}"),
    }
}

fn signal_all(s: &mut ScheduleV1) {
    for i in 0..milestone_count(s) {
        let _ = s.apply_signal_milestone(i);
    }
}

/// The point at which everything has vested: `end` for linear (after signalling nothing), or after signalling all.
fn finish(s: &mut ScheduleV1) -> u64 {
    signal_all(s);
    match s.kind {
        Kind::Linear { end, .. } => end,
        Kind::Milestones { .. } => 0,
    }
}

/// `a * b` as a 256-bit (high, low) pair.
fn mul_256(a: u128, b: u128) -> (u128, u128) {
    let (a1, a0) = (a >> 64, a & u128::from(u64::MAX));
    let (b1, b0) = (b >> 64, b & u128::from(u64::MAX));
    let (p00, p01, p10, p11) = (a0 * b0, a0 * b1, a1 * b0, a1 * b1);
    let mid = (p00 >> 64) + (p01 & u128::from(u64::MAX)) + (p10 & u128::from(u64::MAX));
    let low = (p00 & u128::from(u64::MAX)) | (mid << 64);
    let high = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);
    (high, low)
}

fn add_256((h, l): (u128, u128), x: u128) -> (u128, u128) {
    let (low, carry) = l.overflowing_add(x);
    (h + u128::from(carry), low)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// `mul_div_floor` is exact: q * c <= a * b < (q + 1) * c.
    #[test]
    fn mul_div_floor_is_exact_floor(a in any::<u128>(), c in 1u64.., b_frac in any::<u64>()) {
        // Any b in 0..=c.
        let b = u64::try_from(u128::from(b_frac) % (u128::from(c) + 1)).unwrap();
        let q = mul_div_floor(a, b, c).expect("b <= c never overflows");
        prop_assert!(q <= a);
        let ab = mul_256(a, u128::from(b));
        let qc = mul_256(q, u128::from(c));
        prop_assert!(qc <= ab);
        prop_assert!(add_256(qc, u128::from(c)) > ab);
    }

    /// F1a/F1b: vested is non-decreasing in t, never exceeds total, is 0 before the cliff and total from end.
    #[test]
    fn prop_f1a_vested_monotone_and_bounded(s in linear(), t1 in any::<u64>(), t2 in any::<u64>()) {
        let (lo, hi) = (t1.min(t2), t1.max(t2));
        let (v_lo, v_hi) = (s.vested(lo).unwrap(), s.vested(hi).unwrap());
        prop_assert!(v_lo <= v_hi);
        prop_assert!(v_hi <= s.total);
        let Kind::Linear { cliff, end, .. } = s.kind else { unreachable!() };
        if lo < cliff { prop_assert_eq!(v_lo, 0); }
        if hi >= end { prop_assert_eq!(v_hi, s.total); }
    }

    /// F1a/F1c: claims at any sequence of times pay exactly `vested - claimed` each, and sum to the total by the end.
    #[test]
    fn prop_f1a_claims_sum_to_total(
        (s, times) in any_schedule().prop_flat_map(|s| { let t = vec(time_for(&s), 0..20); (Just(s), t) }),
        signals in vec(any::<u8>(), 0..70),
    ) {
        let mut s = s;
        let mut times = times;
        times.sort_unstable();
        let mut paid = 0u128;
        for (i, t) in times.iter().enumerate() {
            if let Some(index) = signals.get(i) { let _ = s.apply_signal_milestone(*index % 65); }
            let expected = s.vested(*t).unwrap() - s.claimed;
            let amount = claim(&mut s, *t);
            prop_assert_eq!(amount, expected);
            paid += amount;
            prop_assert!(s.claimed <= s.total);
        }
        let end = finish(&mut s);
        paid += claim(&mut s, times.last().copied().unwrap_or(0).max(end));
        prop_assert_eq!(paid, s.total);
        prop_assert_eq!(s.claimed, s.total);
        prop_assert!(s.is_settled());
    }

    /// F3: cancel conserves value: refund + vested_at_cancel == total, claimed never exceeds vested, and the
    /// beneficiary's payouts plus the refund are exactly the total.
    #[test]
    fn prop_f3_cancel_conserves_value(
        (s, before, c, after) in any_schedule().prop_flat_map(|s| {
            let t = time_for(&s);
            (Just(s), vec(t.clone(), 0..10), t.clone(), vec(t, 0..10))
        }),
        signals in vec(any::<u8>(), 0..10),
    ) {
        let mut s = s;
        let mut paid = 0u128;
        let mut before = before;
        before.sort_unstable();
        for (i, t) in before.iter().filter(|t| **t <= c).enumerate() {
            if let Some(index) = signals.get(i) { let _ = s.apply_signal_milestone(*index % 65); }
            paid += claim(&mut s, *t);
        }
        let cancellation = s.apply_cancel(c).unwrap();
        prop_assert_eq!(cancellation.vested_at_cancel + cancellation.refunded, s.total);
        prop_assert!(s.claimed <= cancellation.vested_at_cancel);
        prop_assert_eq!(cancellation.vested_at_cancel, s.vested_raw(c).unwrap(), "monotone clock: no guard needed");
        for t in after {
            prop_assert!(s.apply_signal_milestone(0).is_err(), "no signal after cancel");
            paid += claim(&mut s, t);
            prop_assert!(s.claimed <= cancellation.vested_at_cancel);
        }
        paid += claim(&mut s, u64::MAX);
        prop_assert_eq!(paid + cancellation.refunded, s.total);
        prop_assert!(s.is_settled());
    }

    /// F3: conservation holds even for claims computed at times later than the cancel (the guard case).
    #[test]
    fn prop_f3_cancel_conserves_value_for_any_time_order(
        (s, claims, c) in linear().prop_flat_map(|s| { let t = time_for(&s); (Just(s), vec(t.clone(), 0..10), t) }),
    ) {
        let mut s = s;
        let mut paid = 0u128;
        for t in claims { paid += claim(&mut s, t); }
        let cancellation = s.apply_cancel(c).unwrap();
        prop_assert!(s.claimed <= cancellation.vested_at_cancel);
        prop_assert_eq!(cancellation.vested_at_cancel + cancellation.refunded, s.total);
        paid += claim(&mut s, 0);
        prop_assert_eq!(paid + cancellation.refunded, s.total);
    }

    /// R4: signalling the same milestone again never unlocks it twice and changes nothing.
    #[test]
    fn prop_r4_milestone_idempotent(s in milestones(), indices in vec(any::<u8>(), 0..200)) {
        let mut s = s;
        let n = milestone_count(&s);
        let Kind::Milestones { amounts, .. } = s.kind.clone() else { unreachable!() };
        let mut seen = [false; 256];
        let mut expected = 0u128;
        for i in indices {
            let i = i % (n + 2); // mostly in range, some just past it
            let before = s.clone();
            let result = s.apply_signal_milestone(i);
            if i >= n {
                prop_assert_eq!(result, Err(VestingError::MilestoneOutOfRange));
                prop_assert_eq!(&s, &before);
            } else if seen[usize::from(i)] {
                prop_assert_eq!(result, Err(VestingError::MilestoneAlreadySignalled));
                prop_assert_eq!(&s, &before);
            } else {
                seen[usize::from(i)] = true;
                expected += amounts[usize::from(i)];
                prop_assert_eq!(result.unwrap().amount, amounts[usize::from(i)]);
            }
            prop_assert_eq!(s.vested(0).unwrap(), expected);
        }
    }

    /// F2 (claim as of a time): a claim computed at an earlier time never pays more than one at a later time, and
    /// claiming at t1 then t2 pays the same in total as claiming once at t2.
    #[test]
    fn prop_f2_as_of_earlier_never_pays_more(
        (s, prior, t1, t2) in linear().prop_flat_map(|s| {
            let t = time_for(&s);
            (Just(s), vec(t.clone(), 0..5), t.clone(), t)
        }),
    ) {
        let mut s = s;
        for t in prior { claim(&mut s, t); }
        let (t1, t2) = (t1.min(t2), t1.max(t2));
        prop_assert!(s.claimable(t1).unwrap() <= s.claimable(t2).unwrap());
        let mut split = s.clone();
        let mut once = s.clone();
        let two = claim(&mut split, t1) + claim(&mut split, t2);
        let one = claim(&mut once, t2);
        prop_assert_eq!(two, one);
        prop_assert_eq!(split.claimed, once.claimed);
    }

    /// U7: the next unlock time is exact: the first time after the effective time `max(t, last_time)` at which more
    /// than is already claimed or claimable has vested.
    #[test]
    fn prop_u7_next_unlock_is_exact(
        (s, prior, t) in linear().prop_flat_map(|s| { let t = time_for(&s); (Just(s), vec(t.clone(), 0..3), t) }),
    ) {
        let mut s = s;
        for p in prior { claim(&mut s, p); }
        let te = s.effective_time(t);
        let base = s.claimed.max(s.vested_raw(te).unwrap());
        match s.next_unlock(t).unwrap() {
            NextUnlock::At(r) => {
                prop_assert!(r > te);
                prop_assert!(s.vested_raw(r).unwrap() > base);
                prop_assert!(s.vested_raw(r - 1).unwrap() <= base);
                if s.claimable(t).unwrap() == 0 {
                    prop_assert_eq!(s.apply_claim(t), Err(VestingError::NothingClaimable { next: NextUnlock::At(r) }));
                    prop_assert!(s.apply_claim(r).is_ok());
                }
            }
            NextUnlock::Never => prop_assert_eq!(base, s.total),
            NextUnlock::WhenMilestoneSignalled => prop_assert!(false, "linear schedule"),
        }
    }

    /// F2, one time source per schedule: under any interleaving of claims at any time (a public claim at the clock
    /// time, or a ClaimAsOf up to a block ahead of it, or with an old as_of), signals, make-non-cancelable and a
    /// cancel at any clock time: `last_time` never decreases, `claimed <= vested(last_time) <= total`, a cancel is
    /// refused only with 15 or 16, the escrow never goes below zero, and payouts plus the refund end at exactly the
    /// total.
    #[test]
    fn prop_f2_one_time_source_under_any_interleaving(
        (s, ops) in any_schedule().prop_flat_map(|s| {
            let op = (0u8..5, time_for(&s), any::<u8>());
            (Just(s), vec(op, 0..40))
        }),
    ) {
        let mut s = s;
        let mut paid = 0u128;
        let mut refunded = 0u128;
        let mut last = 0u64;
        for (op, t, x) in ops {
            match op {
                0 | 1 => match s.apply_claim(t) {
                    Ok(out) => {
                        prop_assert!(out.at >= last && out.at >= t);
                        paid += out.amount;
                    }
                    Err(VestingError::NothingClaimable { .. }) => {}
                    Err(e) => prop_assert!(false, "claim refused with {}", e),
                },
                2 => match s.apply_cancel(t) {
                    Ok(c) => {
                        prop_assert!(c.at >= last && c.at >= t);
                        prop_assert!(c.vested_at_cancel >= s.claimed);
                        refunded = c.refunded;
                    }
                    Err(e) => prop_assert!(
                        matches!(e, VestingError::NotCancelable | VestingError::AlreadyCancelled),
                        "cancel refused with {}", e
                    ),
                },
                3 => { let _ = s.apply_signal_milestone(x % 66); }
                _ => { let _ = s.apply_make_non_cancelable(); }
            }
            prop_assert!(s.last_time >= last, "last_time went back");
            last = s.last_time;
            prop_assert_eq!(s.claimed, paid);
            prop_assert!(s.claimed <= s.vested(s.last_time).unwrap());
            prop_assert!(s.vested(s.last_time).unwrap() <= s.total);
            let out = paid.checked_add(refunded).expect("no overflow");
            prop_assert!(out <= s.total, "escrow below zero");
        }
        signal_all(&mut s);
        paid += claim(&mut s, u64::MAX);
        prop_assert_eq!(paid + refunded, s.total);
        prop_assert!(s.is_settled());
    }

    /// Every refused operation leaves the schedule unchanged, and the invariants hold after every operation.
    #[test]
    fn prop_refusals_change_nothing_and_invariants_hold(
        (s, ops) in any_schedule().prop_flat_map(|s| {
            let op = (0u8..6, time_for(&s), any::<u8>());
            (Just(s), vec(op, 0..40))
        }),
    ) {
        let mut s = s;
        for (op, t, x) in ops {
            let before = s.clone();
            let refused = match op {
                0 => s.apply_claim(t).is_err(),
                1 => s.apply_cancel(t).is_err(),
                2 => s.apply_make_non_cancelable().is_err(),
                3 => s.apply_signal_milestone(x % 66).is_err(),
                4 => s.apply_transfer_beneficiary(AccountId::new([x; 32])).is_err(),
                _ => s.begin(Pending::Cancel).and_then(|()| s.finish().map(|_| ())).is_err(),
            };
            if refused { prop_assert_eq!(&s, &before); }
            prop_assert!(s.claimed <= s.vested(s.last_time).unwrap());
            prop_assert!(s.claimed <= s.vested(u64::MAX).unwrap());
            prop_assert!(s.vested(t).unwrap() <= s.total);
            prop_assert_eq!(s.pending, None);
            if let Some(c) = s.cancelled { prop_assert_eq!(c.vested_at_cancel + c.refunded, s.total); }
            prop_assert_eq!(s.total, before.total);
        }
    }

    /// F5: batch milestone amounts always sum to the item total.
    #[test]
    fn prop_f5_batch_milestone_amounts_sum_to_total(
        total in prop_oneof![1u128..=10_000_000, any::<u128>()],
        cuts in vec(1u16..10_000, 0..63),
    ) {
        let mut cuts = cuts;
        cuts.sort_unstable();
        cuts.dedup();
        let mut bps = Vec::new();
        let mut last = 0u16;
        for c in cuts.iter().chain([10_000].iter()) { bps.push(*c - last); last = *c; }
        match batch_milestone_amounts(total, &bps) {
            Ok(amounts) => {
                prop_assert_eq!(amounts.len(), bps.len());
                prop_assert_eq!(amounts.iter().sum::<u128>(), total);
                prop_assert!(amounts.iter().all(|a| *a > 0));
            }
            Err(e) => {
                prop_assert!(total == 0 || e == VestingError::BadMilestones);
            }
        }
    }

    /// Fees never exceed 1% of the total.
    #[test]
    fn prop_fee_at_most_one_percent(total in any::<u128>(), rate in any::<u16>()) {
        match fee_for(total, rate) {
            Ok(fee) => prop_assert!(fee <= total / 100),
            Err(e) => { prop_assert_eq!(e, VestingError::FeeAboveCap); prop_assert!(rate > 100); }
        }
    }

    /// The state encoding roundtrips for any schedule, after any claims.
    #[test]
    fn prop_state_roundtrips((s, t) in any_schedule().prop_flat_map(|s| { let t = time_for(&s); (Just(s), t) })) {
        let mut s = s;
        signal_all(&mut s);
        claim(&mut s, t);
        let state = ScheduleState::V1(s);
        prop_assert_eq!(ScheduleState::decode(&state.encode()), Ok(state));
    }

    /// Decoding arbitrary bytes never panics.
    #[test]
    fn prop_decoding_garbage_never_panics(bytes in vec(any::<u8>(), 0..300)) {
        let _ = Message::decode(&bytes);
        let _ = Internal::decode(&bytes);
        let _ = ScheduleState::decode(&bytes);
    }
}
