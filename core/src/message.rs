//! Messages of `vesting` (SPEC sections 3.1 and 3.3). Borsh enums; the variant order is the wire tag.

use alloc::vec::Vec;

use borsh::{BorshDeserialize, BorshSerialize};

use crate::create::{BatchItem, BatchTerms, Terms};
use crate::state::{AccountId, ScheduleV1};
use crate::{Result, VestingError};

/// A message a transaction sends to the caller's own actor `(caller, vesting)` (SPEC 3.1).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Message {
    /// Tag 0: open one schedule and fund its escrow from `source`; the caller becomes the creator.
    Create {
        /// Chosen by the creator; with the creator it derives the schedule account.
        schedule_id: [u8; 32],
        /// The schedule's terms.
        terms: Terms,
        /// Funding account (native balance, or token holding of the definition). Default refund account.
        source: AccountId,
        /// The creation fee the SDK quoted; checked by `vesting_fees`, and against the hard cap by `vesting`.
        fee: u128,
        /// The fee treasury the SDK read from the `vesting_fees` config; checked by `vesting_fees` when `fee > 0`.
        treasury: AccountId,
    },
    /// Tag 1: open one schedule per item, sharing `common`, in one transaction.
    CreateBatch {
        /// Terms shared by every item.
        common: BatchTerms,
        /// One schedule each.
        items: Vec<BatchItem>,
        /// Funding account. Default refund account of every item.
        source: AccountId,
        /// The creation fee on the sum of item totals.
        fee: u128,
        /// The fee treasury, as in `Create`.
        treasury: AccountId,
    },
    /// Tag 2: pay everything claimable now (clock time) to `to`.
    Claim {
        /// The schedule account.
        schedule: AccountId,
        /// The payout destination; never stored or emitted.
        to: AccountId,
    },
    /// Tag 3: pay everything claimable at `as_of` to `to`; the transaction is valid from `as_of` on.
    ClaimAsOf {
        /// The schedule account.
        schedule: AccountId,
        /// The payout destination; never stored or emitted. On the privacy-preserving path, a one-time blinded
        /// address of the claimant's private account (SPEC 9), never its canonical id.
        to: AccountId,
        /// The time the claim is computed at (ms); the claim is computed at `max(as_of, last_time)`.
        as_of: u64,
    },
    /// Tag 4: cancel, refunding the unvested part.
    Cancel {
        /// The schedule account.
        schedule: AccountId,
    },
    /// Tag 5: make the schedule non-cancelable, for good.
    MakeNonCancelable {
        /// The schedule account.
        schedule: AccountId,
    },
    /// Tag 6: signal milestone `index`.
    SignalMilestone {
        /// The schedule account.
        schedule: AccountId,
        /// The milestone index.
        index: u8,
    },
    /// Tag 7: hand the schedule to `new_beneficiary`.
    TransferBeneficiary {
        /// The schedule account.
        schedule: AccountId,
        /// The new beneficiary.
        new_beneficiary: AccountId,
    },
}

impl Message {
    /// Decodes a message; the whole input must be consumed.
    ///
    /// # Errors
    /// [`VestingError::InvalidMessage`] (2).
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::try_from_slice(bytes).map_err(|_| VestingError::InvalidMessage)
    }

    /// Borsh encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }

    /// For a message that acts on one existing schedule: that schedule's account and the [`Internal`] message the
    /// caller actor forwards to it. `None` for `Create` and `CreateBatch`.
    #[must_use]
    pub const fn forward(&self) -> Option<(AccountId, Internal)> {
        match *self {
            Self::Create { .. } | Self::CreateBatch { .. } => None,
            Self::Claim { schedule, to } => Some((schedule, Internal::ClaimFor { to })),
            Self::ClaimAsOf { schedule, to, as_of } => Some((schedule, Internal::ClaimForAsOf { to, as_of })),
            Self::Cancel { schedule } => Some((schedule, Internal::CancelFor)),
            Self::MakeNonCancelable { schedule } => Some((schedule, Internal::MakeNonCancelableFor)),
            Self::SignalMilestone { schedule, index } => Some((schedule, Internal::SignalFor { index })),
            Self::TransferBeneficiary {
                schedule,
                new_beneficiary,
            } => Some((schedule, Internal::TransferFor { new_beneficiary })),
        }
    }
}

/// A message from a caller actor to a schedule actor; accepted only from a `vesting` actor (SPEC 3.3).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "decoded once per call and never stored; the unboxed state keeps the type exactly as SPEC 3.3 writes it"
)]
pub enum Internal {
    /// Tag 0: write a new schedule. Accepted only from `(state.creator, vesting)` at the derived account.
    Open {
        /// The complete initial state, checked with [`ScheduleV1::validate_new`].
        state: ScheduleV1,
    },
    /// Tag 1: a public claim to `to`; completes on the clock reply.
    ClaimFor {
        /// The payout destination.
        to: AccountId,
    },
    /// Tag 2: a claim computed at `as_of`.
    ClaimForAsOf {
        /// The payout destination.
        to: AccountId,
        /// The time the claim is computed at (ms).
        as_of: u64,
    },
    /// Tag 3: a cancel; completes on the clock reply.
    CancelFor,
    /// Tag 4: make the schedule non-cancelable.
    MakeNonCancelableFor,
    /// Tag 5: signal milestone `index`.
    SignalFor {
        /// The milestone index.
        index: u8,
    },
    /// Tag 6: hand the schedule to `new_beneficiary`.
    TransferFor {
        /// The new beneficiary.
        new_beneficiary: AccountId,
    },
}

impl Internal {
    /// Decodes an internal message; the whole input must be consumed.
    ///
    /// # Errors
    /// [`VestingError::InvalidMessage`] (2).
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::try_from_slice(bytes).map_err(|_| VestingError::InvalidMessage)
    }

    /// Borsh encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }

    /// The role whose holder must have sent this message (checked with [`ScheduleV1::authorise`]). `None` for
    /// `Open`, whose sender is checked against the creator named in the state it carries.
    #[must_use]
    pub const fn required_role(&self) -> Option<Role> {
        match self {
            Self::Open { .. } => None,
            Self::ClaimFor { .. } | Self::ClaimForAsOf { .. } | Self::TransferFor { .. } => Some(Role::Beneficiary),
            Self::CancelFor => Some(Role::CancelAuthority),
            Self::MakeNonCancelableFor => Some(Role::Creator),
            Self::SignalFor { .. } => Some(Role::MilestoneAuthority),
        }
    }
}

/// Who may send a message to a schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The current beneficiary (claims, transfer).
    Beneficiary,
    /// The creator; [`ScheduleV1::authorise`] also admits the cancel authority (`MakeNonCancelable`).
    Creator,
    /// The cancel authority (cancel).
    CancelAuthority,
    /// The milestone authority (signal).
    MilestoneAuthority,
}

impl Role {
    /// The error a sender that does not hold this role gets.
    #[must_use]
    pub const fn refusal(self) -> VestingError {
        match self {
            Self::Beneficiary => VestingError::NotBeneficiary,
            Self::Creator => VestingError::NotCreator,
            Self::CancelAuthority => VestingError::NotCancelAuthority,
            Self::MilestoneAuthority => VestingError::NotMilestoneAuthority,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::create::BatchShape;
    use crate::state::{Asset, Kind};

    const A: AccountId = AccountId::new([1; 32]);
    const B: AccountId = AccountId::new([2; 32]);

    fn terms() -> Terms {
        Terms {
            beneficiary: B,
            asset: Asset::Native,
            total: 5,
            kind: Kind::Linear {
                start: 0,
                cliff: 0,
                end: 10,
            },
            cancelable: true,
            transferable: false,
            cancel_authority: None,
            milestone_authority: Some(A),
            refund_to: None,
        }
    }

    fn every_message() -> Vec<Message> {
        vec![
            Message::Create {
                schedule_id: [3; 32],
                terms: terms(),
                source: A,
                fee: 0,
                treasury: B,
            },
            Message::CreateBatch {
                common: BatchTerms {
                    asset: Asset::Native,
                    shape: BatchShape::Milestones {
                        bps: vec![5_000, 5_000],
                    },
                    cancelable: false,
                    transferable: true,
                    cancel_authority: None,
                    milestone_authority: None,
                    refund_to: Some(B),
                },
                items: vec![BatchItem {
                    schedule_id: [4; 32],
                    beneficiary: B,
                    total: 9,
                }],
                source: A,
                fee: 1,
                treasury: B,
            },
            Message::Claim { schedule: A, to: B },
            Message::ClaimAsOf {
                schedule: A,
                to: B,
                as_of: 77,
            },
            Message::Cancel { schedule: A },
            Message::MakeNonCancelable { schedule: A },
            Message::SignalMilestone { schedule: A, index: 3 },
            Message::TransferBeneficiary {
                schedule: A,
                new_beneficiary: B,
            },
        ]
    }

    #[test]
    fn message_tags_follow_the_spec_and_roundtrip() {
        for (tag, message) in every_message().into_iter().enumerate() {
            let bytes = message.encode();
            assert_eq!(usize::from(bytes[0]), tag);
            assert_eq!(Message::decode(&bytes), Ok(message));
        }
    }

    #[test]
    fn claim_encoding_is_tag_schedule_to() {
        let bytes = Message::Claim { schedule: A, to: B }.encode();
        assert_eq!(bytes.len(), 1 + 32 + 32);
        assert_eq!(&bytes[1..33], A.value());
        assert_eq!(&bytes[33..], B.value());
    }

    #[test]
    fn undecodable_message_is_2() {
        assert_eq!(Message::decode(&[]), Err(VestingError::InvalidMessage));
        assert_eq!(Message::decode(&[8]), Err(VestingError::InvalidMessage), "unknown tag");
        let mut bytes = Message::Cancel { schedule: A }.encode();
        bytes.push(0);
        assert_eq!(
            Message::decode(&bytes),
            Err(VestingError::InvalidMessage),
            "trailing bytes"
        );
        assert_eq!(Internal::decode(&[7]), Err(VestingError::InvalidMessage));
        assert_eq!(VestingError::InvalidMessage.code(), 2);
    }

    #[test]
    fn forwarding_maps_each_message_to_its_internal_message() {
        let forwarded: Vec<_> = every_message().iter().map(Message::forward).collect();
        assert_eq!(forwarded[0], None);
        assert_eq!(forwarded[1], None);
        assert_eq!(forwarded[2], Some((A, Internal::ClaimFor { to: B })));
        assert_eq!(forwarded[3], Some((A, Internal::ClaimForAsOf { to: B, as_of: 77 })));
        assert_eq!(forwarded[4], Some((A, Internal::CancelFor)));
        assert_eq!(forwarded[5], Some((A, Internal::MakeNonCancelableFor)));
        assert_eq!(forwarded[6], Some((A, Internal::SignalFor { index: 3 })));
        assert_eq!(forwarded[7], Some((A, Internal::TransferFor { new_beneficiary: B })));
    }

    #[test]
    fn internal_tags_follow_the_spec_and_roundtrip() {
        let state = terms().into_state(A, [3; 32], A).unwrap();
        let all = vec![
            Internal::Open { state },
            Internal::ClaimFor { to: B },
            Internal::ClaimForAsOf { to: B, as_of: 1 },
            Internal::CancelFor,
            Internal::MakeNonCancelableFor,
            Internal::SignalFor { index: 0 },
            Internal::TransferFor { new_beneficiary: A },
        ];
        for (tag, message) in all.into_iter().enumerate() {
            let bytes = message.encode();
            assert_eq!(usize::from(bytes[0]), tag);
            assert_eq!(Internal::decode(&bytes), Ok(message));
        }
    }

    #[test]
    fn internal_messages_name_the_role_the_spec_requires() {
        assert_eq!(Internal::ClaimFor { to: A }.required_role(), Some(Role::Beneficiary));
        assert_eq!(
            Internal::ClaimForAsOf { to: A, as_of: 0 }.required_role(),
            Some(Role::Beneficiary)
        );
        assert_eq!(
            Internal::TransferFor { new_beneficiary: A }.required_role(),
            Some(Role::Beneficiary)
        );
        assert_eq!(Internal::CancelFor.required_role(), Some(Role::CancelAuthority));
        assert_eq!(Internal::MakeNonCancelableFor.required_role(), Some(Role::Creator));
        assert_eq!(
            Internal::SignalFor { index: 0 }.required_role(),
            Some(Role::MilestoneAuthority)
        );
        let state = terms().into_state(A, [3; 32], A).unwrap();
        assert_eq!(Internal::Open { state }.required_role(), None);
    }

    #[test]
    fn each_role_refuses_with_its_code() {
        assert_eq!(Role::Beneficiary.refusal().code(), 3);
        assert_eq!(Role::Creator.refusal().code(), 4);
        assert_eq!(Role::CancelAuthority.refusal().code(), 5);
        assert_eq!(Role::MilestoneAuthority.refusal().code(), 6);
    }
}
