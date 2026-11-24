//! Token setup for the token tests: a definition with its whole supply in one holding, holdings, balances. The token
//! program is LEZ's (lez-programs at the pinned commit), deployed next to `vesting`.

use anyhow::{Result, anyhow};
use lee_core::account::AccountId;
use token_core::{Message, TokenState};

use crate::Ctx;

/// Creates a fixed-supply token: `definition_party`'s account becomes the definition and `holder_party`'s account
/// receives the whole `supply` (both sign). Returns the definition id.
///
/// # Errors
/// The transaction was refused or not included.
pub async fn new_definition(
    ctx: &Ctx,
    token: AccountId,
    definition_party: &str,
    holder_party: &str,
    supply: u128,
) -> Result<AccountId> {
    let message = Message::NewDefinition {
        name: "VEST".to_owned(),
        total_supply: supply,
        mint_authority: None,
        holding: ctx.id(holder_party),
    };
    ctx.send(
        vec![ctx.signer(definition_party, token), ctx.signer(holder_party, token)],
        borsh::to_vec(&message)?,
    )
    .await
    .map_err(|e| anyhow!("token definition: {e}"))?;
    Ok(ctx.id(definition_party))
}

/// `party` initialises its own empty holding of `definition`.
///
/// # Errors
/// The transaction was refused or not included.
pub async fn init_holding(ctx: &Ctx, token: AccountId, party: &str, definition: AccountId) -> Result<()> {
    let def = wallet::AccountIdentity::PublicNoSign(definition).select_program_actor_state(token);
    ctx.send(
        vec![ctx.signer(party, token), def],
        borsh::to_vec(&Message::InitializeHolding { definition })?,
    )
    .await
    .map_err(|e| anyhow!("initialise holding: {e}"))?;
    Ok(())
}

/// The token holding of `account`: `(definition, balance)`, `None` when it has none.
///
/// # Errors
/// RPC failure, or the token actor state is a definition.
pub async fn balance(ctx: &Ctx, token: AccountId, account: AccountId) -> Result<Option<(AccountId, u128)>> {
    match TokenState::decode(&ctx.actor_state(account, token).await?) {
        None => Ok(None),
        Some(TokenState::Holding(h)) => Ok(Some((h.definition_id, h.balance))),
        Some(TokenState::Definition(_)) => Err(anyhow!("{account} is a token definition, not a holding")),
    }
}

/// Moves `amount` from `party`'s holding to the holding at `to` (which must hold the same token).
///
/// # Errors
/// The transaction was refused or not included.
pub async fn transfer(ctx: &Ctx, token: AccountId, party: &str, to: AccountId, amount: u128) -> Result<()> {
    let recipient = wallet::AccountIdentity::PublicNoSign(to).select_program_actor_state(token);
    ctx.send(
        vec![ctx.signer(party, token), recipient],
        borsh::to_vec(&Message::Transfer { to, amount })?,
    )
    .await
    .map_err(|e| anyhow!("token transfer: {e}"))?;
    Ok(())
}

/// The associated token account of `owner` for `definition` under the ATA program `ata` (SPEC 6): the account a
/// token creation fee is paid to when `owner` is the treasury.
#[must_use]
pub fn ata_account(ata: AccountId, token: AccountId, owner: AccountId, definition: AccountId) -> AccountId {
    use crate::vesting::core_id;
    let seed = vesting_core::ata_seed(&core_id(token), &core_id(owner), &core_id(definition));
    AccountId::for_public_pda(&ata, &lee_core::program::PdaSeed::new(seed))
}

/// The ATA program's `Create` (message tag 0: token program, owner, definition), sent by `party` from its own actor
/// of the ATA program. Permissionless; a repeated `Create` keeps the existing holding.
///
/// # Errors
/// The transaction was refused or not included.
pub async fn create_ata(
    ctx: &Ctx,
    ata: AccountId,
    token: AccountId,
    party: &str,
    owner: AccountId,
    definition: AccountId,
) -> Result<AccountId> {
    let holding = ata_account(ata, token, owner, definition);
    let mut message = vec![0u8];
    for id in [token, owner, definition] {
        message.extend_from_slice(id.value());
    }
    ctx.send(
        vec![
            ctx.signer(party, ata),
            wallet::AccountIdentity::PublicNoSign(holding).select_program_actor_state(token),
            wallet::AccountIdentity::PublicNoSign(definition).select_program_actor_state(token),
        ],
        message,
    )
    .await
    .map_err(|e| anyhow!("ATA create: {e}"))?;
    Ok(holding)
}
