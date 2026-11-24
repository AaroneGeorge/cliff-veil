//! Private accounts for the token private claim (dev mode): a private fee payer funded from a public account, a
//! private token holding initialised by the private account itself, and a privacy-preserving send whose proof
//! re-executes both `vesting` and the token program.
//!
//! A token claim brings no native balance, so the network fee of every privacy-preserving transaction here is paid
//! by a separate owned private account that holds native balance (SPEC 9).

use std::borrow::Borrow as _;

use anyhow::{Context as _, Result, anyhow};
use lee::privacy_preserving_transaction::circuit::ProgramCatalog;
use lee_core::account::AccountId;
use sequencer_service_rpc::SequencerClient;
use wallet::program_facades::CreditDelivery;
use wallet::program_facades::native_token_transfer::NativeTokenTransfer;
use wallet::program_facades::program_loader::ProgramLoader;
use wallet::{AccountIdentity, AccountMention, AsyncDelivery, WalletCore};

/// The wallet type of the SDK's sequencer backend.
pub type Wallet = WalletCore<SequencerClient>;

/// A new owned private account of `wallet` (stored).
///
/// # Errors
/// The wallet cannot be stored.
pub fn new_private(wallet: &mut Wallet) -> Result<AccountId> {
    let (account, _) = wallet.create_new_account_private(None);
    wallet
        .store_persistent_data()
        .map_err(|e| anyhow!("store the wallet: {e:#}"))?;
    Ok(account)
}

/// The deployed bytecode of `programs`, keyed by program id, for the prover.
///
/// # Errors
/// RPC failure, or a program is not deployed.
pub async fn catalog(wallet: &Wallet, programs: &[AccountId]) -> Result<ProgramCatalog> {
    let mut entries = Vec::new();
    for id in programs {
        let program = ProgramLoader(wallet)
            .program(*id)
            .await
            .map_err(|e| anyhow!("fetch program {id}: {e:#}"))?
            .with_context(|| format!("no program deployed at {id}"))?;
        entries.push((*id, program));
    }
    Ok(ProgramCatalog::from_iter(entries))
}

/// Waits for `hash`, then syncs the wallet's private accounts to the latest block and stores them. Returns the
/// block.
///
/// # Errors
/// Not included, or the sync fails.
pub async fn settle(wallet: &mut Wallet, hash: common::HashType) -> Result<u64> {
    let (_, block) = wallet
        .poll_transaction(hash)
        .await
        .map_err(|e| anyhow!("transaction {hash}: {e:#}"))?;
    wallet
        .sync_to_latest_block()
        .await
        .map_err(|e| anyhow!("sync the wallet: {e:#}"))?;
    wallet
        .store_persistent_data()
        .map_err(|e| anyhow!("store the wallet: {e:#}"))?;
    Ok(block)
}

/// Sends `amount` native units from the public account `from` (its key is in `wallet`) to the owned private
/// account `to`, as a privacy-preserving transaction; waits and syncs.
///
/// # Errors
/// The wallet refuses the transfer, or it is not included.
pub async fn fund_private(wallet: &mut Wallet, from: AccountId, to: AccountId, amount: u128) -> Result<u64> {
    let (hash, _) = NativeTokenTransfer(wallet)
        .transfer(
            AccountIdentity::Public(from),
            AccountIdentity::PrivateOwned(to),
            amount,
            CreditDelivery::Automatic,
        )
        .await
        .map_err(|e| anyhow!("public to private transfer: {e:#}"))?;
    settle(wallet, hash).await
}

/// The owned private account `holder` initialises its own holding of `definition` (the token program's
/// `InitializeHolding`, sent from the private account), with `fee_payer` paying the network fee; waits and syncs.
///
/// # Errors
/// The wallet refuses it, or it is not included.
pub async fn init_private_holding(
    wallet: &mut Wallet,
    token: AccountId,
    holder: AccountId,
    definition: AccountId,
    fee_payer: AccountId,
) -> Result<u64> {
    let catalog = catalog(wallet, &[token]).await?;
    let accounts = vec![
        AccountMention {
            identity: AccountIdentity::PrivateOwned(holder),
            program_account_id: token,
            authorizes: true,
            address_openings: std::collections::BTreeSet::new(),
        },
        AccountIdentity::PublicNoSign(definition)
            .select_program_actor_state(token)
            .without_authorization(),
    ];
    let message = borsh::to_vec(&token_core::Message::InitializeHolding { definition })?;
    let (hash, _) = wallet
        .send_tx_with_pre_check(
            accounts,
            0,
            message,
            &catalog,
            None,
            AsyncDelivery::default(),
            Some(fee_payer),
            |_| Ok(()),
        )
        .await
        .map_err(|e| anyhow!("private InitializeHolding: {e:#}"))?;
    settle(wallet, hash).await
}

/// Sends a prepared private claim of the SDK (`Route::Private`) with a catalog holding `vesting` and the token
/// program, since the proof re-executes the token transfer the claim makes; waits and syncs. Returns
/// `(hash, block)`.
///
/// # Errors
/// Not a private claim; the wallet refuses it; it is not included.
pub async fn send_private_claim(
    wallet: &mut Wallet,
    tx: &cliff_veil_sdk::Tx,
    programs: &[AccountId],
) -> Result<(common::HashType, u64)> {
    let cliff_veil_sdk::Route::Private { fee_payer, .. } = tx.route else {
        anyhow::bail!("not a private claim");
    };
    let catalog = catalog(wallet, programs).await?;
    let (hash, _) = wallet
        .send_tx_with_pre_check(
            tx.accounts.clone(),
            0,
            tx.message.encode(),
            &catalog,
            None,
            AsyncDelivery::default(),
            fee_payer,
            |_| Ok(()),
        )
        .await
        .map_err(|e| anyhow!("private claim: {e:#}"))?;
    let block = settle(wallet, hash).await?;
    Ok((hash, block))
}

/// The token holding of the owned private account `account` as the wallet sees it after a sync:
/// `(definition, balance)`, `None` when it holds none.
#[must_use]
pub fn private_token_balance(wallet: &Wallet, token: AccountId, account: AccountId) -> Option<(AccountId, u128)> {
    let a = wallet.get_account_private(account)?;
    let bytes: &[u8] = a.data.actor_state(token).borrow();
    match token_core::TokenState::decode(bytes)? {
        token_core::TokenState::Holding(h) => Some((h.definition_id, h.balance)),
        token_core::TokenState::Definition(_) => None,
    }
}

/// The native balance of the owned private account `account` as the wallet sees it after a sync.
#[must_use]
pub fn private_native_balance(wallet: &Wallet, account: AccountId) -> Option<u128> {
    wallet
        .get_account_private(account)
        .and_then(|a| a.data.native_balance().ok())
}
