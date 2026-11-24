//! Events (SPEC section 7): LP-0012 `ProgramEvent`s with selector = the first 8 bytes of
//! `SHA-256("vesting::<EventName>")` and data = the Borsh encoding of the struct.
//!
//! Selectors are constants here (so the program spends no cycles hashing); a test recomputes each one with
//! [`event_selector`]. No event carries a claim destination.

use alloc::vec::Vec;

use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Digest, Sha256};

use crate::state::{AccountId, Asset, Cancellation, Kind, ScheduleV1};

/// Prefix of every selector preimage.
pub const SELECTOR_PREFIX: &str = "vesting::";

/// The first 8 bytes of `SHA-256("vesting::" ‖ name)`.
#[must_use]
pub fn event_selector(name: &str) -> [u8; 8] {
    let digest = Sha256::new()
        .chain_update(SELECTOR_PREFIX)
        .chain_update(name)
        .finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// An event of `vesting` or `vesting_fees`.
pub trait Event: BorshSerialize {
    /// The event name, as in SPEC section 7.
    const NAME: &'static str;
    /// `event_selector(NAME)`.
    const SELECTOR: [u8; 8];

    /// The event data: the Borsh encoding of the struct.
    fn data(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }
}

macro_rules! event {
    ($name:ident, $selector:expr) => {
        impl Event for $name {
            const NAME: &'static str = stringify!($name);
            const SELECTOR: [u8; 8] = $selector;
        }
    };
}

/// A schedule was opened (`Open`).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ScheduleCreated {
    /// The schedule account.
    pub schedule: AccountId,
    /// The id the creator chose; with `creator` it derives `schedule`.
    pub schedule_id: [u8; 32],
    /// Its creator.
    pub creator: AccountId,
    /// Its beneficiary.
    pub beneficiary: AccountId,
    /// The vested asset.
    pub asset: Asset,
    /// The escrowed total.
    pub total: u128,
    /// The curve.
    pub kind: Kind,
    /// The cancelable flag.
    pub cancelable: bool,
    /// The transferable flag.
    pub transferable: bool,
    /// Who may cancel.
    pub cancel_authority: AccountId,
    /// Who may signal milestones.
    pub milestone_authority: AccountId,
    /// Where a cancel refunds to.
    pub refund_to: AccountId,
}

impl ScheduleCreated {
    /// The event for the schedule `state` opened at account `schedule`.
    #[must_use]
    pub fn from_state(schedule: AccountId, state: &ScheduleV1) -> Self {
        Self {
            schedule,
            schedule_id: state.schedule_id,
            creator: state.creator,
            beneficiary: state.beneficiary,
            asset: state.asset,
            total: state.total,
            kind: state.kind.clone(),
            cancelable: state.cancelable,
            transferable: state.transferable,
            cancel_authority: state.cancel_authority,
            milestone_authority: state.milestone_authority,
            refund_to: state.refund_to,
        }
    }
}

/// A claim paid out. Carries the beneficiary (already public in the schedule state), never the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Claimed {
    /// The schedule account.
    pub schedule: AccountId,
    /// The beneficiary that claimed.
    pub beneficiary: AccountId,
    /// Paid by this claim.
    pub amount: u128,
    /// The effective time the claim was computed at: `max(clock time or as_of, last_time)`.
    pub at: u64,
    /// `claimed` after this claim.
    pub claimed_total: u128,
}

/// A schedule was cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Cancelled {
    /// The schedule account.
    pub schedule: AccountId,
    /// Effective time of the cancel: `max(clock time, last_time)`.
    pub at: u64,
    /// The vested amount, frozen.
    pub vested_at_cancel: u128,
    /// Paid to `refund_to`.
    pub refunded: u128,
    /// The refund account.
    pub refund_to: AccountId,
}

impl Cancelled {
    /// The event for `cancellation` of the schedule at account `schedule`.
    #[must_use]
    pub const fn new(schedule: AccountId, cancellation: &Cancellation, refund_to: AccountId) -> Self {
        Self {
            schedule,
            at: cancellation.at,
            vested_at_cancel: cancellation.vested_at_cancel,
            refunded: cancellation.refunded,
            refund_to,
        }
    }
}

/// A milestone was signalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct MilestoneSignalled {
    /// The schedule account.
    pub schedule: AccountId,
    /// The milestone index.
    pub index: u8,
    /// The amount it unlocked.
    pub amount: u128,
    /// The bitmap after the signal.
    pub signalled: u64,
}

/// The beneficiary changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BeneficiaryTransferred {
    /// The schedule account.
    pub schedule: AccountId,
    /// The previous beneficiary.
    pub from: AccountId,
    /// The new beneficiary.
    pub to: AccountId,
}

/// The schedule became non-cancelable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct MadeNonCancelable {
    /// The schedule account.
    pub schedule: AccountId,
    /// Who did it (the creator or the cancel authority).
    pub by: AccountId,
}

/// The fee rate or treasury changed (emitted by `vesting_fees`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct FeeChanged {
    /// The rate before, in basis points.
    pub old_rate_bps: u16,
    /// The rate after, in basis points.
    pub new_rate_bps: u16,
    /// The treasury after.
    pub treasury: AccountId,
    /// The admin who changed it.
    pub by: AccountId,
}

event!(ScheduleCreated, [0x68, 0xee, 0x83, 0x9f, 0x2c, 0xe9, 0xef, 0xac]);
event!(Claimed, [0xaa, 0x25, 0xb4, 0x64, 0x1c, 0xe8, 0xcb, 0x29]);
event!(Cancelled, [0xfe, 0xe8, 0x2a, 0x2f, 0x47, 0x26, 0x3d, 0x1f]);
event!(MilestoneSignalled, [0x08, 0x82, 0x43, 0x93, 0x81, 0xb3, 0xee, 0xc9]);
event!(BeneficiaryTransferred, [0x3e, 0xdd, 0x69, 0xd4, 0x33, 0x32, 0xbd, 0xb2]);
event!(MadeNonCancelable, [0x94, 0x7b, 0xed, 0xe7, 0x0f, 0xe0, 0x2f, 0x82]);
event!(FeeChanged, [0x14, 0xe7, 0x60, 0x20, 0x77, 0xf3, 0x19, 0x73]);

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    const A: AccountId = AccountId::new([1; 32]);
    const B: AccountId = AccountId::new([2; 32]);

    fn check<E: Event + BorshDeserialize + PartialEq + core::fmt::Debug>(event: &E) {
        assert_eq!(E::SELECTOR, event_selector(E::NAME), "selector constant of {}", E::NAME);
        assert_eq!(
            &borsh::from_slice::<E>(&event.data()).unwrap(),
            event,
            "{} roundtrips",
            E::NAME
        );
    }

    #[test]
    fn f6_event_roundtrip_and_selectors() {
        let state = crate::Terms {
            beneficiary: B,
            asset: Asset::Token { definition: B },
            total: 10,
            kind: Kind::Milestones {
                amounts: vec![4, 6],
                signalled: 0,
            },
            cancelable: true,
            transferable: true,
            cancel_authority: Some(B),
            milestone_authority: None,
            refund_to: None,
        }
        .into_state(A, [0; 32], A)
        .unwrap();
        check(&ScheduleCreated::from_state(A, &state));
        check(&Claimed {
            schedule: A,
            beneficiary: B,
            amount: 1,
            at: 2,
            claimed_total: 3,
        });
        check(&Cancelled {
            schedule: A,
            at: 1,
            vested_at_cancel: 2,
            refunded: 3,
            refund_to: B,
        });
        check(&MilestoneSignalled {
            schedule: A,
            index: 1,
            amount: 2,
            signalled: 3,
        });
        check(&BeneficiaryTransferred {
            schedule: A,
            from: A,
            to: B,
        });
        check(&MadeNonCancelable { schedule: A, by: B });
        check(&FeeChanged {
            old_rate_bps: 0,
            new_rate_bps: 50,
            treasury: A,
            by: B,
        });
    }

    #[test]
    fn f6_selector_matches_independent_hash() {
        // sha256("vesting::Claimed") = aa25b4641ce8cb29...
        assert_eq!(
            event_selector("Claimed"),
            [0xaa, 0x25, 0xb4, 0x64, 0x1c, 0xe8, 0xcb, 0x29]
        );
    }

    #[test]
    fn f6_schedule_created_copies_every_term() {
        let state = crate::Terms {
            beneficiary: B,
            asset: Asset::Native,
            total: 10,
            kind: Kind::Linear {
                start: 1,
                cliff: 2,
                end: 3,
            },
            cancelable: false,
            transferable: true,
            cancel_authority: None,
            milestone_authority: Some(B),
            refund_to: Some(B),
        }
        .into_state(A, [5; 32], A)
        .unwrap();
        let e = ScheduleCreated::from_state(B, &state);
        assert_eq!(
            (e.schedule, e.schedule_id, e.creator, e.beneficiary, e.total),
            (B, [5; 32], A, B, 10)
        );
        assert_eq!((e.cancelable, e.transferable), (false, true));
        assert_eq!((e.cancel_authority, e.milestone_authority, e.refund_to), (A, B, B));
        assert_eq!(e.kind, state.kind);
    }

    #[test]
    fn f6_cancelled_event_from_cancellation() {
        let c = Cancellation {
            at: 5,
            vested_at_cancel: 6,
            refunded: 7,
        };
        assert_eq!(
            Cancelled::new(A, &c, B),
            Cancelled {
                schedule: A,
                at: 5,
                vested_at_cancel: 6,
                refunded: 7,
                refund_to: B
            }
        );
    }

    #[test]
    fn f2_claimed_event_has_no_destination_field() {
        // Fixed layout: schedule (32) + beneficiary (32) + amount (16) + at (8) + claimed_total (16). Nothing else
        // fits, so no destination can be carried.
        let data = Claimed {
            schedule: A,
            beneficiary: B,
            amount: 1,
            at: 2,
            claimed_total: 3,
        }
        .data();
        assert_eq!(data.len(), 32 + 32 + 16 + 8 + 16);
    }
}
