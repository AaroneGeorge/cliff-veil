//! `vesting_fees`: the creation-fee switch of cliff-veil (SPEC sections 3.4 and 6).
//!
//! One config account, a PDA of the program and [`CONFIG_SEED`], holds the fee rate in basis points (0 after
//! `Init`), the treasury, and the admin slot of the RFP-001 admin-authority library (`lez-admin-authority` v0.2.0).
//! The rate can never exceed [`MAX_FEE_BPS`] (1%): `SetFee` refuses a higher rate with error 28, and no message
//! raises the cap.
//!
//! The program's messages ([`FeeMessage`]) are encoded exactly as [`vesting_core::FeeMessage`], which `vesting`
//! sends:
//! - `CheckFee { total, fee, treasury, asset }` (tag 0): anyone, read-only, answered at the config actor. `vesting`
//!   sends it in every creation, before it pays the fee; the check and the payment are one transaction, so a
//!   mismatch (error 27) reverts both. `treasury` is the treasury owner account: for a token fee `vesting` derives
//!   the destination (the owner's associated token account) itself, with `vesting_core::ata_seed`.
//! - `SetFee { rate_bps, treasury }` (tag 1): admin only, sent from the admin's own actor. Sets the rate and the
//!   treasury and emits [`FeeChanged`]. There is no other message.
//!
//! The admin's own operations (`Init`, `Nominate`, `Accept`, `Renounce`) are the library's, sent as
//! [`lez_admin_authority::admin_op`]. `Init` installs the deployer (the program's header account) as admin on the
//! default config: rate 0, treasury all zero.
//!
//! **Before `Init`.** The library refuses every message on an empty config (its E101). `CheckFee` is answered before
//! the library's guard instead, and reads an empty config as rate 0: a creation with fee 0 passes and any other fee
//! is refused, so deploying the program never blocks creation and never lets a fee through unchecked.
//!
//! **After `Renounce`.** The rate and treasury are frozen for good; `CheckFee` keeps answering, so creation goes on.

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::account::{AccountId, Actor};
use lee_core::program::{PdaSeed, ProgramEvent, ReceiveInput, Response};
use lez_admin_authority::{Admin, AuthoritySlot, Gated, HasSlot, Wire};
use vesting_core::{AccountId as CoreAccountId, Asset, Event, FeeChanged, VestingError};

pub use vesting_core::{BPS_DENOMINATOR, MAX_FEE_BPS};

/// Seed of the config account: `lez_admin_authority::seed(b"cliff-veil/fees/v1")` (the label, zero-padded to 32
/// bytes).
pub const CONFIG_SEED: PdaSeed = lez_admin_authority::seed(b"cliff-veil/fees/v1");

/// The config account of a `vesting_fees` deployment at `program`.
#[must_use]
pub fn config_account(program: AccountId) -> AccountId {
    lez_admin_authority::config_account(program, &CONFIG_SEED)
}

/// The config actor of a `vesting_fees` deployment at `program`: the only actor that answers `CheckFee`.
#[must_use]
pub fn config_actor(program: AccountId) -> Actor {
    Actor::new(config_account(program), program)
}

/// The state of the config actor (Borsh, SPEC 3.4): `{ rate_bps, treasury, admin }`.
///
/// The default is the state `Init` writes: rate 0, treasury all zero, the admin installed by the library.
#[derive(Debug, Clone, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize, HasSlot)]
pub struct FeeConfig {
    /// Fee rate in basis points, `0..=MAX_FEE_BPS`.
    pub rate_bps: u16,
    /// The treasury: the owner of the fee destination (SPEC 6).
    pub treasury: AccountId,
    /// The admin slot of the RFP-001 admin authority.
    #[slot(Admin)]
    pub admin: AuthoritySlot,
}

/// The messages of `vesting_fees` (SPEC 3.4): the variants, tags and field order of [`vesting_core::FeeMessage`],
/// the encoding `vesting` sends, with the library's guard attribute added. A test checks both encode every variant
/// to the same bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize, Gated)]
pub enum FeeMessage {
    /// Tag 0, anyone, read-only, answered only at the config actor: refuses (27) unless `fee` is the configured
    /// rate's fee on `total` and, when `fee > 0`, `treasury` is the configured treasury.
    CheckFee {
        /// The schedule total, or the sum of item totals of a batch.
        total: u128,
        /// The quoted fee.
        fee: u128,
        /// The treasury account named by the creation message (never its associated token account).
        treasury: CoreAccountId,
        /// The asset the fee is paid in. Version 1 applies one rate to every asset.
        asset: Asset,
    },
    /// Tag 1, admin only: sets the rate and the treasury. Refuses a rate above [`MAX_FEE_BPS`] (28). Emits
    /// [`FeeChanged`].
    #[only(Admin)]
    SetFee {
        /// The new rate in basis points.
        rate_bps: u16,
        /// The new treasury.
        treasury: CoreAccountId,
    },
}

impl From<vesting_core::FeeMessage> for FeeMessage {
    fn from(message: vesting_core::FeeMessage) -> Self {
        match message {
            vesting_core::FeeMessage::CheckFee {
                total,
                fee,
                treasury,
                asset,
            } => Self::CheckFee {
                total,
                fee,
                treasury,
                asset,
            },
            vesting_core::FeeMessage::SetFee { rate_bps, treasury } => Self::SetFee { rate_bps, treasury },
        }
    }
}

/// A program message on the wire (what a client sends and the guest decodes).
#[must_use]
pub fn wire(message: FeeMessage) -> Wire<FeeMessage> {
    Wire::Program(message)
}

impl FeeConfig {
    /// `CheckFee`: whether a creation quoted `fee` on `total` and named `treasury`. A zero fee needs no treasury.
    /// Version 1 applies one rate to every asset; `asset` is carried for later versions.
    ///
    /// # Errors
    /// [`VestingError::FeeMismatch`] (27) on a wrong amount, or a non-zero fee with another treasury.
    pub fn check(&self, _asset: &Asset, total: u128, fee: u128, treasury: AccountId) -> Result<(), VestingError> {
        vesting_core::check_fee(total, fee, &core_id(treasury), self.rate_bps, &core_id(self.treasury))
    }
}

/// Entry point of the guest: `lee_core::actor_entrypoint!(vesting_fees::handle_message)`.
///
/// `CheckFee` is answered here, before the admin guard, so that an empty config reads as rate 0 rather than the
/// library's E101; it needs no role and writes nothing. Every other message goes through the guard.
pub fn handle_message(input: &ReceiveInput, message: Wire<FeeMessage>) -> Response {
    match message {
        Wire::Program(FeeMessage::CheckFee {
            total,
            fee,
            treasury,
            asset,
        }) => check_fee(input, &asset, total, fee, lee_id(treasury)),
        other => guarded(input, other),
    }
}

/// `CheckFee` at the config actor. Refused (27) anywhere else: another actor of this program holds no config, and
/// reading its empty state as rate 0 would let any fee through.
fn check_fee(input: &ReceiveInput, asset: &Asset, total: u128, fee: u128, treasury: AccountId) -> Response {
    if input.receiver != config_actor(input.receiver.program_account_id) {
        refuse(VestingError::FeeMismatch);
    }
    let config = read_config(&input.pre_state);
    if let Err(error) = config.check(asset, total, fee, treasury) {
        refuse(error);
    }
    Response::keep_state()
}

/// The config in `state`; an empty state (before `Init`) is the default config, rate 0.
///
/// # Panics
/// When the state does not decode (the guard writes only configs that do).
#[must_use]
pub fn read_config(state: &[u8]) -> FeeConfig {
    if state.is_empty() {
        return FeeConfig::default();
    }
    borsh::from_slice(state).unwrap_or_else(|_| panic!("vesting_fees: the config account does not decode"))
}

/// The admin's messages. The guard has already checked, at the config actor, that the message came through the
/// admin's own actor from the admin, and refuses the result if the admin slot changed.
#[lez_admin_authority::admin(config = FeeConfig, seed = CONFIG_SEED)]
fn guarded(input: &ReceiveInput, message: FeeMessage) -> Response {
    let (rate_bps, treasury) = match message {
        // Answered before the guard; kept here so the match is total.
        FeeMessage::CheckFee {
            total,
            fee,
            treasury,
            asset,
        } => return check_fee(input, &asset, total, fee, lee_id(treasury)),
        FeeMessage::SetFee { rate_bps, treasury } => (rate_bps, treasury),
    };
    if let Err(error) = vesting_core::check_rate(rate_bps) {
        refuse(error);
    }
    let mut config = read_config(&input.pre_state);
    let old_rate_bps = config.rate_bps;
    config.rate_bps = rate_bps;
    config.treasury = lee_id(treasury);
    // Through the admin's seat, `from` is the admin's own actor.
    let by = input.from.map(|from| from.account_id).unwrap_or_default();
    let event = FeeChanged {
        old_rate_bps,
        new_rate_bps: rate_bps,
        treasury,
        by: core_id(by),
    };
    let bytes = borsh::to_vec(&config).unwrap_or_else(|_| panic!("vesting_fees: the config encodes"));
    Response::set_state(bytes).event(ProgramEvent {
        selector: FeeChanged::SELECTOR,
        data: event.data(),
    })
}

fn core_id(id: AccountId) -> CoreAccountId {
    CoreAccountId::new(id.into_value())
}

fn lee_id(id: CoreAccountId) -> AccountId {
    AccountId::new(id.into_value())
}

/// Fails the transaction with `cliff-veil error <code>: <message>`.
fn refuse(error: VestingError) -> ! {
    panic!("{error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREASURY: AccountId = AccountId::new([7; 32]);
    const OTHER: AccountId = AccountId::new([9; 32]);

    fn config(rate_bps: u16) -> FeeConfig {
        FeeConfig {
            rate_bps,
            treasury: TREASURY,
            admin: AuthoritySlot::Unset,
        }
    }

    fn token() -> Asset {
        Asset::Token {
            definition: vesting_core::AccountId::new([0xde; 32]),
        }
    }

    /// Every variant of the shared enum, with distinct field values.
    fn every_shared_message() -> [vesting_core::FeeMessage; 3] {
        [
            vesting_core::FeeMessage::CheckFee {
                total: 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10,
                fee: 0x1112_1314_1516_1718_191a_1b1c_1d1e_1f20,
                treasury: core_id(TREASURY),
                asset: Asset::Native,
            },
            vesting_core::FeeMessage::CheckFee {
                total: 1_000_000,
                fee: 5_000,
                treasury: core_id(OTHER),
                asset: token(),
            },
            vesting_core::FeeMessage::SetFee {
                rate_bps: 0x0164,
                treasury: core_id(TREASURY),
            },
        ]
    }

    #[test]
    fn fee_message_encodes_exactly_as_the_shared_message() {
        for shared in every_shared_message() {
            let ours = FeeMessage::from(shared);
            let bytes = shared.encode();
            assert_eq!(borsh::to_vec(&ours).unwrap(), bytes, "{shared:?}");
            assert_eq!(borsh::to_vec(&wire(ours)).unwrap(), bytes, "{shared:?} on the wire");
            // What `vesting` sends decodes here as the same message, and back.
            let back: Wire<FeeMessage> = borsh::from_slice(&bytes).unwrap();
            assert_eq!(back, wire(ours));
            assert_eq!(
                vesting_core::FeeMessage::decode(&borsh::to_vec(&ours).unwrap()),
                Ok(shared)
            );
        }
        assert_eq!(every_shared_message()[0].encode()[0], 0, "CheckFee is tag 0");
        assert_eq!(
            every_shared_message()[2].encode()[..3],
            [1, 0x64, 0x01],
            "SetFee is tag 1"
        );
    }

    #[test]
    fn fee_only_set_fee_needs_the_admin() {
        let [check, _, set] = every_shared_message().map(FeeMessage::from);
        assert!(set.needs::<Admin>());
        assert!(!check.needs::<Admin>());
        assert!(!set.only_from_config() && !check.only_from_config());
    }

    #[test]
    fn fee_config_seed_is_the_padded_label() {
        let mut expected = [0u8; 32];
        expected[..18].copy_from_slice(b"cliff-veil/fees/v1");
        assert_eq!(CONFIG_SEED, PdaSeed::new(expected));
    }

    #[test]
    fn fee_config_layout_is_rate_treasury_admin() {
        let bytes = borsh::to_vec(&config(42)).unwrap();
        assert_eq!(bytes[..2], 42u16.to_le_bytes());
        assert_eq!(bytes[2..34], *TREASURY.value());
        assert_eq!(bytes[34..], borsh::to_vec(&AuthoritySlot::Unset).unwrap());
    }

    #[test]
    fn fee_empty_state_is_rate_zero() {
        let empty = read_config(&[]);
        assert_eq!(empty, FeeConfig::default());
        empty.check(&Asset::Native, 1_000_000, 0, OTHER).unwrap();
        assert_eq!(
            empty.check(&Asset::Native, 1_000_000, 1, TREASURY),
            Err(VestingError::FeeMismatch)
        );
    }

    #[test]
    fn fee_check_matches_rate_and_treasury() {
        config(100).check(&Asset::Native, 1_000_000, 10_000, TREASURY).unwrap();
        config(100).check(&token(), 1_000_000, 10_000, TREASURY).unwrap();
        assert_eq!(
            config(100).check(&Asset::Native, 1_000_000, 9_999, TREASURY),
            Err(VestingError::FeeMismatch)
        );
        assert_eq!(
            config(100).check(&Asset::Native, 1_000_000, 10_000, OTHER),
            Err(VestingError::FeeMismatch)
        );
        // A fee that rounds to zero needs no treasury.
        config(50).check(&Asset::Native, 199, 0, OTHER).unwrap();
        config(0).check(&token(), 1_000_000, 0, OTHER).unwrap();
    }

    #[test]
    fn fee_refusal_texts_carry_the_stable_codes() {
        assert_eq!(
            VestingError::FeeMismatch.to_string(),
            "cliff-veil error 27: the creation fee does not match the current rate or treasury"
        );
        assert!(
            VestingError::FeeAboveCap
                .to_string()
                .starts_with("cliff-veil error 28: ")
        );
    }
}
