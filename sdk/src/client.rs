//! The full lifecycle of a schedule, each operation checked before it is built (RFP Usability 1, 4, 7; Privacy 3).
//!
//! Every operation is two steps:
//! 1. `prepare_*` reads current state, runs the same `vesting_core` rule the program runs on a copy of the schedule
//!    (so a refusal is reported before anything is signed, with its code and an action), builds the transaction of
//!    SPEC 3.2, dry-runs it on the backend to price it, and checks that whoever pays can pay. It returns a
//!    [`Prepared`]: the transaction, its fee estimate and the predicted outcome.
//! 2. [`Client::send`] sends a [`Prepared`] and waits for inclusion.
//!
//! A caller shows the [`Prepared`] (for a private claim: the summary and [`crate::privacy_disclosure`]) between the
//! two steps and asks for confirmation.

use lee_core::account::AccountId;
use sha2::Digest as _;
use vesting_core::{Asset, BatchItem, BatchTerms, Kind, NextUnlock, Role, ScheduleV1, Terms, max_batch};

use crate::backend::{AccountRecord, Backend, Estimate, FeeSettings, Receipt};
use crate::errors::{Error, Result};
use crate::list::{ListFilter, ScanCache};
use crate::time::utc;
use crate::tx::{Programs, Tx, core_id, lez_id};

/// Blocks read per `getBlockRange` call of a scan.
pub const SCAN_RANGE: u64 = 100;

/// A schedule id from a human label: `SHA-256(label)` (SPEC 1.2).
#[must_use]
pub fn schedule_id_from_label(label: &str) -> [u8; 32] {
    sha2::Sha256::digest(label.as_bytes()).into()
}

/// Where a claim pays to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimTarget {
    /// A public account (normally the beneficiary itself). A public `Claim`, computed at the clock time.
    Public(AccountId),
    /// A private account of this wallet, reached at a one-time blinded address (SPEC 9). `account: None` creates a
    /// fresh private account for this claim. `fee_payer: None` lets the destination pay the network fee out of a
    /// native claim; a token claim needs a private fee payer holding native balance.
    Private {
        /// The owned private account that receives the claim; `None`: a fresh one.
        account: Option<AccountId>,
        /// The owned private account that pays the network fee.
        fee_payer: Option<AccountId>,
    },
}

/// Where a prepared claim pays to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// A public account.
    Public(AccountId),
    /// An owned private account, reached at `blinded`.
    Private {
        /// The canonical id: known to this wallet only, never sent.
        account: AccountId,
        /// The one-time blinded address the transaction names.
        blinded: AccountId,
        /// `true` when the SDK created the account for this claim.
        fresh: bool,
    },
}

/// The predicted result of a claim, for the pre-claim summary (RFP Usability 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimSummary {
    /// The schedule account.
    pub schedule: AccountId,
    /// The beneficiary that signs.
    pub beneficiary: AccountId,
    /// What the claim pays. For a public claim, at least this (the clock at inclusion is later).
    pub amount: u128,
    /// The time it is computed at: the clock time read now (public) or `as_of` (private).
    pub at: u64,
    /// `claimed` after the claim.
    pub claimed_total: u128,
    /// What the schedule will still pay out after this claim (vesting later, or vested and unclaimed).
    pub remaining: u128,
    /// Where it goes.
    pub destination: Destination,
    /// Estimated network fee.
    pub fee: u128,
    /// Who pays the network fee.
    pub fee_payer: AccountId,
    /// What the fee payer holds when the fee is charged.
    pub fee_payer_holds: u128,
    /// The fee reserve: for a private claim, the amount fixed inside the proof that the fee is paid from; for a
    /// public claim, what admission holds from the signer.
    pub reserve: u128,
    /// Blocks of base-fee climb the fee is priced for.
    pub headroom_blocks: u64,
    /// The reserve that covers `headroom_blocks` of climb.
    pub headroom_reserve: u128,
    /// How the fee payer was chosen.
    pub fee_payer_choice: FeePayerChoice,
}

/// How the network fee payer of a claim was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeePayerChoice {
    /// A public claim: the beneficiary, which signs, pays.
    Signer,
    /// The caller named the private fee payer.
    Named,
    /// None named: the SDK picked the owned private account (not the destination) with the smallest balance that
    /// covers the reserve for the whole headroom. `balance` is what it held.
    Auto {
        /// The payer's native balance when picked.
        balance: u128,
    },
    /// None named and no other owned private account covers the headroom reserve: the destination pays out of
    /// the claim.
    Destination,
}

impl FeePayerChoice {
    /// A stable tag: `signer`, `named`, `auto`, `destination`.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Signer => "signer",
            Self::Named => "named",
            Self::Auto { .. } => "auto",
            Self::Destination => "destination",
        }
    }
}

impl ClaimSummary {
    /// The pre-claim summary as text, one item per line (shown by the CLI and the mini-app).
    #[must_use]
    pub fn render(&self) -> String {
        let (kind, to) = match self.destination {
            Destination::Public(a) => ("public", format!("{a} (public account)")),
            Destination::Private {
                account,
                blinded,
                fresh,
            } => (
                "private",
                format!(
                    "{account} ({}private account of this wallet; the transaction names only the one-time address {blinded})",
                    if fresh { "new " } else { "" }
                ),
            ),
        };
        let amount = match self.destination {
            Destination::Public(_) => format!(
                "at least {} (computed at the clock time of the including block)",
                self.amount
            ),
            Destination::Private { .. } => format!("{} (computed at {})", self.amount, utc(self.at)),
        };
        format!(
            "Claim summary ({kind} claim)\n  schedule:       {}\n  beneficiary:    {}\n  claimable now:  {amount}\n  \
             destination:    {to}\n  claimed after:  {}\n  still to come:  {}\n  network fee:    ~{} (estimated), paid by \
             {} which will hold {}\n  fee payer:      {}\n  fee reserve:    {}",
            self.schedule,
            self.beneficiary,
            self.claimed_total,
            self.remaining,
            self.fee,
            self.fee_payer,
            self.fee_payer_holds,
            self.fee_payer_reason(),
            self.reserve_text()
        )
    }

    /// Why this account pays the network fee.
    #[must_use]
    pub fn fee_payer_reason(&self) -> String {
        match self.fee_payer_choice {
            FeePayerChoice::Signer => "the beneficiary, which signs the public claim".to_owned(),
            FeePayerChoice::Named => "named for this claim".to_owned(),
            FeePayerChoice::Auto { balance } => format!(
                "chosen by the SDK: the private account of this wallet with the smallest balance ({balance}) that \
                 covers the reserve for {} blocks of base-fee rise ({})",
                self.headroom_blocks, self.headroom_reserve
            ),
            FeePayerChoice::Destination => format!(
                "the destination, out of the claim: no other private account of this wallet holds the reserve for {} \
                 blocks of base-fee rise ({})",
                self.headroom_blocks, self.headroom_reserve
            ),
        }
    }

    /// `true` when the reserve covers `headroom_blocks` of base-fee climb.
    #[must_use]
    pub const fn headroom_covered(&self) -> bool {
        self.reserve >= self.headroom_reserve
    }

    /// The fee reserve line of the summary.
    #[must_use]
    pub fn reserve_text(&self) -> String {
        let private = matches!(self.destination, Destination::Private { .. });
        let fixed = if private { ", fixed in the proof" } else { "" };
        if self.headroom_covered() {
            format!(
                "{}{fixed}; covers {} blocks of base-fee rise before inclusion",
                self.reserve, self.headroom_blocks
            )
        } else {
            format!(
                "{}{fixed}; covering {} blocks of base-fee rise needs {}, but the fee payer holds less before the \
                 claim. If blocks are busy the claim can be dropped (nothing is consumed; retry). To cover the rise, \
                 name a private fee payer of this wallet that holds at least {}",
                self.reserve, self.headroom_blocks, self.headroom_reserve, self.headroom_reserve
            )
        }
    }
}

/// The predicted result of an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "one per prepared transaction, never stored in bulk; unboxed so callers match on the summary directly"
)]
pub enum Outcome {
    /// One or more schedules are created.
    Created {
        /// The schedule accounts.
        schedules: Vec<AccountId>,
        /// The amount escrowed (sum over the schedules).
        total: u128,
        /// The protocol creation fee.
        protocol_fee: u128,
        /// The fee treasury named in the message.
        treasury: AccountId,
    },
    /// A claim.
    Claim(ClaimSummary),
    /// A cancel.
    Cancelled {
        /// Effective time (at least the clock time read now).
        at: u64,
        /// Vested amount frozen by the cancel.
        vested_at_cancel: u128,
        /// Refunded to `refund_to`.
        refunded: u128,
        /// The refund account.
        refund_to: AccountId,
        /// Vested and unclaimed: stays claimable by the beneficiary.
        still_claimable: u128,
    },
    /// The schedule becomes non-cancelable.
    MadeNonCancelable,
    /// A milestone is signalled.
    Signalled {
        /// Milestone index.
        index: u8,
        /// Amount it unlocks.
        amount: u128,
    },
    /// The beneficiary changes.
    Transferred {
        /// Previous beneficiary.
        from: AccountId,
        /// New beneficiary.
        to: AccountId,
    },
}

/// A checked, priced transaction, ready to send.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// The transaction.
    pub tx: Tx,
    /// Its network fee, from a dry run at current state.
    pub estimate: Estimate,
    /// What it will do.
    pub outcome: Outcome,
}

/// Where a schedule stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Vesting, or vested and not yet fully claimed.
    Active,
    /// Cancelled; the vested remainder may still be claimable.
    Cancelled,
    /// Everything that will ever vest has been paid out.
    Settled,
}

/// A schedule as a recipient or creator sees it (RFP mini-app recipient view, CLI `show`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleView {
    /// The schedule account.
    pub account: AccountId,
    /// Its state.
    pub state: ScheduleV1,
    /// The clock time the view is computed at.
    pub now: u64,
    /// Vested at `now` (frozen at the cancel for a cancelled schedule).
    pub vested: u128,
    /// Claimable now.
    pub claimable: u128,
    /// Not vested yet (0 once cancelled: the unvested part was refunded).
    pub locked: u128,
    /// When more becomes claimable.
    pub next_unlock: NextUnlock,
    /// Escrow balance (native schedules).
    pub escrow: Option<u128>,
    /// Active, cancelled or settled.
    pub standing: Standing,
}

impl ScheduleView {
    /// The next unlock as text: an ISO-8601 UTC time, "when a milestone is signalled", or "none".
    #[must_use]
    pub fn next_unlock_text(&self) -> String {
        match self.next_unlock {
            NextUnlock::At(t) => utc(t),
            NextUnlock::WhenMilestoneSignalled => "when the milestone authority signals a milestone".to_owned(),
            NextUnlock::Never => "none (nothing more vests)".to_owned(),
        }
    }
}

/// The SDK entry point: the full lifecycle over a [`Backend`].
pub struct Client<B> {
    backend: B,
}

impl<B: Backend> Client<B> {
    /// A client over `backend`.
    pub const fn new(backend: B) -> Self {
        Self { backend }
    }

    /// The backend.
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// The backend, mutable.
    pub const fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// The program ids.
    pub fn programs(&self) -> &Programs {
        self.backend.programs()
    }

    /// A fresh random schedule id.
    pub fn fresh_schedule_id(&mut self) -> [u8; 32] {
        self.backend.random_bytes()
    }

    // ---------------------------------------------------------------- reading

    /// The schedule at `account`.
    ///
    /// # Errors
    /// [`Error::NoSchedule`] when there is none; backend failures.
    pub async fn schedule(&self, account: AccountId) -> Result<ScheduleV1> {
        self.backend.schedule(account).await?.ok_or(Error::NoSchedule(account))
    }

    /// The schedule at `account` as of the current clock time: vested, claimable now, locked, next unlock.
    ///
    /// # Errors
    /// As [`Self::schedule`].
    pub async fn status(&self, account: AccountId) -> Result<ScheduleView> {
        let state = self.schedule(account).await?;
        let now = self.backend.now().await?;
        let te = state.effective_time(now);
        let vested = state.vested(te).map_err(Error::from_core)?;
        let claimable = state.claimable(now).map_err(Error::from_core)?;
        let next_unlock = state.next_unlock(now).map_err(Error::from_core)?;
        let locked = if state.cancelled.is_some() {
            0
        } else {
            state.total.saturating_sub(vested)
        };
        let escrow = match state.asset {
            Asset::Native => Some(self.backend.native_balance(account).await?),
            Asset::Token { .. } => None,
        };
        let standing = if state.is_settled() {
            Standing::Settled
        } else if state.cancelled.is_some() {
            Standing::Cancelled
        } else {
            Standing::Active
        };
        Ok(ScheduleView {
            account,
            state,
            now,
            vested,
            claimable,
            locked,
            next_unlock,
            escrow,
            standing,
        })
    }

    /// The amount claimable now.
    ///
    /// # Errors
    /// As [`Self::status`].
    pub async fn claimable(&self, account: AccountId) -> Result<u128> {
        Ok(self.status(account).await?.claimable)
    }

    /// Scans the blocks not yet in `cache` (from `max(cache.next_block, from_block)` to the latest) for schedule
    /// creations, in ranges of [`SCAN_RANGE`] blocks, and records them. A cache of another `vesting` deployment is
    /// reset first. Returns the latest block scanned.
    ///
    /// # Errors
    /// Backend failures.
    pub async fn scan(&self, cache: &mut ScanCache, from_block: Option<u64>) -> Result<u64> {
        let programs = *self.programs();
        if cache.vesting != programs.vesting {
            *cache = ScanCache::new(programs.vesting);
        }
        let last = self.backend.last_block().await?;
        let mut start = cache.next_block.max(from_block.unwrap_or(0));
        while start <= last {
            let end = start.saturating_add(SCAN_RANGE - 1).min(last);
            let calls = self.backend.root_calls(start, end).await?;
            cache.absorb(&programs, &calls);
            cache.next_block = end + 1;
            start = end + 1;
        }
        Ok(last)
    }

    /// The schedules created by, or currently payable to, an account (SPEC 11): scans new blocks into `cache`,
    /// then reads every known schedule's current state and keeps those `filter` matches, in account order.
    ///
    /// # Errors
    /// Backend failures.
    pub async fn list(
        &self,
        filter: ListFilter,
        cache: &mut ScanCache,
        from_block: Option<u64>,
    ) -> Result<Vec<ScheduleView>> {
        self.scan(cache, from_block).await?;
        let mut out = Vec::new();
        for account in cache.schedules.clone() {
            let Some(state) = self.backend.schedule(account).await? else {
                continue;
            };
            if filter.matches(&state) {
                out.push(self.status(account).await?);
            }
        }
        Ok(out)
    }

    /// The `vesting_fees` configuration.
    ///
    /// # Errors
    /// Backend failures.
    pub async fn fee_config(&self) -> Result<FeeSettings> {
        self.backend.fee_config().await
    }

    /// The creation fee on `total` at the current rate, and the treasury to name: `(fee, treasury)`. With no fee
    /// program, or a zero fee, the treasury named is `fallback` (any treasury is accepted with a zero fee).
    ///
    /// # Errors
    /// Backend failures; a configured rate above the cap (28).
    pub async fn quote_fee(&self, total: u128, fallback: AccountId) -> Result<(u128, AccountId)> {
        let config = self.backend.fee_config().await?;
        let fee = if config.deployed {
            vesting_core::fee_for(total, config.rate_bps).map_err(Error::from_core)?
        } else {
            0
        };
        Ok((fee, if fee > 0 { config.treasury } else { fallback }))
    }

    // ---------------------------------------------------------------- creation

    /// Prepares `Create`: the creator funds `terms.total` (plus the creation fee) from its own account.
    ///
    /// # Errors
    /// The terms break a rule (10 to 13, 26, 28), the schedule id is taken (7), the creator cannot pay
    /// ([`Error::InsufficientBalance`]), or the dry run refuses.
    pub async fn prepare_create(
        &mut self,
        creator: AccountId,
        schedule_id: [u8; 32],
        terms: Terms,
    ) -> Result<Prepared> {
        let programs = *self.programs();
        let schedule = programs.schedule_account(creator, &schedule_id);
        let state = terms
            .clone()
            .into_state(core_id(creator), schedule_id, core_id(creator))
            .map_err(Error::from_core)?;
        vesting_core::check_destination(&core_id(schedule), &state.refund_to).map_err(Error::from_core)?;
        if self.backend.schedule(schedule).await?.is_some() {
            return Err(Error::from_core(vesting_core::VestingError::ScheduleExists));
        }
        let (fee, treasury) = self.quote_fee(terms.total, creator).await?;
        vesting_core::check_fee_cap(terms.total, fee).map_err(Error::from_core)?;
        let asset = terms.asset;
        let total = terms.total;
        self.check_funding(creator, &asset, total, fee, 0).await?;
        let tx = programs.create(creator, schedule_id, terms, fee, treasury)?;
        let estimate = self.backend.estimate(&tx).await?;
        self.check_funding(creator, &asset, total, fee, estimate.reserve)
            .await?;
        Ok(Prepared {
            tx,
            estimate,
            outcome: Outcome::Created {
                schedules: vec![schedule],
                total,
                protocol_fee: fee,
                treasury,
            },
        })
    }

    /// How many schedules of `common`'s shape go in one `CreateBatch` transaction: the most the cycle model of
    /// [`crate::batch`] fits under 85% of the chain's execution budget, given the milestone count and whether the
    /// current fee rate charges a fee, never above the program's cap (SPEC 8).
    ///
    /// # Errors
    /// Backend failures reading the fee configuration.
    pub async fn batch_capacity(&self, common: &BatchTerms) -> Result<usize> {
        let config = self.backend.fee_config().await?;
        let with_fee = config.deployed && config.rate_bps > 0;
        Ok(crate::batch::capacity(
            &common.asset,
            crate::batch::milestones_of(&common.shape),
            with_fee,
        ))
    }

    /// Prepares a batch creation, split into transactions that each fit the chain's execution budget: at most
    /// [`Self::batch_capacity`] schedules each, or `per_tx` if that is smaller. Each transaction is checked and
    /// priced on its own, and one whose dry run measures more than the budget is refused before anything is sent.
    ///
    /// # Errors
    /// As [`Self::prepare_create`] for any item; no items (24); `per_tx` of 0 or above the program's cap (25); a
    /// transaction over the budget ([`crate::ChainFailure::CycleBudget`]).
    pub async fn prepare_create_batch(
        &mut self,
        creator: AccountId,
        common: BatchTerms,
        items: Vec<BatchItem>,
        per_tx: Option<usize>,
    ) -> Result<Vec<Prepared>> {
        let programs = *self.programs();
        if per_tx.is_some_and(|n| n == 0 || n > max_batch(&common.asset)) {
            return Err(Error::from_core(vesting_core::VestingError::BatchTooLarge));
        }
        let fits = self.batch_capacity(&common).await?;
        let per_tx = per_tx.map_or(fits, |n| n.min(fits));
        if items.is_empty() {
            return Err(Error::from_core(vesting_core::VestingError::EmptyBatch));
        }
        // The whole list first: one bad item or a repeated id stops everything before anything is priced.
        let mut ids: Vec<&[u8; 32]> = items.iter().map(|i| &i.schedule_id).collect();
        ids.sort_unstable();
        if ids.windows(2).any(|w| w[0] == w[1]) {
            return Err(Error::from_core(vesting_core::VestingError::ScheduleExists));
        }
        for item in &items {
            common
                .terms_for(item)
                .and_then(|t| t.into_state(core_id(creator), item.schedule_id, core_id(creator)))
                .map_err(Error::from_core)?;
            if self
                .backend
                .schedule(programs.schedule_account(creator, &item.schedule_id))
                .await?
                .is_some()
            {
                return Err(Error::from_core(vesting_core::VestingError::ScheduleExists));
            }
        }
        let mut prepared = Vec::new();
        let (mut sum_total, mut sum_fee, mut sum_reserve) = (0u128, 0u128, 0u128);
        for chunk in items.chunks(per_tx) {
            let (_, total) = vesting_core::validate_batch(core_id(creator), core_id(creator), &common, chunk)
                .map_err(Error::from_core)?;
            let (fee, treasury) = self.quote_fee(total, creator).await?;
            vesting_core::check_fee_cap(total, fee).map_err(Error::from_core)?;
            sum_total = sum_total.saturating_add(total);
            sum_fee = sum_fee.saturating_add(fee);
            self.check_funding(creator, &common.asset, sum_total, sum_fee, sum_reserve)
                .await?;
            let tx = programs.create_batch(creator, common.clone(), chunk.to_vec(), fee, treasury)?;
            let estimate = self.backend.estimate(&tx).await?;
            if estimate.cycles > crate::batch::MAX_GAS_EXEC {
                return Err(Error::Chain {
                    kind: crate::ChainFailure::CycleBudget,
                    reason: format!(
                        "a batch of {} measures {} cycles, above the budget of {}; pass a smaller per-transaction size",
                        chunk.len(),
                        estimate.cycles,
                        crate::batch::MAX_GAS_EXEC
                    ),
                });
            }
            sum_reserve = sum_reserve.saturating_add(estimate.reserve);
            self.check_funding(creator, &common.asset, sum_total, sum_fee, sum_reserve)
                .await?;
            prepared.push(Prepared {
                outcome: Outcome::Created {
                    schedules: tx.schedules.clone(),
                    total,
                    protocol_fee: fee,
                    treasury,
                },
                tx,
                estimate,
            });
        }
        Ok(prepared)
    }

    /// Checks that `payer` holds `amount + fee` of a native asset plus the network `reserve`. Token amounts are
    /// checked by the dry run.
    async fn check_funding(
        &self,
        payer: AccountId,
        asset: &Asset,
        amount: u128,
        fee: u128,
        reserve: u128,
    ) -> Result<()> {
        let have = self.backend.native_balance(payer).await?;
        let (need, what) = match asset {
            Asset::Native => (
                amount.saturating_add(fee).saturating_add(reserve),
                if reserve == 0 {
                    "escrow and creation fee"
                } else {
                    "escrow, creation fee and network fee"
                },
            ),
            Asset::Token { .. } => (reserve, "network fee"),
        };
        if have < need {
            return Err(Error::InsufficientBalance {
                account: payer,
                need,
                have,
                what,
            });
        }
        Ok(())
    }

    /// Checks that a public signer can pay the network fee of `estimate`.
    async fn check_network_fee(&self, signer: AccountId, estimate: &Estimate) -> Result<()> {
        let have = self.backend.native_balance(signer).await?;
        if have < estimate.reserve {
            return Err(Error::InsufficientBalance {
                account: signer,
                need: estimate.reserve,
                have,
                what: "network fee",
            });
        }
        Ok(())
    }

    // ---------------------------------------------------------------- claims

    /// Prepares a claim by `beneficiary` on `schedule` to `target`, public or private.
    ///
    /// Checks, in order: the schedule exists (8); `beneficiary` is its beneficiary (3); for a private claim, that
    /// the target and the fee payer are private accounts of this wallet ([`Error::NotPrivateAccount`], Privacy 3);
    /// that something is claimable now ([`Error::NothingClaimable`] with the next unlock in UTC); for a public
    /// claim, that the destination is not the schedule (26) and can receive the asset; then it dry-runs the
    /// transaction and checks that the fee payer can pay the fee ([`Error::PrivateFeeShortfall`] or
    /// [`Error::InsufficientBalance`], Usability 4).
    ///
    /// A private claim is a `ClaimAsOf` at the current clock time to a fresh one-time blinded address of the
    /// destination (SPEC 9): a new opening per call, never reused.
    ///
    /// # Errors
    /// As listed.
    pub async fn prepare_claim(
        &mut self,
        beneficiary: AccountId,
        schedule: AccountId,
        target: ClaimTarget,
    ) -> Result<Prepared> {
        let programs = *self.programs();
        let state = self.schedule(schedule).await?;
        state
            .authorise(Role::Beneficiary, &core_id(beneficiary))
            .map_err(Error::from_core)?;
        if let ClaimTarget::Private { account, fee_payer } = target {
            for a in account.into_iter().chain(fee_payer) {
                self.require_private(a)?;
            }
        }
        let now = self.backend.now().await?;
        let mut after = state.clone();
        let outcome = after.apply_claim(now).map_err(Error::from_core)?;
        let remaining = after
            .cancelled
            .map_or(after.total, |c| c.vested_at_cancel)
            .saturating_sub(after.claimed);

        let (tx, destination) = match target {
            ClaimTarget::Public(to) => {
                vesting_core::check_destination(&core_id(schedule), &core_id(to)).map_err(Error::from_core)?;
                if !self.backend.can_receive(to, &state.asset).await? {
                    return Err(Error::NoHolding(to));
                }
                if to != beneficiary
                    && self.backend.account_record(to) != AccountRecord::PublicOwned
                    && !self.backend.exists(to).await?
                {
                    return Err(Error::UnknownDestination(to));
                }
                (
                    programs.claim(beneficiary, schedule, to, &state.asset)?,
                    Destination::Public(to),
                )
            }
            ClaimTarget::Private { account, fee_payer } => {
                let (account, fresh) = match account {
                    Some(a) => (a, false),
                    None => (self.backend.new_private_account().await?, true),
                };
                // Privacy 3 on the account actually used, fresh ones included.
                self.require_private(account)?;
                let opening = self.backend.random_bytes();
                let blinded = account.blinded(&opening);
                vesting_core::check_destination(&core_id(schedule), &core_id(blinded)).map_err(Error::from_core)?;
                // First priced with the payer named by the caller, else with the destination paying (out of a native
                // claim). Left unnamed, the wallet would pick a payer the summary could not predict.
                let tx = programs.claim_private(
                    beneficiary,
                    schedule,
                    account,
                    opening,
                    now,
                    &state.asset,
                    Some(fee_payer.unwrap_or(account)),
                )?;
                (
                    tx,
                    Destination::Private {
                        account,
                        blinded,
                        fresh,
                    },
                )
            }
        };

        let mut tx = tx;
        let mut estimate = self.backend.estimate(&tx).await?;
        let fee_payer_choice = match (target, destination) {
            (ClaimTarget::Public(_), _) => FeePayerChoice::Signer,
            (ClaimTarget::Private { fee_payer: Some(_), .. }, _) => FeePayerChoice::Named,
            (_, Destination::Private { account, .. }) => {
                // No payer named: the owned private account (other than the destination) with the smallest balance
                // that covers the reserve for the whole headroom, so large holdings are not used needlessly; ties
                // by account id. None qualifies: the destination pays (out of a native claim), with a warning.
                let need = estimate.headroom_reserve;
                let pick = self
                    .backend
                    .owned_private_accounts()
                    .into_iter()
                    .filter(|(a, balance)| *a != account && *balance >= need)
                    .min_by_key(|(a, balance)| (*balance, *a));
                match pick {
                    Some((payer, balance)) => {
                        if let crate::Route::Private { fee_payer, .. } = &mut tx.route {
                            *fee_payer = Some(payer);
                        }
                        estimate = self.backend.estimate(&tx).await?;
                        FeePayerChoice::Auto { balance }
                    }
                    None => FeePayerChoice::Destination,
                }
            }
            (_, Destination::Public(_)) => FeePayerChoice::Signer,
        };
        let fee_payer_holds = match destination {
            Destination::Public(_) => {
                self.check_network_fee(beneficiary, &estimate).await?;
                self.backend.native_balance(beneficiary).await?
            }
            Destination::Private { account, .. } => {
                let payer = estimate.payer;
                let mut holds = self.backend.private_native_balance(payer).unwrap_or(0);
                if payer == account && state.asset == Asset::Native {
                    // The fee is charged after the root call, so a native claim pays its own fee (SPEC 9).
                    holds = holds.saturating_add(outcome.amount);
                }
                if holds < estimate.fee {
                    return Err(Error::PrivateFeeShortfall {
                        payer,
                        need: estimate.fee,
                        have: holds,
                        hint: match state.asset {
                            Asset::Native => {
                                "claim later, when more has vested, or name a funded private account of this wallet \
                                 as fee payer"
                            }
                            Asset::Token { .. } => {
                                "a token claim brings no native balance: name a private account of this wallet that \
                                 holds native balance as fee payer, and fund it first"
                            }
                        },
                    });
                }
                holds
            }
        };
        Ok(Prepared {
            outcome: Outcome::Claim(ClaimSummary {
                schedule,
                beneficiary,
                amount: outcome.amount,
                at: outcome.at,
                claimed_total: outcome.claimed_total,
                remaining,
                destination,
                fee: estimate.fee,
                fee_payer: estimate.payer,
                fee_payer_holds,
                reserve: estimate.reserve,
                headroom_blocks: estimate.headroom_blocks,
                headroom_reserve: estimate.headroom_reserve,
                fee_payer_choice,
            }),
            tx,
            estimate,
        })
    }

    /// Privacy 3: `account` must be a private account this wallet owns, from the wallet's own account record.
    fn require_private(&self, account: AccountId) -> Result<()> {
        match self.backend.account_record(account) {
            AccountRecord::PrivateOwned => Ok(()),
            found => Err(Error::NotPrivateAccount { account, found }),
        }
    }

    // ---------------------------------------------------------------- authority operations

    /// Prepares `Cancel` by `authority`: freezes vesting now and refunds the unvested part to the refund account.
    ///
    /// # Errors
    /// No schedule (8); not the cancel authority (5); cancelled (16); not cancelable (15); the dry run refuses; the
    /// authority cannot pay the network fee.
    pub async fn prepare_cancel(&mut self, authority: AccountId, schedule: AccountId) -> Result<Prepared> {
        let state = self.schedule(schedule).await?;
        state
            .authorise(Role::CancelAuthority, &core_id(authority))
            .map_err(Error::from_core)?;
        state.check_cancel().map_err(Error::from_core)?;
        let now = self.backend.now().await?;
        let mut after = state.clone();
        let c = after.apply_cancel(now).map_err(Error::from_core)?;
        let refund_to = lez_id(state.refund_to);
        let tx = self.programs().cancel(authority, schedule, refund_to, &state.asset)?;
        let estimate = self.backend.estimate(&tx).await?;
        self.check_network_fee(authority, &estimate).await?;
        Ok(Prepared {
            tx,
            estimate,
            outcome: Outcome::Cancelled {
                at: c.at,
                vested_at_cancel: c.vested_at_cancel,
                refunded: c.refunded,
                refund_to,
                still_claimable: c.vested_at_cancel.saturating_sub(state.claimed),
            },
        })
    }

    /// Prepares `MakeNonCancelable` by the creator or the cancel authority.
    ///
    /// # Errors
    /// No schedule (8); not allowed (4); cancelled (16); already non-cancelable (15); dry run; network fee.
    pub async fn prepare_make_non_cancelable(&mut self, caller: AccountId, schedule: AccountId) -> Result<Prepared> {
        let state = self.schedule(schedule).await?;
        state
            .authorise(Role::Creator, &core_id(caller))
            .map_err(Error::from_core)?;
        state.clone().apply_make_non_cancelable().map_err(Error::from_core)?;
        let tx = self.programs().make_non_cancelable(caller, schedule);
        self.finish_simple(caller, tx, Outcome::MadeNonCancelable).await
    }

    /// Prepares `SignalMilestone` by the milestone authority.
    ///
    /// # Errors
    /// No schedule (8); not the milestone authority (6); linear schedule (20); cancelled (16); index out of range
    /// (17); already signalled (18); dry run; network fee.
    pub async fn prepare_signal(&mut self, caller: AccountId, schedule: AccountId, index: u8) -> Result<Prepared> {
        let state = self.schedule(schedule).await?;
        state
            .authorise(Role::MilestoneAuthority, &core_id(caller))
            .map_err(Error::from_core)?;
        let signal = state.clone().apply_signal_milestone(index).map_err(Error::from_core)?;
        let tx = self.programs().signal(caller, schedule, index);
        self.finish_simple(
            caller,
            tx,
            Outcome::Signalled {
                index,
                amount: signal.amount,
            },
        )
        .await
    }

    /// Prepares `TransferBeneficiary` by the current beneficiary.
    ///
    /// # Errors
    /// No schedule (8); not the beneficiary (3); not transferable (19); dry run; network fee.
    pub async fn prepare_transfer(
        &mut self,
        caller: AccountId,
        schedule: AccountId,
        new_beneficiary: AccountId,
    ) -> Result<Prepared> {
        let state = self.schedule(schedule).await?;
        state
            .authorise(Role::Beneficiary, &core_id(caller))
            .map_err(Error::from_core)?;
        let from = state
            .clone()
            .apply_transfer_beneficiary(core_id(new_beneficiary))
            .map_err(Error::from_core)?;
        let tx = self.programs().transfer(caller, schedule, new_beneficiary);
        self.finish_simple(
            caller,
            tx,
            Outcome::Transferred {
                from: lez_id(from),
                to: new_beneficiary,
            },
        )
        .await
    }

    async fn finish_simple(&mut self, caller: AccountId, tx: Tx, outcome: Outcome) -> Result<Prepared> {
        let estimate = self.backend.estimate(&tx).await?;
        self.check_network_fee(caller, &estimate).await?;
        Ok(Prepared { tx, estimate, outcome })
    }

    // ---------------------------------------------------------------- sending

    /// Sends a prepared transaction and waits until it is included.
    ///
    /// # Errors
    /// A `ClaimAsOf` whose `as_of` is ahead of the chain clock ([`ChainFailure::ValidityWindow`](crate::ChainFailure),
    /// nothing sent); the wallet's local run refuses it (mapped like a dry-run refusal); it is not included; or it
    /// was included and reverted.
    pub async fn send(&mut self, prepared: &Prepared) -> Result<Receipt> {
        // A local run does not check validity windows, and the chain publishes no reason for a revert: a `ClaimAsOf`
        // whose `as_of` is ahead of the clock would be charged for a certain revert. Check it here, every time.
        if let vesting_core::Message::ClaimAsOf { as_of, .. } = prepared.tx.message {
            let now = self.backend.now().await?;
            if as_of > now {
                return Err(Error::Chain {
                    kind: crate::ChainFailure::ValidityWindow,
                    reason: format!(
                        "the claim is computed at {} but the chain clock reads {}",
                        utc(as_of),
                        utc(now)
                    ),
                });
            }
        }
        self.backend.submit(&prepared.tx).await
    }
}

/// Terms of a linear schedule with the RFP's defaults (cancelable, not transferable, authorities and refund
/// account defaulted). `cliff == start` is fully linear; `cliff == end` is a single cliff.
#[must_use]
pub fn linear_terms(beneficiary: AccountId, asset: Asset, total: u128, start: u64, cliff: u64, end: u64) -> Terms {
    Terms {
        beneficiary: core_id(beneficiary),
        asset,
        total,
        kind: Kind::Linear { start, cliff, end },
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    }
}

/// Terms of a milestone schedule with the RFP's defaults; the total is the sum of `amounts`.
#[must_use]
pub fn milestone_terms(beneficiary: AccountId, asset: Asset, amounts: Vec<u128>) -> Terms {
    Terms {
        beneficiary: core_id(beneficiary),
        asset,
        total: amounts.iter().fold(0u128, |s, a| s.saturating_add(*a)),
        kind: Kind::Milestones { amounts, signalled: 0 },
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    }
}
