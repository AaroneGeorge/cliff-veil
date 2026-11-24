//! The creation fee (SPEC sections 3.4 and 6): `fee = floor(total * rate_bps / 10_000)`, charged once at creation,
//! with a hard cap on the rate. `vesting_fees` checks a quoted fee with [`check_fee`]; `vesting` checks it against
//! the cap with [`check_fee_cap`]; the SDK quotes it with [`fee_for`].

use alloc::vec::Vec;

use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Digest, Sha256};

use crate::math::mul_div_floor;
use crate::state::{AccountId, Asset};
use crate::{Result, VestingError};

/// The messages of `vesting_fees` that are not admin operations (SPEC 3.4). Borsh enum; the variant order is the
/// wire tag. `vesting` encodes [`FeeMessage::CheckFee`] with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum FeeMessage {
    /// Tag 0: refuse (27) unless `fee` is the configured rate's fee on `total` and, when `fee > 0`, `treasury` is
    /// the configured treasury. Writes nothing. Accepted from anyone, only at the config actor, in every config
    /// state: an empty config (before `Init`) reads as rate 0, and it keeps working after `Renounce`.
    CheckFee {
        /// The schedule total, or the sum of item totals of a batch.
        total: u128,
        /// The quoted fee.
        fee: u128,
        /// The treasury named by the creation message (the owner of the fee destination).
        treasury: AccountId,
        /// The asset the fee is paid in. Version 1 applies one rate to every asset.
        asset: Asset,
    },
    /// Tag 1: set the rate and treasury; admin only. Refuses a rate above [`MAX_FEE_BPS`] (28).
    SetFee {
        /// The new rate in basis points.
        rate_bps: u16,
        /// The new treasury.
        treasury: AccountId,
    },
}

impl FeeMessage {
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
}

/// Basis points in a whole.
pub const BPS_DENOMINATOR: u64 = 10_000;

/// Hard cap on the fee rate: 100 basis points (1%). No message can raise it.
pub const MAX_FEE_BPS: u16 = 100;

/// The fee on `total` at `rate_bps`: `floor(total * rate_bps / 10_000)`. For a batch, `total` is the sum of the
/// item totals.
///
/// # Errors
/// [`VestingError::FeeAboveCap`] (28) when `rate_bps > MAX_FEE_BPS`, so the fee is always at most 1% of `total`.
pub fn fee_for(total: u128, rate_bps: u16) -> Result<u128> {
    check_rate(rate_bps)?;
    // rate_bps <= 100 < 10_000, so the result is at most total and cannot overflow.
    mul_div_floor(total, u64::from(rate_bps), BPS_DENOMINATOR).ok_or(VestingError::Overflow)
}

/// Checks a new rate for `SetFee`.
///
/// # Errors
/// [`VestingError::FeeAboveCap`] (28) when `rate_bps > MAX_FEE_BPS`.
pub const fn check_rate(rate_bps: u16) -> Result<()> {
    if rate_bps > MAX_FEE_BPS {
        Err(VestingError::FeeAboveCap)
    } else {
        Ok(())
    }
}

/// The hard cap check `vesting` runs itself before it asks `vesting_fees`, so the cap holds whatever the fee program
/// does: `fee <= floor(total * MAX_FEE_BPS / 10_000)`.
///
/// # Errors
/// [`VestingError::FeeAboveCap`] (28).
pub fn check_fee_cap(total: u128, fee: u128) -> Result<()> {
    if fee > fee_for(total, MAX_FEE_BPS)? {
        Err(VestingError::FeeAboveCap)
    } else {
        Ok(())
    }
}

/// The seed of the treasury's associated token account for a token fee (SPEC section 6):
/// `SHA-256(token_program ‖ owner ‖ definition)`, the ATA program's `compute_ata_seed`. The account is
/// `AccountId::for_public_pda(ata_program, PdaSeed::new(seed))` (LEZ), which this crate does not compute.
#[must_use]
pub fn ata_seed(token_program: &AccountId, owner: &AccountId, definition: &AccountId) -> [u8; 32] {
    Sha256::new()
        .chain_update(token_program.value())
        .chain_update(owner.value())
        .chain_update(definition.value())
        .finalize()
        .into()
}

/// Checks a creation's quoted `fee` and `treasury` against the configured `rate_bps` and `configured_treasury`
/// (`CheckFee`). A zero fee is accepted with any treasury when the configured rate makes the fee zero.
///
/// # Errors
/// [`VestingError::FeeMismatch`] (27) when `fee != fee_for(total, rate_bps)`, or when the fee is non-zero and
/// `treasury != configured_treasury`; [`VestingError::FeeAboveCap`] (28) for a configured rate above the cap.
pub fn check_fee(
    total: u128,
    fee: u128,
    treasury: &AccountId,
    rate_bps: u16,
    configured_treasury: &AccountId,
) -> Result<()> {
    let expected = fee_for(total, rate_bps)?;
    if fee != expected || (fee != 0 && treasury != configured_treasury) {
        return Err(VestingError::FeeMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREASURY: AccountId = AccountId::new([7; 32]);
    const OTHER: AccountId = AccountId::new([8; 32]);

    #[test]
    fn fee_zero_by_default() {
        assert_eq!(fee_for(1_000_000, 0), Ok(0));
        check_fee(1_000_000, 0, &OTHER, 0, &TREASURY).unwrap();
    }

    #[test]
    fn fee_is_floor_of_rate() {
        assert_eq!(fee_for(1_000_000, 100), Ok(10_000));
        assert_eq!(fee_for(199, 50), Ok(0), "0.995 rounds down");
        assert_eq!(fee_for(200, 50), Ok(1));
        assert_eq!(fee_for(u128::MAX, 100), Ok(u128::MAX / 100));
    }

    #[test]
    fn fee_mismatch_is_27() {
        assert_eq!(
            check_fee(1_000, 1, &TREASURY, 0, &TREASURY),
            Err(VestingError::FeeMismatch)
        );
        assert_eq!(
            check_fee(1_000, 0, &TREASURY, 100, &TREASURY),
            Err(VestingError::FeeMismatch)
        );
        assert_eq!(
            check_fee(1_000, 10, &OTHER, 100, &TREASURY),
            Err(VestingError::FeeMismatch),
            "wrong treasury"
        );
        check_fee(1_000, 10, &TREASURY, 100, &TREASURY).unwrap();
        // The fee rounds to zero: any treasury.
        check_fee(99, 0, &OTHER, 100, &TREASURY).unwrap();
        assert_eq!(VestingError::FeeMismatch.code(), 27);
    }

    #[test]
    fn vesting_enforces_the_cap_itself() {
        check_fee_cap(1_000, 10).unwrap();
        check_fee_cap(1_000, 0).unwrap();
        assert_eq!(check_fee_cap(1_000, 11), Err(VestingError::FeeAboveCap));
        assert_eq!(
            check_fee_cap(99, 1),
            Err(VestingError::FeeAboveCap),
            "1% of 99 floors to 0"
        );
        check_fee_cap(u128::MAX, u128::MAX / 100).unwrap();
    }

    #[test]
    fn fee_message_tags_follow_the_spec_and_roundtrip() {
        let check = FeeMessage::CheckFee {
            total: 5,
            fee: 0,
            treasury: TREASURY,
            asset: Asset::Token { definition: OTHER },
        };
        let bytes = check.encode();
        assert_eq!(bytes[0], 0);
        assert_eq!(bytes.len(), 1 + 16 + 16 + 32 + 1 + 32);
        assert_eq!(FeeMessage::decode(&bytes), Ok(check));
        let set = FeeMessage::SetFee {
            rate_bps: 100,
            treasury: TREASURY,
        };
        assert_eq!(set.encode()[0], 1);
        assert_eq!(FeeMessage::decode(&set.encode()), Ok(set));
        assert_eq!(FeeMessage::decode(&[2]), Err(VestingError::InvalidMessage));
    }

    #[test]
    fn ata_seed_hashes_program_owner_definition_in_order() {
        let (program, owner, definition) = (AccountId::new([1; 32]), TREASURY, OTHER);
        let mut preimage = [0u8; 96];
        preimage[..32].copy_from_slice(&[1; 32]);
        preimage[32..64].copy_from_slice(&[7; 32]);
        preimage[64..].copy_from_slice(&[8; 32]);
        let expected: [u8; 32] = Sha256::digest(preimage).into();
        assert_eq!(ata_seed(&program, &owner, &definition), expected);
        assert_ne!(
            ata_seed(&program, &definition, &owner),
            expected,
            "owner and definition are not interchangeable"
        );
    }

    #[test]
    fn fee_switch_capped() {
        check_rate(MAX_FEE_BPS).unwrap();
        assert_eq!(check_rate(MAX_FEE_BPS + 1), Err(VestingError::FeeAboveCap));
        assert_eq!(fee_for(1_000, 101), Err(VestingError::FeeAboveCap));
        assert_eq!(
            check_fee(1_000, 0, &TREASURY, 10_000, &TREASURY),
            Err(VestingError::FeeAboveCap)
        );
        assert_eq!(VestingError::FeeAboveCap.code(), 28);
    }
}
