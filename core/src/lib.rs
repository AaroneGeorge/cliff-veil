//! Vesting rules shared by the `vesting` program and the SDK.
//!
//! This crate is the single source of truth for everything in `SPEC.md` that is pure computation: the schedule
//! state layout (section 2), the message encodings (section 3), the vesting arithmetic (section 4), the error codes
//! (section 5), the fee formula (section 6) and the event schemas (section 7). The program runs these functions on
//! chain and the SDK runs the same functions for its pre-flight checks, so both always agree on an amount.
//!
//! The crate is `no_std` (it uses `alloc` for `Vec`), has no LEZ dependency and never panics on any input: every
//! arithmetic step is checked and a failure is a [`VestingError`] with a stable code.
//!
//! # Map
//!
//! | Module | SPEC | What it holds |
//! |---|---|---|
//! | [`state`] | 2 | [`ScheduleState`], [`ScheduleV1`], [`Kind`], [`Asset`], [`Cancellation`], [`Pending`], [`AccountId`] |
//! | [`rules`] | 4 | `vested`, `claimable`, `next_unlock` and every state transition (`apply_*`) |
//! | [`create`] | 3.1 | [`Terms`], batch terms, creation and batch validation, the schedule seed |
//! | [`message`] | 3.1, 3.3 | [`Message`] and [`Internal`] |
//! | [`error`] | 5 | [`VestingError`] and the code table |
//! | [`fees`] | 3.4, 6 | fee formula, checks and the `vesting_fees` message |
//! | [`events`] | 7 | event structs and selectors |
//! | [`math`] | 4 | [`mul_div_floor`] |
//!
//! # Example
//!
//! ```
//! use vesting_core::{AccountId, Asset, Kind, Terms};
//!
//! let creator = AccountId::new([1; 32]);
//! let terms = Terms {
//!     beneficiary: AccountId::new([2; 32]),
//!     asset: Asset::Native,
//!     total: 1_000,
//!     // One year in milliseconds with a three-month cliff.
//!     kind: Kind::Linear { start: 0, cliff: 7_776_000_000, end: 31_536_000_000 },
//!     cancelable: true,
//!     transferable: false,
//!     cancel_authority: None,
//!     milestone_authority: None,
//!     refund_to: None,
//! };
//! let mut schedule = terms.into_state(creator, [7; 32], creator).unwrap();
//! assert_eq!(schedule.claimable(7_775_999_999).unwrap(), 0); // before the cliff
//! assert_eq!(schedule.claimable(7_776_000_000).unwrap(), 246); // the cliff lump: floor(1000 * 90 / 365)
//! let paid = schedule.apply_claim(31_536_000_000).unwrap();
//! assert_eq!(paid.amount, 1_000);
//! ```

#![no_std]

extern crate alloc;

pub mod create;
pub mod error;
pub mod events;
pub mod fees;
pub mod math;
pub mod message;
pub mod rules;
pub mod state;

pub use create::{
    BatchItem, BatchShape, BatchTerms, MAX_BATCH_NATIVE, MAX_BATCH_TOKEN, SCHEDULE_SEED_PREFIX, Terms,
    batch_milestone_amounts, max_batch, schedule_seed, schedule_seed_preimage, validate_batch,
};
pub use error::{ERRORS, ErrorInfo, NextUnlock, VestingError, parse_code};
pub use events::{
    BeneficiaryTransferred, Cancelled, Claimed, Event, FeeChanged, MadeNonCancelable, MilestoneSignalled,
    ScheduleCreated, event_selector,
};
pub use fees::{BPS_DENOMINATOR, FeeMessage, MAX_FEE_BPS, ata_seed, check_fee, check_fee_cap, check_rate, fee_for};
pub use math::mul_div_floor;
pub use message::{Internal, Message, Role};
pub use rules::{ClaimOutcome, SignalOutcome, check_destination};
pub use state::{AccountId, Asset, Cancellation, Kind, MAX_MILESTONES, Pending, ScheduleState, ScheduleV1};

/// Version of the schedule state layout this crate reads and writes (the [`ScheduleState`] variant it produces).
pub const STATE_VERSION: u8 = 1;

/// `Result` with a [`VestingError`].
pub type Result<T> = core::result::Result<T, VestingError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_version_is_one() {
        assert_eq!(STATE_VERSION, 1);
    }
}
