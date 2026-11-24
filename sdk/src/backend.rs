//! What the SDK needs from a chain: [`Backend`].
//!
//! Two implementations: [`crate::SequencerBackend`] (LEZ's wallet against a sequencer's RPC) and
//! [`crate::MemoryBackend`] (an in-memory ledger that runs the `vesting_core` rules, for tests and for offline use).
//! The [`crate::Client`] does every check itself and calls only these methods, so both backends give the same
//! answers to the same questions.

use std::fmt;

use lee_core::account::AccountId;
use vesting_core::{Asset, ScheduleV1};

use crate::errors::Result;
use crate::tx::{Programs, Tx};

/// What the wallet's account record says about an account (Privacy 3 reads this).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountRecord {
    /// A public account whose signing key the wallet holds.
    PublicOwned,
    /// A private (shielded) account whose keys the wallet holds: it can take part in a proof.
    PrivateOwned,
    /// A private account shared with others (group keys); not used as a claim destination by the SDK.
    PrivateShared,
    /// The wallet holds no key for it.
    NotOwned,
}

impl fmt::Display for AccountRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PublicOwned => "the wallet records it as a public account",
            Self::PrivateOwned => "the wallet records it as an owned private account",
            Self::PrivateShared => "the wallet records it as a shared private account",
            Self::NotOwned => "the wallet holds no key for it",
        })
    }
}

/// The fee configuration of `vesting_fees` (SPEC 3.4, 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeSettings {
    /// Creation fee rate in basis points (0 at deployment, at most `MAX_FEE_BPS`).
    pub rate_bps: u16,
    /// The treasury (all zero until set).
    pub treasury: AccountId,
    /// `false` when no `vesting_fees` program is configured (then no fee is charged or quoted).
    pub deployed: bool,
}

/// The network fee of a transaction, from a dry run at current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    /// The fee at the current base fees.
    pub fee: u128,
    /// What the payer must hold for the transaction to be admitted (the wallet's reserve; at least `fee`).
    pub reserve: u128,
    /// Who pays it: the signer (public), or a private account inside the proof (private).
    pub payer: AccountId,
    /// `true` when the fee is paid inside a privacy-preserving transaction.
    pub private: bool,
    /// Execution cycles of the (public part of the) transaction.
    pub cycles: u64,
}

/// A transaction that was included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// Transaction hash (hex).
    pub hash: String,
    /// The block that holds it.
    pub block: u64,
    /// The fee the send declared (public: the `max_fee` cap; private: the reserve), when known.
    pub declared_fee: Option<u128>,
}

/// The public root call of one transaction in a block: what anyone can read from the chain (SPEC 11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootCallSeen {
    /// The block that holds the transaction.
    pub block: u64,
    /// The account of the root actor (for a vesting message: the caller).
    pub account: AccountId,
    /// The program of the root actor.
    pub program: AccountId,
    /// The encoded root message.
    pub message: Vec<u8>,
    /// `true` when the transaction was included as reverted (it changed nothing but its fee).
    pub reverted: bool,
}

/// A chain the SDK can read and send to.
///
/// `async fn` in a public trait: callers use a concrete backend type (`Client<SequencerBackend>`), so the futures'
/// auto traits are those of the backend.
#[allow(async_fn_in_trait)]
pub trait Backend {
    /// The program ids.
    fn programs(&self) -> &Programs;

    /// The current clock time (ms): the last block's timestamp, the time a transaction in the next block reads
    /// (SPEC 4.1).
    async fn now(&self) -> Result<u64>;

    /// The schedule at `account`, `None` when the actor is empty.
    async fn schedule(&self, account: AccountId) -> Result<Option<ScheduleV1>>;

    /// The public native balance of `account` (0 when it does not exist).
    async fn native_balance(&self, account: AccountId) -> Result<u128>;

    /// `true` when `account` can receive `asset`: always for native; for a token, when it holds a holding of the
    /// definition.
    async fn can_receive(&self, account: AccountId, asset: &Asset) -> Result<bool>;

    /// `true` when `account` exists in public state.
    async fn exists(&self, account: AccountId) -> Result<bool>;

    /// The `vesting_fees` configuration.
    async fn fee_config(&self) -> Result<FeeSettings>;

    /// The id of the latest block.
    async fn last_block(&self) -> Result<u64>;

    /// The public root calls of every transaction in blocks `from..=to`, in order. A privacy-preserving
    /// transaction whose proof reveals no public root call is left out.
    async fn root_calls(&self, from: u64, to: u64) -> Result<Vec<RootCallSeen>>;

    /// The wallet's record of `account`.
    fn account_record(&self, account: AccountId) -> AccountRecord;

    /// The native balance of an owned private account, as the wallet last synced it.
    fn private_native_balance(&self, account: AccountId) -> Option<u128>;

    /// A fresh owned private account (stored in the wallet).
    async fn new_private_account(&mut self) -> Result<AccountId>;

    /// 32 fresh random bytes (schedule ids, blinding openings).
    fn random_bytes(&mut self) -> [u8; 32];

    /// Runs `tx` against current state without sending it and prices it. A refusal is returned as the error the
    /// program or LEZ gives.
    async fn estimate(&mut self, tx: &Tx) -> Result<Estimate>;

    /// Sends `tx` and waits until it is included.
    async fn submit(&mut self, tx: &Tx) -> Result<Receipt>;
}
