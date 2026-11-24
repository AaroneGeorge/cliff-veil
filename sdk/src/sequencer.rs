//! [`SequencerBackend`]: LEZ v0.4.0-rc3's own wallet against a sequencer's JSON-RPC.
//!
//! The wallet holds the keys, prices and simulates every transaction, signs public ones, and builds and proves
//! privacy-preserving ones (with `RISC0_DEV_MODE=1` the proof is a dev-mode receipt). Reads go through the same
//! RPC. Recipe for the private claim: the wallet's `send_tx_with_pre_check` with a private mention that carries the
//! address opening; the program's bytecode for the prover is fetched from its deployment.

use std::borrow::Borrow as _;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use clock_core::{ClockAccountData, clock_account_id, clock_actor};
use common::transaction::LeeTransaction;
use lee::privacy_preserving_transaction::circuit::ProgramCatalog;
use lee_core::account::{AccountId, Actor};
use lee_core::native_token;
use sequencer_service_rpc::SequencerClient;
use vesting_core::{Asset, ScheduleState, ScheduleV1};
use wallet::program_facades::program_loader::ProgramLoader;
use wallet::{AccountIdentity, AsyncDelivery, DeclaredFee, DryRunReport, ExecutionFailureKind, WalletCore};

use crate::backend::{AccountRecord, Backend, Estimate, FeeSettings, Receipt, RootCallSeen};
use crate::errors::{Error, Result};
use crate::tx::{Programs, Route, Tx};

/// The wallet type the backend drives.
pub type Wallet = WalletCore<SequencerClient>;

/// LEZ's wallet against a sequencer.
pub struct SequencerBackend {
    wallet: Wallet,
    programs: Programs,
    /// Deployed bytecode fetched for the prover, by program id.
    bytecode: std::collections::BTreeMap<AccountId, lee::program::Program>,
    headroom_blocks: u64,
    /// How long [`Backend::submit`] waits for inclusion.
    pub inclusion_timeout: Duration,
    /// After this many new blocks without the transaction, [`Backend::submit`] reports it dropped.
    pub drop_after_blocks: u64,
}

/// Blocks of steepest base-fee climb the SDK prices fees for, unless configured. LEZ's wallet default is 5; a
/// private claim's reserve is fixed in its proof and must survive busy blocks until inclusion.
pub const DEFAULT_FEE_HEADROOM_BLOCKS: u64 = 12;

impl SequencerBackend {
    /// A backend over an opened wallet.
    #[must_use]
    /// Fees are priced for [`DEFAULT_FEE_HEADROOM_BLOCKS`] blocks of base-fee climb
    /// ([`Self::set_fee_headroom_blocks`] changes it).
    pub fn new(mut wallet: Wallet, programs: Programs) -> Self {
        wallet.set_fee_headroom_blocks(DEFAULT_FEE_HEADROOM_BLOCKS);
        Self {
            wallet,
            programs,
            bytecode: std::collections::BTreeMap::new(),
            headroom_blocks: DEFAULT_FEE_HEADROOM_BLOCKS,
            inclusion_timeout: Duration::from_secs(300),
            drop_after_blocks: 20,
        }
    }

    /// Prices every fee for `blocks` of the steepest base-fee climb past the current quote (the wallet's fee
    /// headroom). A privacy-preserving transaction's reserve is fixed inside its proof, so it must already cover
    /// the climb until inclusion.
    pub fn set_fee_headroom_blocks(&mut self, blocks: u64) {
        self.headroom_blocks = blocks;
        self.wallet.set_fee_headroom_blocks(blocks);
    }

    /// Opens the wallet from the environment (`LEE_WALLET_HOME_DIR`, `LEE_WALLET_PASSWORD`), as LEZ's `wallet`
    /// binary does.
    ///
    /// # Errors
    /// The wallet home is missing or does not open.
    pub fn from_env(programs: Programs) -> Result<Self> {
        let wallet = Wallet::from_env().map_err(|e| anyhow!("open the wallet (LEE_WALLET_HOME_DIR): {e:#}"))?;
        Ok(Self::new(wallet, programs))
    }

    /// The wallet.
    #[must_use]
    pub const fn wallet(&self) -> &Wallet {
        &self.wallet
    }

    /// The wallet, mutable (labels, accounts).
    pub const fn wallet_mut(&mut self) -> &mut Wallet {
        &mut self.wallet
    }

    /// An account by label (`cv-payer`), by `Public/<id>` or `Private/<id>`, or by bare base58 id.
    ///
    /// # Errors
    /// Neither a known label nor an account id.
    pub fn resolve(&self, text: &str) -> Result<AccountId> {
        use wallet::account::{AccountIdWithPrivacy, Label};
        let from_privacy = |id: AccountIdWithPrivacy| match id {
            AccountIdWithPrivacy::Public(a) | AccountIdWithPrivacy::Private(a) => a,
        };
        if let Some(id) = self.wallet.storage().resolve_label(&Label::new(text)) {
            return Ok(from_privacy(id));
        }
        if let Ok(id) = text.parse::<AccountIdWithPrivacy>() {
            return Ok(from_privacy(id));
        }
        text.parse()
            .map_err(|e| Error::Input(format!("`{text}` is neither a wallet label nor an account id: {e:?}")))
    }

    async fn actor_state(&self, actor: Actor) -> Result<Vec<u8>> {
        let view = self
            .wallet
            .get_account_view(actor)
            .await
            .map_err(|e| anyhow!("getAccountView {actor:?}: {e:#}"))?;
        Ok(view
            .map(|a| {
                let bytes: &[u8] = a.data.actor_state(actor.program_account_id).borrow();
                bytes.to_vec()
            })
            .unwrap_or_default())
    }

    /// The deployed bytecode of every program a proof of `tx` re-executes ([`Tx::prover_programs`]), keyed by
    /// id, for the prover. Each program is fetched from its deployment once.
    async fn catalog(&mut self, tx: &Tx) -> Result<ProgramCatalog> {
        let needed = tx.prover_programs();
        for id in &needed {
            if self.bytecode.contains_key(id) {
                continue;
            }
            let program = ProgramLoader(&self.wallet)
                .program(*id)
                .await
                .map_err(|e| anyhow!("fetch program {id}: {e:#}"))?
                .with_context(|| format!("no program is deployed at {id}"))?;
            self.bytecode.insert(*id, program);
        }
        Ok(ProgramCatalog::from_iter(
            needed
                .iter()
                .filter_map(|id| self.bytecode.get(id).map(|p| (*id, p.clone()))),
        ))
    }

    async fn send_raw(&mut self, tx: &Tx) -> std::result::Result<common::HashType, ExecutionFailureKind> {
        let message = tx.message.encode();
        match tx.route {
            Route::Public => self.wallet.send_pub_tx(tx.accounts.clone(), 0, message).await,
            Route::Private { fee_payer, .. } => {
                let catalog = self
                    .catalog(tx)
                    .await
                    .map_err(|e| ExecutionFailureKind::SequencerError(anyhow!("{e}")))?;
                self.wallet
                    .send_tx_with_pre_check(
                        tx.accounts.clone(),
                        0,
                        message,
                        &catalog,
                        None,
                        AsyncDelivery::default(),
                        fee_payer,
                        |_| Ok(()),
                    )
                    .await
                    .map(|(hash, _)| hash)
            }
        }
    }
}

/// The SDK error of a wallet failure: a program refusal or a known LEZ failure when the text carries one.
fn wallet_error(e: &ExecutionFailureKind) -> Error {
    let text = match e {
        ExecutionFailureKind::SimulatedRevert(reason) => reason.clone(),
        other => format!("{other} ({other:?})"),
    };
    Error::from_text(&text)
}

impl Backend for SequencerBackend {
    fn programs(&self) -> &Programs {
        &self.programs
    }

    async fn now(&self) -> Result<u64> {
        let account = self
            .wallet
            .get_account_view(clock_actor())
            .await
            .map_err(|e| anyhow!("read the clock: {e:#}"))?
            .context("the clock account is missing")?;
        Ok(ClockAccountData::from_bytes(account.data.actor_state(clock_account_id())).timestamp)
    }

    async fn schedule(&self, account: AccountId) -> Result<Option<ScheduleV1>> {
        let bytes = self.actor_state(Actor::new(account, self.programs.vesting)).await?;
        if bytes.is_empty() {
            return Ok(None);
        }
        ScheduleState::decode_v1(&bytes).map(Some).map_err(|_| {
            Error::Input(format!(
                "the state at {account} is not a schedule of this vesting program"
            ))
        })
    }

    async fn native_balance(&self, account: AccountId) -> Result<u128> {
        let bytes = self.actor_state(Actor::native_balance(account)).await?;
        if bytes.is_empty() {
            return Ok(0);
        }
        native_token::decode_balance(&bytes).map_err(|e| Error::Backend(anyhow!("balance of {account}: {e:?}")))
    }

    async fn can_receive(&self, account: AccountId, asset: &Asset) -> Result<bool> {
        match asset {
            Asset::Native => Ok(true),
            Asset::Token { .. } => {
                let token = self.programs.asset_program(asset)?;
                Ok(!self.actor_state(Actor::new(account, token)).await?.is_empty())
            }
        }
    }

    async fn exists(&self, account: AccountId) -> Result<bool> {
        let a = self
            .wallet
            .get_account_public(account)
            .await
            .map_err(|e| anyhow!("getAccount {account}: {e:#}"))?;
        Ok(a.is_some_and(|a| a != lee::Account::default()))
    }

    async fn fee_config(&self) -> Result<FeeSettings> {
        let (Some(program), Some(config)) = (self.programs.fees, self.programs.fees_config()) else {
            return Ok(FeeSettings {
                rate_bps: 0,
                treasury: AccountId::new([0; 32]),
                deployed: false,
            });
        };
        let bytes = self.actor_state(Actor::new(config, program)).await?;
        let config: vesting_fees::FeeConfig = if bytes.is_empty() {
            vesting_fees::FeeConfig::default()
        } else {
            borsh::from_slice(&bytes).map_err(|e| anyhow!("the vesting_fees config does not decode: {e}"))?
        };
        Ok(FeeSettings {
            rate_bps: config.rate_bps,
            treasury: config.treasury,
            deployed: true,
        })
    }

    async fn last_block(&self) -> Result<u64> {
        Ok(self
            .wallet
            .get_last_block_id()
            .await
            .map_err(|e| anyhow!("getLastBlockId: {e:#}"))?)
    }

    async fn root_calls(&self, from: u64, to: u64) -> Result<Vec<RootCallSeen>> {
        use wallet::WalletSequencerClient as _;
        let blocks = self
            .wallet
            .client_owned()
            .get_block_range(from, to)
            .await
            .map_err(|e| anyhow!("getBlockRange {from}..={to}: {e}"))?;
        let mut out = Vec::new();
        for block in blocks {
            for (i, tx) in block.body.transactions.iter().enumerate() {
                let root = match tx {
                    LeeTransaction::Public(t) => Some(&t.message.execution.root),
                    LeeTransaction::PrivacyPreserving(t) => t.message.execution.public_root.as_ref(),
                };
                if let Some(root) = root {
                    out.push(RootCallSeen {
                        block: block.header.block_id,
                        account: root.to.account_id,
                        program: root.to.program_account_id,
                        message: root.message.clone(),
                        reverted: block.body.is_reverted(i),
                    });
                }
            }
        }
        Ok(out)
    }

    fn account_record(&self, account: AccountId) -> AccountRecord {
        match self.wallet.resolve_private_account(account) {
            Some(AccountIdentity::PrivateOwned(_)) => AccountRecord::PrivateOwned,
            Some(_) => AccountRecord::PrivateShared,
            None if self.wallet.get_account_public_signing_key(account).is_some() => AccountRecord::PublicOwned,
            None => AccountRecord::NotOwned,
        }
    }

    fn owned_private_accounts(&self) -> Vec<(AccountId, u128)> {
        // Regular accounts only: the wallet takes its fee payers from them.
        let keys = self.wallet.storage().key_chain();
        keys.private_account_ids()
            .map(|(id, _)| id)
            .filter(|id| {
                keys.private_account(*id)
                    .is_some_and(|f| *f.kind == lee_core::PrivateAccountKind::Regular)
            })
            .map(|id| (id, self.private_native_balance(id).unwrap_or(0)))
            .collect()
    }

    fn fee_headroom_blocks(&self) -> u64 {
        self.headroom_blocks
    }

    fn private_native_balance(&self, account: AccountId) -> Option<u128> {
        self.wallet
            .get_account_private(account)
            .and_then(|a| a.data.native_balance().ok())
    }

    async fn new_private_account(&mut self) -> Result<AccountId> {
        let (account, _) = self.wallet.create_new_account_private(None);
        self.wallet
            .store_persistent_data()
            .map_err(|e| anyhow!("store the wallet after creating a private account: {e:#}"))?;
        Ok(account)
    }

    fn random_bytes(&mut self) -> [u8; 32] {
        use rand::RngCore as _;
        let mut b = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut b);
        b
    }

    async fn estimate(&mut self, tx: &Tx) -> Result<Estimate> {
        self.wallet.set_dry_run(true);
        let result = self.send_raw(tx).await;
        self.wallet.set_dry_run(false);
        match result {
            Err(ExecutionFailureKind::DryRun(report)) => match *report {
                DryRunReport::Public {
                    fee,
                    cycles,
                    fee_at_head,
                    reserve_at_head,
                    revert_reason,
                    ..
                } => {
                    if let Some(reason) = revert_reason {
                        return Err(Error::from_text(&reason));
                    }
                    Ok(Estimate {
                        fee: fee_at_head,
                        reserve: reserve_at_head.max(fee_at_head),
                        payer: fee.payer,
                        private: false,
                        cycles,
                        headroom_blocks: self.headroom_blocks,
                        // The cap the wallet declares: the fee at the gas limit after the headroom climb.
                        headroom_reserve: fee.max_fee,
                    })
                }
                DryRunReport::Private {
                    fee,
                    cycles,
                    fee_at_head,
                    headroom_blocks,
                    headroom_reserve,
                    ..
                } => Ok(Estimate {
                    fee: fee_at_head,
                    reserve: fee.reserve,
                    payer: fee.payer,
                    private: true,
                    cycles,
                    headroom_blocks,
                    headroom_reserve,
                }),
            },
            Err(e) => Err(wallet_error(&e)),
            Ok(hash) => Err(Error::Backend(anyhow!(
                "the wallet sent {hash} during a dry run; it was meant only to be priced"
            ))),
        }
    }

    async fn submit(&mut self, tx: &Tx) -> Result<Receipt> {
        let hash = self.send_raw(tx).await.map_err(|e| wallet_error(&e))?;
        let declared_fee = self.wallet.take_declared_fee().map(|f| match f {
            DeclaredFee::Public(d) => d.max_fee,
            DeclaredFee::Private(p) => p.reserve,
        });
        let sent_at = self.wallet.get_last_block_id().await.unwrap_or(0);
        let deadline = Instant::now() + self.inclusion_timeout;
        let block = loop {
            if let Some((_, block)) = self
                .wallet
                .get_transaction(hash)
                .await
                .map_err(|e| anyhow!("getTransaction {hash}: {e:#}"))?
            {
                break block;
            }
            // A transaction that fails settlement (for example a private fee reserve below the base fee at
            // inclusion) is dropped with no trace a client can read (S7): stop waiting after a number of blocks.
            if let Ok(now) = self.wallet.get_last_block_id().await
                && now > sent_at.saturating_add(self.drop_after_blocks)
            {
                return Err(Error::Dropped {
                    hash: hash.to_string(),
                    blocks: now - sent_at,
                    private: tx.is_private(),
                });
            }
            if Instant::now() > deadline {
                return Err(Error::Backend(anyhow!(
                    "transaction {hash} was not included within {:?}",
                    self.inclusion_timeout
                )));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        let reverted = self
            .wallet
            .get_block(block)
            .await
            .map_err(|e| anyhow!("getBlock {block}: {e:#}"))?
            .and_then(|b| {
                let i = b
                    .body
                    .transactions
                    .iter()
                    .position(|t: &LeeTransaction| t.hash() == hash)?;
                Some(b.body.is_reverted(i))
            })
            .unwrap_or(false);
        if tx.is_private() {
            self.wallet
                .sync_to_latest_block()
                .await
                .map_err(|e| anyhow!("sync the wallet: {e:#}"))?;
            self.wallet
                .store_persistent_data()
                .map_err(|e| anyhow!("store the wallet: {e:#}"))?;
        }
        if reverted {
            return Err(Error::Reverted {
                hash: hash.to_string(),
                block,
            });
        }
        Ok(Receipt {
            hash: hash.to_string(),
            block,
            declared_fee,
        })
    }
}
