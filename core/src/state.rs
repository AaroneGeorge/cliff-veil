//! Schedule state (SPEC section 2).
//!
//! Field order and enum variant order are the Borsh wire format. They never change; a new layout is a new
//! [`ScheduleState`] variant.

use alloc::vec::Vec;
use core::fmt;

use borsh::{BorshDeserialize, BorshSerialize};

use crate::{Result, VestingError};

/// Most milestones a schedule may have: one bit each in the `u64` `signalled` bitmap.
pub const MAX_MILESTONES: usize = 64;

/// A LEZ account id: 32 bytes.
///
/// Its Borsh encoding is the 32 bytes as they are, which is also the encoding of `lee_core::account::AccountId`, so
/// a message or state encoded with this type decodes with LEZ's type and back. The program and the SDK convert
/// with `AccountId::new(id.into_value())` / `vesting_core::AccountId::new(*id.value())`.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize)]
pub struct AccountId([u8; 32]);

impl AccountId {
    /// Wraps 32 bytes.
    #[must_use]
    pub const fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    /// The 32 bytes.
    #[must_use]
    pub const fn value(&self) -> &[u8; 32] {
        &self.0
    }

    /// The 32 bytes, by value.
    #[must_use]
    pub const fn into_value(self) -> [u8; 32] {
        self.0
    }
}

impl From<[u8; 32]> for AccountId {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
}

impl fmt::Debug for AccountId {
    /// Lowercase hex (LEZ itself prints base58; hex keeps this crate free of an encoding dependency).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// The state of a schedule actor under `vesting`: a versioned enum, only [`ScheduleState::V1`] exists.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ScheduleState {
    /// Layout version 1 (Borsh tag 0).
    V1(ScheduleV1),
}

impl ScheduleState {
    /// Decodes a schedule actor's state.
    ///
    /// # Errors
    /// [`VestingError::NoSuchSchedule`] (8) when the state is empty (no schedule was ever opened at this account)
    /// or is not a schedule state. `vesting` writes only schedule states, so the second case cannot occur for an
    /// account it owns.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Err(VestingError::NoSuchSchedule);
        }
        Self::try_from_slice(bytes).map_err(|_| VestingError::NoSuchSchedule)
    }

    /// Decodes the state and returns the version 1 schedule. See [`ScheduleState::decode`].
    ///
    /// # Errors
    /// As [`ScheduleState::decode`].
    pub fn decode_v1(bytes: &[u8]) -> Result<ScheduleV1> {
        let Self::V1(schedule) = Self::decode(bytes)?;
        Ok(schedule)
    }

    /// Borsh encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }
}

impl From<ScheduleV1> for ScheduleState {
    fn from(schedule: ScheduleV1) -> Self {
        Self::V1(schedule)
    }
}

/// One vesting schedule, version 1.
///
/// Invariants kept by every function in [`crate::rules`]: `0 < total`; `claimed <= vested(t) <= total` for every
/// `t >= last_time`; `last_time` never decreases; and `pending` is `None` between transactions.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ScheduleV1 {
    /// Chosen by the creator; with `creator` it derives the schedule account.
    pub schedule_id: [u8; 32],
    /// The account that created and funded the schedule.
    pub creator: AccountId,
    /// Who may claim. Changes only by `TransferBeneficiary`, and only when `transferable`.
    pub beneficiary: AccountId,
    /// What is vested.
    pub asset: Asset,
    /// Escrowed at creation; greater than zero.
    pub total: u128,
    /// Paid out so far; never more than the vested amount at `last_time`.
    pub claimed: u128,
    /// High-water time (ms): the latest time a claim or cancel was computed at; 0 until the first one. Every claim
    /// and cancel is computed at `max(t, last_time)`, so the clock read and a claimant's `as_of` act as one
    /// non-decreasing time source per schedule.
    pub last_time: u64,
    /// The vesting curve.
    pub kind: Kind,
    /// `true` to `false` only (`MakeNonCancelable`).
    pub cancelable: bool,
    /// Written at creation, never changed.
    pub transferable: bool,
    /// May cancel. Default: the creator.
    pub cancel_authority: AccountId,
    /// May signal milestones. Default: the creator.
    pub milestone_authority: AccountId,
    /// Receives the unvested part on cancel. Default: the funding account (`source`). Fixed at creation.
    pub refund_to: AccountId,
    /// Set once by a cancel.
    pub cancelled: Option<Cancellation>,
    /// An operation waiting for the clock reply; `Some` only inside one transaction.
    pub pending: Option<Pending>,
}

/// The vested asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Asset {
    /// The LEZ native token (Borsh tag 0).
    Native,
    /// A fungible token of `definition` under the token program the deployment accepts (Borsh tag 1).
    Token {
        /// The token definition account.
        definition: AccountId,
    },
}

/// The vesting curve.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Kind {
    /// Nothing before `cliff`, then linear accrual measured from `start`, everything at `end` (Borsh tag 0).
    /// `start <= cliff <= end` and `start < end`. Fully linear: `cliff == start`. Pure cliff: `cliff == end`.
    Linear {
        /// Accrual is measured from here (ms).
        start: u64,
        /// Nothing is vested before this time (ms).
        cliff: u64,
        /// Everything is vested from this time on (ms).
        end: u64,
    },
    /// Fixed amounts, each unlocked when the milestone authority signals it (Borsh tag 1).
    /// `1..=64` amounts, each greater than zero, summing to `total`.
    Milestones {
        /// Amount unlocked by milestone `i`.
        amounts: Vec<u128>,
        /// Bit `i` set: milestone `i` was signalled.
        signalled: u64,
    },
}

/// What a cancel recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Cancellation {
    /// Effective time of the cancel (ms): `max(clock time, last_time)`.
    pub at: u64,
    /// The vested amount, frozen at the cancel.
    pub vested_at_cancel: u128,
    /// Paid to `refund_to`: `total - vested_at_cancel`.
    pub refunded: u128,
}

/// An operation waiting for the clock reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Pending {
    /// A public claim paying to `to` (Borsh tag 0).
    Claim {
        /// The payout destination.
        to: AccountId,
    },
    /// A cancel (Borsh tag 1).
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ScheduleV1 {
        ScheduleV1 {
            schedule_id: [9; 32],
            creator: AccountId::new([1; 32]),
            beneficiary: AccountId::new([2; 32]),
            asset: Asset::Token {
                definition: AccountId::new([3; 32]),
            },
            total: 1_000,
            claimed: 10,
            last_time: 77,
            kind: Kind::Milestones {
                amounts: alloc::vec![400, 600],
                signalled: 1,
            },
            cancelable: true,
            transferable: false,
            cancel_authority: AccountId::new([1; 32]),
            milestone_authority: AccountId::new([4; 32]),
            refund_to: AccountId::new([1; 32]),
            cancelled: None,
            pending: Some(Pending::Claim {
                to: AccountId::new([5; 32]),
            }),
        }
    }

    #[test]
    fn state_roundtrips_through_borsh() {
        let state = ScheduleState::V1(sample());
        assert_eq!(ScheduleState::decode(&state.encode()), Ok(state));
    }

    #[test]
    fn state_layout_starts_with_version_tag_then_schedule_id() {
        let bytes = ScheduleState::V1(sample()).encode();
        assert_eq!(bytes[0], 0, "V1 is tag 0");
        assert_eq!(&bytes[1..33], &[9; 32], "schedule_id comes first");
        assert_eq!(&bytes[33..65], &[1; 32], "then the creator, 32 raw bytes");
    }

    #[test]
    fn account_id_encodes_as_its_32_bytes() {
        let id = AccountId::new([0xab; 32]);
        assert_eq!(borsh::to_vec(&id).unwrap(), [0xab; 32]);
        assert_eq!(id.into_value(), [0xab; 32]);
        assert_eq!(AccountId::from([0xab; 32]), id);
    }

    #[test]
    fn account_id_debug_is_hex() {
        let mut bytes = [0; 32];
        bytes[0] = 0xff;
        let text = alloc::format!("{:?}", AccountId::new(bytes));
        assert_eq!(text.len(), 64);
        assert!(text.starts_with("ff00"));
    }

    #[test]
    fn empty_state_is_no_such_schedule() {
        assert_eq!(ScheduleState::decode(&[]), Err(VestingError::NoSuchSchedule));
        assert_eq!(VestingError::NoSuchSchedule.code(), 8);
    }

    #[test]
    fn foreign_or_trailing_bytes_are_refused() {
        let mut bytes = ScheduleState::V1(sample()).encode();
        bytes.push(0);
        assert_eq!(ScheduleState::decode(&bytes), Err(VestingError::NoSuchSchedule));
        assert_eq!(ScheduleState::decode(&[1, 2, 3]), Err(VestingError::NoSuchSchedule));
    }

    #[test]
    fn decode_v1_returns_the_schedule() {
        let bytes = ScheduleState::from(sample()).encode();
        assert_eq!(ScheduleState::decode_v1(&bytes), Ok(sample()));
    }

    #[test]
    fn enum_tags_follow_the_spec() {
        assert_eq!(borsh::to_vec(&Asset::Native).unwrap(), [0]);
        assert_eq!(
            borsh::to_vec(&Asset::Token {
                definition: AccountId::new([0; 32])
            })
            .unwrap()[0],
            1
        );
        assert_eq!(
            borsh::to_vec(&Kind::Linear {
                start: 0,
                cliff: 0,
                end: 1
            })
            .unwrap()[0],
            0
        );
        assert_eq!(
            borsh::to_vec(&Kind::Milestones {
                amounts: alloc::vec![],
                signalled: 0
            })
            .unwrap()[0],
            1
        );
        assert_eq!(
            borsh::to_vec(&Pending::Claim {
                to: AccountId::new([0; 32])
            })
            .unwrap()[0],
            0
        );
        assert_eq!(borsh::to_vec(&Pending::Cancel).unwrap(), [1]);
    }
}
