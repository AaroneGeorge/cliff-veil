//! `vesting-fees-admin`: sends the admin's messages of a `vesting_fees` deployment, and `CheckFee`, as public
//! transactions through the LEZ wallet library, then reads the config back.
//!
//! The wallet is the one `LEE_WALLET_HOME_DIR` names; it must hold the key of every signing account (the admin, or
//! for `init` the program's header account, whose key the deployer holds). `--payer` pays the network fee.
//!
//! ```text
//! vesting-fees-admin --program <id> --payer <id> init
//! vesting-fees-admin --program <id> --payer <id> set-fee --admin <id> --rate-bps 0 --treasury <id>
//! vesting-fees-admin --program <id> --payer <id> set-treasury --admin <id> --treasury <id>
//! vesting-fees-admin --program <id> --payer <id> check-fee --total 1000000 --fee 0 --treasury <id>
//! vesting-fees-admin --program <id> show
//! ```

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use lee_core::account::AccountId;
use lez_admin_authority::{AdminOp, Wire, admin_op};
use vesting_core::Asset;
use vesting_fees::{CONFIG_SEED, FeeConfig, FeeMessage, config_account, config_actor, wire};
use wallet::{AccountIdentity, AccountMention, DeclaredFee, WalletCore};

#[derive(Parser)]
#[command(about = "Admin client of the vesting_fees program")]
struct Cli {
    /// The program id (its loader header account).
    #[arg(long, value_parser = parse_id)]
    program: AccountId,
    /// The account that pays the network fee of each transaction.
    #[arg(long, value_parser = parse_id)]
    payer: Option<AccountId>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Installs the first admin: the program's own (header) account, which signs.
    Init,
    /// `SetFee`: sets the fee rate in basis points (at most 100) and the treasury.
    SetFee {
        #[arg(long, value_parser = parse_id)]
        admin: AccountId,
        #[arg(long)]
        rate_bps: u16,
        #[arg(long, value_parser = parse_id)]
        treasury: AccountId,
    },
    /// Sets the treasury and keeps the current rate (a `SetFee` with the rate read from the config).
    SetTreasury {
        #[arg(long, value_parser = parse_id)]
        admin: AccountId,
        #[arg(long, value_parser = parse_id)]
        treasury: AccountId,
    },
    /// Offers the admin role to `next` (all-zero id cancels).
    Nominate {
        #[arg(long, value_parser = parse_id)]
        admin: AccountId,
        #[arg(long, value_parser = parse_id)]
        next: AccountId,
    },
    /// The nominee takes the admin role.
    Accept {
        #[arg(long, value_parser = parse_id)]
        nominee: AccountId,
    },
    /// Gives the admin role up for good: the rate and treasury are frozen.
    Renounce {
        #[arg(long, value_parser = parse_id)]
        admin: AccountId,
    },
    /// Sends `CheckFee` for a native creation, as `vesting` does on every creation.
    CheckFee {
        #[arg(long)]
        total: u128,
        #[arg(long)]
        fee: u128,
        #[arg(long, value_parser = parse_id, default_value_t = AccountId::default())]
        treasury: AccountId,
        /// A token definition: check a token creation instead of a native one.
        #[arg(long, value_parser = parse_id)]
        definition: Option<AccountId>,
    },
    /// Prints the config.
    Show,
}

fn parse_id(text: &str) -> Result<AccountId, String> {
    text.trim_start_matches("Public/")
        .parse()
        .map_err(|error| format!("{error:?}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let wallet = WalletCore::from_env().context("opening the wallet (LEE_WALLET_HOME_DIR)")?;
    let program = cli.program;
    let seat = |account: AccountId| AccountIdentity::Public(account).select_program_actor_state(program);
    let config = || {
        AccountIdentity::PublicPda {
            program,
            seed: CONFIG_SEED,
        }
        .select_program_actor_state(program)
        .without_authorization()
    };
    let (accounts, message, what): (Vec<AccountMention>, Wire<FeeMessage>, &str) = match cli.command {
        Command::Show => {
            show(&wallet, program).await?;
            return Ok(());
        }
        Command::Init => (vec![seat(program), config()], admin_op(AdminOp::Init), "init"),
        Command::SetFee {
            admin,
            rate_bps,
            treasury,
        } => (
            vec![seat(admin), config()],
            wire(FeeMessage::SetFee {
                rate_bps,
                treasury: core_id(treasury),
            }),
            "set-fee",
        ),
        Command::SetTreasury { admin, treasury } => {
            let rate_bps = read(&wallet, program).await?.unwrap_or_default().rate_bps;
            (
                vec![seat(admin), config()],
                wire(FeeMessage::SetFee {
                    rate_bps,
                    treasury: core_id(treasury),
                }),
                "set-treasury",
            )
        }
        Command::Nominate { admin, next } => (
            vec![seat(admin), config()],
            admin_op(AdminOp::Nominate { next }),
            "nominate",
        ),
        Command::Accept { nominee } => (
            vec![seat(nominee), config()],
            admin_op(AdminOp::Accept { pda_seed: None }),
            "accept",
        ),
        Command::Renounce { admin } => (vec![seat(admin), config()], admin_op(AdminOp::Renounce), "renounce"),
        Command::CheckFee {
            total,
            fee,
            treasury,
            definition,
        } => {
            let asset = definition.map_or(Asset::Native, |definition| Asset::Token {
                definition: core_id(definition),
            });
            (
                vec![config()],
                wire(FeeMessage::CheckFee {
                    total,
                    fee,
                    treasury: core_id(treasury),
                    asset,
                }),
                "check-fee",
            )
        }
    };
    let payer = cli.payer.ok_or_else(|| anyhow!("--payer is required to send {what}"))?;
    let bytes = borsh::to_vec(&message)?;
    let hash = wallet
        .send_pub_tx_paid_by(accounts, 0, bytes, Some(payer))
        .await
        .map_err(|error| anyhow!("{what}: {error:?}"))?;
    let gas = match wallet.take_declared_fee() {
        Some(DeclaredFee::Public(fee)) => format!("{:?}", fee.gas_limit),
        _ => "none".to_owned(),
    };
    let (_tx, block) = wallet.poll_transaction(hash).await?;
    println!("{what}: tx {hash} included in block {block:?}, declared gas {gas}");
    show(&wallet, program).await
}

/// The config; `None` before `Init` (an empty config, which `CheckFee` reads as rate 0).
async fn read<C: wallet::WalletSequencerClient>(
    wallet: &WalletCore<C>,
    program: AccountId,
) -> Result<Option<FeeConfig>> {
    let state = match wallet.get_account_view(config_actor(program)).await? {
        None => return Ok(None),
        Some(account) => account.data.actor_state(program).to_vec(),
    };
    if state.is_empty() {
        return Ok(None);
    }
    borsh::from_slice(&state)
        .map(Some)
        .map_err(|_| anyhow!("the config does not decode"))
}

async fn show<C: wallet::WalletSequencerClient>(wallet: &WalletCore<C>, program: AccountId) -> Result<()> {
    match read(wallet, program).await? {
        None => println!(
            "config {}: empty (before Init: CheckFee reads rate 0)",
            config_account(program)
        ),
        Some(config) => println!(
            "config {}: rate_bps {}, treasury {}, admin {:?}",
            config_account(program),
            config.rate_bps,
            config.treasury,
            config.admin
        ),
    }
    Ok(())
}

fn core_id(id: AccountId) -> vesting_core::AccountId {
    vesting_core::AccountId::new(id.into_value())
}
