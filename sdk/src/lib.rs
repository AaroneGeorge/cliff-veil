//! Client SDK for the `vesting` program (cliff-veil) on LEZ v0.4.0-rc3.
//!
//! It covers the whole lifecycle of a schedule: create (one, or a batch split into transactions that fit), read
//! (vested, claimable now, locked, next unlock), claim to a public account or to a private account, cancel, make
//! non-cancelable, signal a milestone, transfer the beneficiary, and read the fee configuration.
//!
//! # How it is put together
//!
//! | Module | What it holds |
//! |---|---|
//! | [`client`] | [`Client`]: `prepare_*` (checks with the `vesting_core` rules, builds, dry-runs, checks the fee payer) and [`Client::send`] |
//! | [`batch`] | the cycle model that splits a batch into transactions that fit the execution budget |
//! | [`tx`] | [`Programs`] and the transactions of SPEC section 3.2, root first |
//! | [`list`] | the block scan that lists schedules by creator or beneficiary |
//! | [`backend`] | the [`Backend`] trait: what the SDK needs from a chain |
//! | [`sequencer`] | [`SequencerBackend`]: LEZ's wallet against a sequencer's RPC |
//! | [`memory`] | [`MemoryBackend`]: an in-memory chain running the same rules, for tests |
//! | [`errors`] | [`Error`], with a message and an action for every program error code |
//! | [`disclosure`] | the privacy disclosure shown before every private claim |
//! | [`time`] | ISO-8601 UTC rendering of clock times |
//!
//! # Example (in memory)
//!
//! The same code runs as the unit test `doc_example_runs`.
//!
//! ```ignore
//! use cliff_veil_sdk::{Client, ClaimTarget, MemoryBackend, Programs, linear_terms};
//! use lee_core::account::AccountId;
//! use vesting_core::Asset;
//!
//! let (creator, alice) = (AccountId::new([1; 32]), AccountId::new([2; 32]));
//! let mut chain = MemoryBackend::new(Programs::new(AccountId::new([9; 32])), 0);
//! chain.add_public(creator, 10_000);
//! chain.add_public(alice, 100);
//! let mut client = Client::new(chain);
//!
//! // 1 000 units over 1 000 ms, 250 ms cliff.
//! let create = client.prepare_create(creator, [7; 32], linear_terms(alice, Asset::Native, 1_000, 0, 250, 1_000)).await.unwrap();
//! client.send(&create).await.unwrap();
//! let schedule = client.programs().schedule_account(creator, &[7; 32]);
//!
//! client.backend_mut().now = 500;
//! assert_eq!(client.claimable(schedule).await.unwrap(), 500);
//! let claim = client.prepare_claim(alice, schedule, ClaimTarget::Public(alice)).await.unwrap();
//! client.send(&claim).await.unwrap();
//! assert_eq!(client.status(schedule).await.unwrap().state.claimed, 500);
//! ```

pub mod backend;
pub mod batch;
pub mod client;
pub mod disclosure;
pub mod errors;
pub mod list;
pub mod memory;
pub mod sequencer;
pub mod time;
pub mod tx;

pub use backend::{AccountRecord, Backend, Estimate, FeeSettings, Receipt, RootCallSeen};
pub use client::{
    ClaimSummary, ClaimTarget, Client, Destination, FeePayerChoice, Outcome, Prepared, ScheduleView, Standing,
    linear_terms, milestone_terms, schedule_id_from_label,
};
pub use disclosure::{DEV_MODE_NOTE, PRIVACY_DISCLOSURE, dev_mode, privacy_disclosure};
pub use errors::{ChainFailure, Error, Result};
pub use list::{ListFilter, ScanCache};
pub use memory::MemoryBackend;
pub use sequencer::{DEFAULT_FEE_HEADROOM_BLOCKS, SequencerBackend};
pub use tx::{Programs, Route, Tx, core_id, lez_id};
pub use vesting_core::{self, STATE_VERSION};

#[cfg(test)]
mod tests;
