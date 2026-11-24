//! An in-memory chain for tests and offline use: [`MemoryBackend`].
//!
//! It keeps public native balances, token holdings, schedules and the wallet's private accounts in maps, and runs
//! every vesting message with the same `vesting_core` functions the program runs, in the order SPEC 3.3 gives.
//! A refusal is the program's own refusal text (`cliff-veil error <code>: ...`), so the SDK reads it back through the
//! same parser it uses for a sequencer. A transaction either applies completely or changes nothing.
//!
//! What it does not model: proofs, gas metering (each transaction costs a fixed fee), the token program's own
//! rules beyond "a holding must exist to be credited", and block inclusion delays (the clock is set by the test).

use std::collections::{BTreeMap, BTreeSet};

use lee_core::account::{AccountId, Actor};
use vesting_core::{Asset, Internal, Message, ScheduleV1, VestingError};

use crate::backend::{AccountRecord, Backend, Estimate, FeeSettings, Receipt, RootCallSeen};
use crate::errors::{Error, Result};
use crate::tx::{Programs, Route, Tx, core_id, lez_id};

/// One transaction as an outside observer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentTx {
    /// The encoded root message (public for every transaction, privacy-preserving ones included).
    pub message: Vec<u8>,
    /// The public actors it declared.
    pub public_actors: Vec<Actor>,
    /// `true` when it was privacy-preserving.
    pub private: bool,
}

/// Balances and schedules; cloned for each transaction so a failure changes nothing.
#[derive(Debug, Clone, Default)]
struct Ledger {
    native: BTreeMap<AccountId, u128>,
    /// `(account, definition) -> amount`: a token holding exists when its key is present.
    tokens: BTreeMap<(AccountId, AccountId), u128>,
    schedules: BTreeMap<AccountId, ScheduleV1>,
    /// Owned private accounts and their native balance.
    private: BTreeMap<AccountId, u128>,
    /// Token amounts credited to private accounts, `(account, definition)`.
    private_tokens: BTreeMap<(AccountId, AccountId), u128>,
}

/// The in-memory chain and wallet.
#[derive(Debug, Clone)]
pub struct MemoryBackend {
    programs: Programs,
    /// The clock time (ms) transactions read.
    pub now: u64,
    /// Network fee of a public transaction, paid by the signer.
    pub public_fee: u128,
    /// Network fee of a privacy-preserving transaction, paid inside the proof by a private account.
    pub private_fee: u128,
    /// The `vesting_fees` configuration.
    pub fees: FeeSettings,
    ledger: Ledger,
    owned_public: BTreeSet<AccountId>,
    counter: u64,
    block: u64,
    sent: Vec<SentTx>,
    calls: Vec<RootCallSeen>,
}

impl MemoryBackend {
    /// An empty chain with `programs`, the clock at `now`, a public fee of 10 and a private fee of 50, and a zero
    /// creation fee.
    #[must_use]
    pub fn new(programs: Programs, now: u64) -> Self {
        Self {
            programs,
            now,
            public_fee: 10,
            private_fee: 50,
            fees: FeeSettings {
                rate_bps: 0,
                treasury: AccountId::new([0; 32]),
                deployed: programs.fees.is_some(),
            },
            ledger: Ledger::default(),
            owned_public: BTreeSet::new(),
            counter: 0,
            block: 0,
            sent: Vec::new(),
            calls: Vec::new(),
        }
    }

    /// Adds a public account the wallet holds the key of, with `balance` native units.
    pub fn add_public(&mut self, account: AccountId, balance: u128) {
        self.owned_public.insert(account);
        self.ledger.native.insert(account, balance);
    }

    /// Sets the native balance of a public account the wallet does not own (it then exists on chain).
    pub fn set_native(&mut self, account: AccountId, balance: u128) {
        self.ledger.native.insert(account, balance);
    }

    /// Adds an owned private account with `balance` native units.
    pub fn add_private(&mut self, account: AccountId, balance: u128) {
        self.ledger.private.insert(account, balance);
    }

    /// Gives `account` a holding of token `definition` with `amount`.
    pub fn set_token(&mut self, account: AccountId, definition: AccountId, amount: u128) {
        self.ledger.tokens.insert((account, definition), amount);
    }

    /// The token amount `account` holds of `definition`, `None` without a holding.
    #[must_use]
    pub fn token(&self, account: AccountId, definition: AccountId) -> Option<u128> {
        self.ledger.tokens.get(&(account, definition)).copied()
    }

    /// Public native balance (0 when the account does not exist).
    #[must_use]
    pub fn native(&self, account: AccountId) -> u128 {
        self.ledger.native.get(&account).copied().unwrap_or(0)
    }

    /// Native balance of an owned private account.
    #[must_use]
    pub fn private(&self, account: AccountId) -> Option<u128> {
        self.ledger.private.get(&account).copied()
    }

    /// Every transaction sent so far, as an outside observer sees it.
    #[must_use]
    pub fn sent(&self) -> &[SentTx] {
        &self.sent
    }

    /// Records a reverted transaction's root call in the next block (a creation that changed nothing), for scan
    /// tests.
    pub fn record_reverted(&mut self, tx: &Tx) {
        self.block += 1;
        if let Some(root) = tx.accounts.first() {
            self.calls.push(RootCallSeen {
                block: self.block,
                account: root.identity.account_id(),
                program: root.program_account_id,
                message: tx.message.encode(),
                reverted: true,
            });
        }
    }

    /// Every public account with a native balance (what public state shows).
    #[must_use]
    pub fn public_accounts(&self) -> Vec<AccountId> {
        self.ledger.native.keys().copied().collect()
    }

    fn next(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Runs `tx` on a copy of the ledger; with `charge` the network fee is taken too.
    fn run(&self, tx: &Tx, charge: bool) -> std::result::Result<Ledger, String> {
        let mut l = self.ledger.clone();
        Exec {
            l: &mut l,
            programs: &self.programs,
            fees: self.fees,
            now: self.now,
            tx,
        }
        .message()?;
        if charge {
            match tx.route {
                Route::Public => debit(&mut l.native, tx.signer, self.public_fee)?,
                Route::Private {
                    destination, fee_payer, ..
                } => {
                    debit(&mut l.private, fee_payer.unwrap_or(destination), self.private_fee)?;
                }
            }
        }
        Ok(l)
    }
}

fn debit(map: &mut BTreeMap<AccountId, u128>, account: AccountId, amount: u128) -> std::result::Result<(), String> {
    let have = map.get(&account).copied().unwrap_or(0);
    let left = have
        .checked_sub(amount)
        .ok_or_else(|| format!("InsufficientBalance {{ account_id: {account} }}: Can not pay for operation"))?;
    map.insert(account, left);
    Ok(())
}

fn credit(map: &mut BTreeMap<AccountId, u128>, account: AccountId, amount: u128) {
    let entry = map.entry(account).or_insert(0);
    *entry = entry.saturating_add(amount);
}

/// One message's execution against a ledger.
struct Exec<'a> {
    l: &'a mut Ledger,
    programs: &'a Programs,
    fees: FeeSettings,
    now: u64,
    tx: &'a Tx,
}

type Run = std::result::Result<(), String>;

fn refuse(e: VestingError) -> String {
    e.to_string()
}

impl Exec<'_> {
    fn message(&mut self) -> Run {
        let creator = self.tx.signer;
        match self.tx.message.clone() {
            Message::Create {
                schedule_id,
                terms,
                source,
                fee,
                treasury,
            } => {
                let asset = terms.asset;
                let state = terms
                    .into_state(core_id(creator), schedule_id, source)
                    .map_err(refuse)?;
                self.fee(state.total, fee, treasury)?;
                self.open(state, lez_id(source))?;
                self.fund(lez_id(source), fee, treasury, &asset)
            }
            Message::CreateBatch {
                common,
                items,
                source,
                fee,
                treasury,
            } => {
                let (states, sum) =
                    vesting_core::validate_batch(core_id(creator), source, &common, &items).map_err(refuse)?;
                self.fee(sum, fee, treasury)?;
                for state in states {
                    self.open(state, lez_id(source))?;
                }
                self.fund(lez_id(source), fee, treasury, &common.asset)
            }
            other => {
                let (schedule, internal) = other.forward().ok_or_else(|| refuse(VestingError::InvalidMessage))?;
                self.forwarded(lez_id(schedule), internal)
            }
        }
    }

    /// `Create`'s fee checks: the hard cap (28), then `CheckFee` (27).
    fn fee(&self, total: u128, fee: u128, treasury: vesting_core::AccountId) -> Run {
        vesting_core::check_fee_cap(total, fee).map_err(refuse)?;
        let rate = if self.fees.deployed { self.fees.rate_bps } else { 0 };
        vesting_core::check_fee(total, fee, &treasury, rate, &core_id(self.fees.treasury)).map_err(refuse)
    }

    /// `Open` at the derived account (7, 26), its escrow funded with the total from `source`.
    fn open(&mut self, state: ScheduleV1, source: AccountId) -> Run {
        let account = self.programs.schedule_account(self.tx.signer, &state.schedule_id);
        if self.l.schedules.contains_key(&account) {
            return Err(refuse(VestingError::ScheduleExists));
        }
        vesting_core::check_destination(&core_id(account), &state.refund_to).map_err(refuse)?;
        self.move_asset(source, account, state.total, &state.asset)?;
        self.l.schedules.insert(account, state);
        Ok(())
    }

    fn fund(&mut self, source: AccountId, fee: u128, treasury: vesting_core::AccountId, asset: &Asset) -> Run {
        if fee > 0 {
            self.move_asset(source, lez_id(treasury), fee, asset)?;
        }
        Ok(())
    }

    fn move_asset(&mut self, from: AccountId, to: AccountId, amount: u128, asset: &Asset) -> Run {
        match asset {
            Asset::Native => {
                debit(&mut self.l.native, from, amount)?;
                credit(&mut self.l.native, to, amount);
            }
            Asset::Token { definition } => {
                let d = lez_id(*definition);
                let have = self.l.tokens.get(&(from, d)).copied().unwrap_or(0);
                let left = have
                    .checked_sub(amount)
                    .ok_or_else(|| format!("token transfer: InsufficientBalance {{ account_id: {from} }}"))?;
                // The token program credits only an existing holding; a new schedule's escrow is initialised by the
                // program itself at creation.
                if !self.l.tokens.contains_key(&(to, d))
                    && !self.l.schedules.contains_key(&to)
                    && amount > 0
                    && !self.opening(to)
                {
                    return Err(format!("token transfer: the account {to} has no holding of {d}"));
                }
                self.l.tokens.insert((from, d), left);
                let entry = self.l.tokens.entry((to, d)).or_insert(0);
                *entry = entry.saturating_add(amount);
            }
        }
        Ok(())
    }

    /// `true` while `to` is a schedule account being opened by this transaction (its holding is initialised then).
    fn opening(&self, to: AccountId) -> bool {
        self.tx.schedules.contains(&to)
    }

    fn forwarded(&mut self, account: AccountId, internal: Internal) -> Run {
        let mut state = self
            .l
            .schedules
            .get(&account)
            .cloned()
            .ok_or_else(|| refuse(VestingError::NoSuchSchedule))?;
        let caller = core_id(self.tx.signer);
        if let Some(role) = internal.required_role() {
            state.authorise(role, &caller).map_err(refuse)?;
        }
        match internal {
            Internal::Open { .. } => return Err(refuse(VestingError::InvalidMessage)),
            Internal::ClaimFor { to } => {
                vesting_core::check_destination(&core_id(account), &to).map_err(refuse)?;
                let paid = state.apply_claim(self.now).map_err(refuse)?;
                self.payout(account, lez_id(to), paid.amount, &state.asset)?;
            }
            Internal::ClaimForAsOf { to, as_of } => {
                vesting_core::check_destination(&core_id(account), &to).map_err(refuse)?;
                if as_of > self.now {
                    return Err("Execution outside of the validity window".to_owned());
                }
                state.ensure_idle().map_err(refuse)?;
                let paid = state.apply_claim(as_of).map_err(refuse)?;
                self.payout(account, lez_id(to), paid.amount, &state.asset)?;
            }
            Internal::CancelFor => {
                state.check_cancel().map_err(refuse)?;
                let c = state.apply_cancel(self.now).map_err(refuse)?;
                if c.refunded > 0 {
                    self.payout(account, lez_id(state.refund_to), c.refunded, &state.asset)?;
                }
            }
            Internal::MakeNonCancelableFor => state.apply_make_non_cancelable().map_err(refuse)?,
            Internal::SignalFor { index } => {
                state.apply_signal_milestone(index).map_err(refuse)?;
            }
            Internal::TransferFor { new_beneficiary } => {
                state.apply_transfer_beneficiary(new_beneficiary).map_err(refuse)?;
            }
        }
        self.l.schedules.insert(account, state);
        Ok(())
    }

    /// Pays `amount` from the escrow of `schedule` to `to`: a public account, or the private destination that opens
    /// the blinded address `to` inside the proof.
    fn payout(&mut self, schedule: AccountId, to: AccountId, amount: u128, asset: &Asset) -> Run {
        if let Route::Private {
            destination, opening, ..
        } = self.tx.route
            && destination.blinded(&opening) == to
        {
            match asset {
                Asset::Native => {
                    debit(&mut self.l.native, schedule, amount)?;
                    credit(&mut self.l.private, destination, amount);
                }
                Asset::Token { definition } => {
                    let d = lez_id(*definition);
                    let have = self.l.tokens.get(&(schedule, d)).copied().unwrap_or(0);
                    self.l.tokens.insert((schedule, d), have.saturating_sub(amount));
                    let e = self.l.private_tokens.entry((destination, d)).or_insert(0);
                    *e = e.saturating_add(amount);
                }
            }
            return Ok(());
        }
        self.move_asset(schedule, to, amount, asset)
    }
}

impl Backend for MemoryBackend {
    fn programs(&self) -> &Programs {
        &self.programs
    }

    async fn now(&self) -> Result<u64> {
        Ok(self.now)
    }

    async fn schedule(&self, account: AccountId) -> Result<Option<ScheduleV1>> {
        Ok(self.ledger.schedules.get(&account).cloned())
    }

    async fn native_balance(&self, account: AccountId) -> Result<u128> {
        Ok(self.native(account))
    }

    async fn can_receive(&self, account: AccountId, asset: &Asset) -> Result<bool> {
        Ok(match asset {
            Asset::Native => true,
            Asset::Token { definition } => self.ledger.tokens.contains_key(&(account, lez_id(*definition))),
        })
    }

    async fn exists(&self, account: AccountId) -> Result<bool> {
        Ok(self.ledger.native.contains_key(&account) || self.ledger.tokens.keys().any(|(a, _)| *a == account))
    }

    async fn fee_config(&self) -> Result<FeeSettings> {
        Ok(self.fees)
    }

    async fn last_block(&self) -> Result<u64> {
        Ok(self.block)
    }

    async fn root_calls(&self, from: u64, to: u64) -> Result<Vec<RootCallSeen>> {
        Ok(self
            .calls
            .iter()
            .filter(|c| (from..=to).contains(&c.block))
            .cloned()
            .collect())
    }

    fn account_record(&self, account: AccountId) -> AccountRecord {
        if self.ledger.private.contains_key(&account) {
            AccountRecord::PrivateOwned
        } else if self.owned_public.contains(&account) {
            AccountRecord::PublicOwned
        } else {
            AccountRecord::NotOwned
        }
    }

    fn private_native_balance(&self, account: AccountId) -> Option<u128> {
        self.private(account)
    }

    async fn new_private_account(&mut self) -> Result<AccountId> {
        let n = self.next();
        let mut bytes = [0xA5; 32];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        let account = AccountId::new(bytes);
        self.ledger.private.insert(account, 0);
        Ok(account)
    }

    fn random_bytes(&mut self) -> [u8; 32] {
        let n = self.next();
        let mut bytes = [0x5A; 32];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        bytes
    }

    async fn estimate(&mut self, tx: &Tx) -> Result<Estimate> {
        self.run(tx, false).map_err(|e| Error::from_text(&e))?;
        Ok(match tx.route {
            Route::Public => Estimate {
                fee: self.public_fee,
                reserve: self.public_fee,
                payer: tx.signer,
                private: false,
                // A batch is priced with the SDK's own cycle model (the memory chain meters nothing).
                cycles: match &tx.message {
                    Message::CreateBatch { common, items, fee, .. } => crate::batch::estimate_cycles(
                        &common.asset,
                        crate::batch::milestones_of(&common.shape),
                        *fee > 0,
                        items.len() as u64,
                    ),
                    _ => 0,
                },
            },
            Route::Private {
                destination, fee_payer, ..
            } => Estimate {
                fee: self.private_fee,
                reserve: self.private_fee,
                payer: fee_payer.unwrap_or(destination),
                private: true,
                cycles: 0,
            },
        })
    }

    async fn submit(&mut self, tx: &Tx) -> Result<Receipt> {
        let ledger = self.run(tx, true).map_err(|e| Error::from_text(&e))?;
        self.ledger = ledger;
        self.block += 1;
        let public_actors = tx
            .accounts
            .iter()
            .filter(|m| !m.identity.is_private())
            .map(wallet::AccountMention::actor)
            .collect();
        if let Some(root) = tx.accounts.first() {
            self.calls.push(RootCallSeen {
                block: self.block,
                account: root.identity.account_id(),
                program: root.program_account_id,
                message: tx.message.encode(),
                reverted: false,
            });
        }
        self.sent.push(SentTx {
            message: tx.message.encode(),
            public_actors,
            private: tx.is_private(),
        });
        Ok(Receipt {
            hash: format!("{:064x}", self.block),
            block: self.block,
            declared_fee: Some(if tx.is_private() {
                self.private_fee
            } else {
                self.public_fee
            }),
        })
    }
}
