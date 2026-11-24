//! `cliff-veil`: command-line client for the `vesting` program, a thin layer over the SDK (`cliff_veil_sdk`).
//!
//! Every command that changes state first prints what it will do (checked against current state, with the
//! estimated network fee), then sends. A claim to a private account also prints the privacy disclosure and asks for
//! confirmation unless `--yes` is given. `--dry-run` stops after the check; `--json` prints one JSON object.
//!
//! The wallet is LEZ's, opened from `LEE_WALLET_HOME_DIR` (and `LEE_WALLET_PASSWORD`). Accounts are given as wallet
//! labels, `Public/<id>`, `Private/<id>`, or bare base58 ids. Program ids come from `--ids` (the file
//! `scripts/deploy_local.sh` writes) or `--vesting`.

mod input;
mod output;

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use cliff_veil_sdk::{
    Backend as _, ClaimTarget, Client, ListFilter, Outcome, Prepared, Programs, ScanCache, SequencerBackend, core_id,
    linear_terms, milestone_terms, schedule_id_from_label,
};
use lee_core::account::AccountId;
use vesting_core::{Asset, BatchItem, BatchShape, BatchTerms, Terms};

use crate::output::Printer;

/// Client of the cliff-veil vesting program on LEZ.
#[derive(Parser)]
#[command(name = "cliff-veil", version, about, long_about = None)]
struct Cli {
    /// Program ids file (`[programs]` table: vesting, vesting_fees, token, ata). Without it: `deploy/ids-local-dev.toml`
    /// in the working directory if it exists, else the file named by `CV_IDS` (an absolute path), else
    /// `ids-local-dev.toml` or `deploy/ids-local-dev.toml` next to the binary.
    #[arg(long, global = true)]
    ids: Option<PathBuf>,
    /// The vesting program id; overrides the ids file (then `--fees-program` and `--token-program` apply too).
    #[arg(long, env = "CV_VESTING", global = true)]
    vesting: Option<String>,
    /// The vesting_fees program id, with `--vesting`.
    #[arg(long, env = "CV_VESTING_FEES", global = true)]
    fees_program: Option<String>,
    /// The token program id, with `--vesting`.
    #[arg(long, env = "CV_TOKEN_PROGRAM", global = true)]
    token_program: Option<String>,
    /// Blocks of steepest base-fee rise every fee is priced for. A private claim's fee reserve is fixed in its
    /// proof, so it must cover the rise until inclusion.
    #[arg(long, env = "CV_FEE_HEADROOM_BLOCKS", default_value_t = cliff_veil_sdk::DEFAULT_FEE_HEADROOM_BLOCKS, global = true)]
    fee_headroom_blocks: u64,
    /// Print one JSON object instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// Do not ask for confirmation (private claims ask by default).
    #[arg(long, short = 'y', global = true)]
    yes: bool,
    /// Check and price the operation, print the summary, and send nothing.
    #[arg(long, global = true)]
    dry_run: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create one schedule (linear with optional cliff, or milestones), funded by the creator.
    Create(CreateArgs),
    /// Create one schedule per recipient of a CSV or JSON file, split into transactions that fit.
    CreateBatch(BatchArgs),
    /// Show a schedule: terms, vested, claimed, claimable now, locked, next unlock.
    Show {
        /// The schedule account.
        schedule: String,
    },
    /// List the schedules an account created (--creator) or can claim from now (--beneficiary), from a block scan.
    List {
        /// Schedules created by this account.
        #[arg(long, required_unless_present = "beneficiary", conflicts_with = "beneficiary")]
        creator: Option<String>,
        /// Schedules whose current beneficiary is this account (transfers included).
        #[arg(long)]
        beneficiary: Option<String>,
        /// Start the scan at this block (blocks before it are not read; a cache that is further along wins).
        #[arg(long)]
        from_block: Option<u64>,
        /// Scan cache file (default: `cliff-veil-scan-<vesting>.txt` in the wallet home).
        #[arg(long, conflicts_with = "no_cache")]
        cache: Option<PathBuf>,
        /// Scan without reading or writing a cache.
        #[arg(long)]
        no_cache: bool,
        /// Only schedules still active (not cancelled, not settled).
        #[arg(long)]
        active: bool,
    },
    /// Print the amount claimable now and the next unlock.
    Claimable {
        /// The schedule account.
        schedule: String,
    },
    /// Claim everything claimable now, to a public account or (with --private) to a private account.
    Claim(ClaimArgs),
    /// Cancel a schedule: the unvested part goes back to the refund account, the vested part stays claimable.
    Cancel {
        /// The schedule account.
        schedule: String,
        /// The cancel authority (signs).
        #[arg(long = "as")]
        signer: String,
    },
    /// Make a schedule non-cancelable, for good.
    MakeNonCancelable {
        /// The schedule account.
        schedule: String,
        /// The creator or the cancel authority (signs).
        #[arg(long = "as")]
        signer: String,
    },
    /// Signal a milestone, unlocking its amount.
    Signal {
        /// The schedule account.
        schedule: String,
        /// Milestone index (0-based).
        #[arg(long)]
        index: u8,
        /// The milestone authority (signs).
        #[arg(long = "as")]
        signer: String,
    },
    /// Hand a transferable schedule to a new beneficiary.
    Transfer {
        /// The schedule account.
        schedule: String,
        /// The new beneficiary.
        #[arg(long)]
        to: String,
        /// The current beneficiary (signs).
        #[arg(long = "as")]
        signer: String,
    },
    /// Print the schedule account of a creator and a schedule label (or hex id).
    Address {
        /// The creator.
        #[arg(long)]
        creator: String,
        /// The label the schedule id was derived from.
        #[arg(long, required_unless_present = "id", conflicts_with = "id")]
        label: Option<String>,
        /// The schedule id as 64 hex characters.
        #[arg(long)]
        id: Option<String>,
    },
    /// Show the creation fee configuration (rate, cap, treasury).
    FeeShow,
    /// Print the privacy disclosure shown before every private claim.
    Disclosure,
}

#[derive(Args)]
struct CreateArgs {
    /// The creator: signs and funds the escrow (and the creation fee) from its own account.
    #[arg(long)]
    from: String,
    /// Who may claim.
    #[arg(long)]
    beneficiary: String,
    /// Total amount (linear schedules; a milestone schedule's total is the sum of its amounts).
    #[arg(long, required_unless_present = "milestones")]
    total: Option<u128>,
    /// Vesting start: ms since the epoch, `now`, or an offset like `+10m` (default: now).
    #[arg(long, default_value = "now")]
    start: String,
    /// Cliff (default: the start, i.e. fully linear). At the cliff the amount vested since the start unlocks at once.
    #[arg(long)]
    cliff: Option<String>,
    /// Vesting end (linear schedules).
    #[arg(long, required_unless_present = "milestones", conflicts_with = "milestones")]
    end: Option<String>,
    /// Milestone amounts, comma-separated (`300,700`): a milestone schedule.
    #[arg(long)]
    milestones: Option<String>,
    #[command(flatten)]
    common: CommonTerms,
    /// Schedule id derived from this label (SHA-256); default: a random id.
    #[arg(long, conflicts_with = "id")]
    label: Option<String>,
    /// Schedule id as 64 hex characters.
    #[arg(long)]
    id: Option<String>,
}

#[derive(Args)]
struct CommonTerms {
    /// Vest a token of this definition instead of native LEZ.
    #[arg(long)]
    token: Option<String>,
    /// Create the schedule non-cancelable (default: cancelable).
    #[arg(long)]
    non_cancelable: bool,
    /// Let the beneficiary transfer the schedule (default: not transferable).
    #[arg(long)]
    transferable: bool,
    /// Cancel authority (default: the creator).
    #[arg(long)]
    cancel_authority: Option<String>,
    /// Milestone authority (default: the creator).
    #[arg(long)]
    milestone_authority: Option<String>,
    /// Refund account on cancel (default: the creator, which funds the escrow).
    #[arg(long)]
    refund_to: Option<String>,
}

#[derive(Args)]
struct BatchArgs {
    /// The creator: signs and funds every escrow.
    #[arg(long)]
    from: String,
    /// Recipients: CSV (`beneficiary,total[,label]`) or a JSON array (`[{"beneficiary","total","label"}]`).
    #[arg(long)]
    file: PathBuf,
    /// Vesting start, as for `create` (linear batches).
    #[arg(long, default_value = "now")]
    start: String,
    /// Cliff, as for `create`.
    #[arg(long)]
    cliff: Option<String>,
    /// Vesting end (linear batches).
    #[arg(long, required_unless_present = "milestone_bps", conflicts_with = "milestone_bps")]
    end: Option<String>,
    /// Milestone weights in basis points summing to 10000 (`2500,7500`): every item gets these shares of its total.
    #[arg(long)]
    milestone_bps: Option<String>,
    /// Most schedules per transaction (default and upper bound: what the cycle model fits under 85% of the budget).
    #[arg(long)]
    per_tx: Option<usize>,
    #[command(flatten)]
    common: CommonTerms,
}

#[derive(Args)]
struct ClaimArgs {
    /// The schedule account.
    schedule: String,
    /// The beneficiary (signs).
    #[arg(long = "as")]
    signer: String,
    /// Public destination (default: the beneficiary itself).
    #[arg(long, conflicts_with = "private")]
    to: Option<String>,
    /// Claim to a private account of this wallet, through a one-time blinded address.
    #[arg(long)]
    private: bool,
    /// The private account that receives the claim (default: a fresh private account).
    #[arg(long, requires = "private")]
    private_to: Option<String>,
    /// A private account of this wallet that pays the network fee (default: the destination, out of the claim).
    #[arg(long, requires = "private")]
    fee_payer: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            let help = matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
            if !help && std::env::args().any(|a| a == "--json") {
                let error = serde_json::json!({
                    "code": null, "name": "Usage", "message": e.to_string().trim(), "action": "see `cliff-veil --help`",
                });
                println!("{}", serde_json::json!({ "ok": false, "error": error }));
                return ExitCode::from(2);
            }
            e.exit();
        }
    };
    let printer = Printer { json: cli.json };
    match run(&cli, &printer).await {
        Ok(code) => code,
        Err(e) => {
            printer.error(&e);
            ExitCode::FAILURE
        }
    }
}

fn programs(cli: &Cli) -> Result<Programs> {
    let parse = |s: &str| -> Result<AccountId> { s.parse().map_err(|e| anyhow!("`{s}` is not an account id: {e:?}")) };
    if let Some(v) = &cli.vesting {
        return Ok(Programs {
            vesting: parse(v)?,
            fees: cli.fees_program.as_deref().map(parse).transpose()?,
            token: cli.token_program.as_deref().map(parse).transpose()?,
            ata: None,
        });
    }
    let path = ids_path(cli.ids.as_deref())?;
    Programs::from_ids_file(&path).with_context(|| format!("program ids from {}", path.display()))
}

/// The ids file name `scripts/deploy_local.sh dev` writes.
const DEFAULT_IDS: &str = "deploy/ids-local-dev.toml";

/// Where the program ids file is: `--ids`; else [`DEFAULT_IDS`] relative to the working directory if it exists;
/// else `CV_IDS`, which must be absolute; else next to the binary.
fn ids_path(explicit: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let mut tried = vec![PathBuf::from(DEFAULT_IDS)];
    if tried[0].is_file() {
        return Ok(tried.remove(0));
    }
    if let Some(env) = std::env::var_os("CV_IDS") {
        let p = PathBuf::from(env);
        if !p.is_absolute() {
            bail!("CV_IDS must be an absolute path (it is `{}`)", p.display());
        }
        if p.is_file() {
            return Ok(p);
        }
        tried.push(p);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(std::path::Path::to_path_buf))
    {
        for p in [dir.join("ids-local-dev.toml"), dir.join(DEFAULT_IDS)] {
            if p.is_file() {
                return Ok(p);
            }
            tried.push(p);
        }
    }
    let tried: Vec<String> = tried.iter().map(|p| p.display().to_string()).collect();
    bail!(
        "no program ids file: give --ids or --vesting, or set CV_IDS to an absolute path (tried {})",
        tried.join(", ")
    )
}

/// The default scan cache: in the wallet home (`LEE_WALLET_HOME_DIR`), one file per `vesting` deployment.
fn default_cache(vesting: AccountId) -> PathBuf {
    let home = std::env::var_os("LEE_WALLET_HOME_DIR").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(format!("cliff-veil-scan-{vesting}.txt"))
}

async fn run(cli: &Cli, out: &Printer) -> Result<ExitCode> {
    if matches!(cli.cmd, Cmd::Disclosure) {
        out.disclosure();
        return Ok(ExitCode::SUCCESS);
    }
    let mut backend = SequencerBackend::from_env(programs(cli)?)?;
    backend.set_fee_headroom_blocks(cli.fee_headroom_blocks);
    let mut client = Client::new(backend);
    let resolve = |c: &Client<SequencerBackend>, s: &str| c.backend().resolve(s);
    let schedule_of = |c: &Client<SequencerBackend>, s: &str| -> Result<AccountId> { Ok(resolve(c, s)?) };

    match &cli.cmd {
        Cmd::Disclosure => unreachable!("handled above"),
        Cmd::Show { schedule } => {
            let view = client.status(schedule_of(&client, schedule)?).await?;
            out.schedule(&view);
        }
        Cmd::List {
            creator,
            beneficiary,
            from_block,
            cache,
            no_cache,
            active,
        } => {
            let filter = match (creator, beneficiary) {
                (Some(c), _) => ListFilter::Creator(resolve(&client, c)?),
                (None, Some(b)) => ListFilter::Beneficiary(resolve(&client, b)?),
                (None, None) => bail!("give --creator or --beneficiary"),
            };
            let vesting = client.programs().vesting;
            let path = if *no_cache {
                None
            } else {
                Some(cache.clone().unwrap_or_else(|| default_cache(vesting)))
            };
            let mut scan = match &path {
                Some(p) if p.is_file() => ScanCache::from_text(&std::fs::read_to_string(p)?)?,
                _ => ScanCache::new(vesting),
            };
            let mut views = client.list(filter, &mut scan, *from_block).await?;
            if let Some(p) = &path {
                let tmp = p.with_extension("tmp");
                std::fs::write(&tmp, scan.to_text()).with_context(|| format!("write {}", tmp.display()))?;
                std::fs::rename(&tmp, p).with_context(|| format!("write {}", p.display()))?;
            }
            if *active {
                views.retain(|v| v.standing == cliff_veil_sdk::Standing::Active);
            }
            out.list(&views, scan.next_block.saturating_sub(1));
        }
        Cmd::Claimable { schedule } => {
            let view = client.status(schedule_of(&client, schedule)?).await?;
            out.claimable(&view);
        }
        Cmd::Address { creator, label, id } => {
            let id = match (label, id) {
                (Some(l), _) => schedule_id_from_label(l),
                (None, Some(h)) => input::parse_id_hex(h)?,
                (None, None) => bail!("give --label or --id"),
            };
            let account = client.programs().schedule_account(resolve(&client, creator)?, &id);
            out.address(account);
        }
        Cmd::FeeShow => {
            let config = client.fee_config().await?;
            out.fees(&config, client.programs());
        }
        Cmd::Create(a) => {
            let creator = resolve(&client, &a.from)?;
            let beneficiary = resolve(&client, &a.beneficiary)?;
            let now = client.backend().now().await?;
            let asset = asset(&client, a.common.token.as_deref())?;
            let mut terms = if let Some(m) = &a.milestones {
                milestone_terms(beneficiary, asset, input::parse_amounts(m)?)
            } else {
                let start = input::parse_time(&a.start, now)?;
                let cliff = a.cliff.as_deref().map_or(Ok(start), |c| input::parse_time(c, now))?;
                let end = input::parse_time(a.end.as_deref().context("--end is required")?, now)?;
                linear_terms(
                    beneficiary,
                    asset,
                    a.total.context("--total is required")?,
                    start,
                    cliff,
                    end,
                )
            };
            if let (Some(total), Some(_)) = (a.total, &a.milestones)
                && total != terms.total
            {
                bail!("--total {total} differs from the sum of --milestones ({})", terms.total);
            }
            apply_common(&client, &a.common, &mut terms)?;
            let id = match (&a.label, &a.id) {
                (Some(l), _) => schedule_id_from_label(l),
                (None, Some(h)) => input::parse_id_hex(h)?,
                (None, None) => client.fresh_schedule_id(),
            };
            let prepared = client.prepare_create(creator, id, terms).await?;
            out.prepared("create", &prepared, Some(id));
            send_all(cli, out, &mut client, &[prepared]).await?;
        }
        Cmd::CreateBatch(a) => {
            let creator = resolve(&client, &a.from)?;
            let now = client.backend().now().await?;
            let text = std::fs::read_to_string(&a.file).with_context(|| format!("read {}", a.file.display()))?;
            let rows = input::parse_batch(&text)?;
            let shape = if let Some(bps) = &a.milestone_bps {
                BatchShape::Milestones {
                    bps: input::parse_bps(bps)?,
                }
            } else {
                let start = input::parse_time(&a.start, now)?;
                let cliff = a.cliff.as_deref().map_or(Ok(start), |c| input::parse_time(c, now))?;
                let end = input::parse_time(a.end.as_deref().context("--end is required")?, now)?;
                BatchShape::Linear { start, cliff, end }
            };
            let mut probe = linear_terms(creator, asset(&client, a.common.token.as_deref())?, 1, 0, 0, 1);
            apply_common(&client, &a.common, &mut probe)?;
            let common = BatchTerms {
                asset: probe.asset,
                shape,
                cancelable: probe.cancelable,
                transferable: probe.transferable,
                cancel_authority: probe.cancel_authority,
                milestone_authority: probe.milestone_authority,
                refund_to: probe.refund_to,
            };
            let mut items = Vec::with_capacity(rows.len());
            for row in rows {
                let schedule_id = match &row.label {
                    Some(l) => schedule_id_from_label(l),
                    None => client.fresh_schedule_id(),
                };
                items.push(BatchItem {
                    schedule_id,
                    beneficiary: core_id(resolve(&client, &row.beneficiary)?),
                    total: row.total,
                });
            }
            let prepared = client.prepare_create_batch(creator, common, items, a.per_tx).await?;
            for p in &prepared {
                out.prepared("create-batch", p, None);
            }
            send_all(cli, out, &mut client, &prepared).await?;
        }
        Cmd::Claim(a) => {
            let beneficiary = resolve(&client, &a.signer)?;
            let schedule = schedule_of(&client, &a.schedule)?;
            let target = if a.private {
                ClaimTarget::Private {
                    account: a.private_to.as_deref().map(|s| resolve(&client, s)).transpose()?,
                    fee_payer: a.fee_payer.as_deref().map(|s| resolve(&client, s)).transpose()?,
                }
            } else {
                ClaimTarget::Public(a.to.as_deref().map_or(Ok(beneficiary), |s| resolve(&client, s))?)
            };
            let prepared = client.prepare_claim(beneficiary, schedule, target).await?;
            out.prepared("claim", &prepared, None);
            if prepared.tx.is_private() && !cli.dry_run && !cli.yes && !confirm(out)? {
                out.note("not sent: the claim was not confirmed");
                out.finish();
                return Ok(ExitCode::from(3));
            }
            send_all(cli, out, &mut client, &[prepared]).await?;
        }
        Cmd::Cancel { schedule, signer } => {
            let p = client
                .prepare_cancel(resolve(&client, signer)?, schedule_of(&client, schedule)?)
                .await?;
            out.prepared("cancel", &p, None);
            send_all(cli, out, &mut client, &[p]).await?;
        }
        Cmd::MakeNonCancelable { schedule, signer } => {
            let p = client
                .prepare_make_non_cancelable(resolve(&client, signer)?, schedule_of(&client, schedule)?)
                .await?;
            out.prepared("make-non-cancelable", &p, None);
            send_all(cli, out, &mut client, &[p]).await?;
        }
        Cmd::Signal {
            schedule,
            index,
            signer,
        } => {
            let p = client
                .prepare_signal(resolve(&client, signer)?, schedule_of(&client, schedule)?, *index)
                .await?;
            out.prepared("signal", &p, None);
            send_all(cli, out, &mut client, &[p]).await?;
        }
        Cmd::Transfer { schedule, to, signer } => {
            let p = client
                .prepare_transfer(
                    resolve(&client, signer)?,
                    schedule_of(&client, schedule)?,
                    resolve(&client, to)?,
                )
                .await?;
            out.prepared("transfer", &p, None);
            send_all(cli, out, &mut client, &[p]).await?;
        }
    }
    out.finish();
    Ok(ExitCode::SUCCESS)
}

fn asset(client: &Client<SequencerBackend>, token: Option<&str>) -> Result<Asset> {
    Ok(match token {
        None => Asset::Native,
        Some(d) => Asset::Token {
            definition: core_id(client.backend().resolve(d)?),
        },
    })
}

fn apply_common(client: &Client<SequencerBackend>, c: &CommonTerms, terms: &mut Terms) -> Result<()> {
    let opt = |s: &Option<String>| -> Result<Option<vesting_core::AccountId>> {
        s.as_deref()
            .map(|s| client.backend().resolve(s).map(core_id))
            .transpose()
            .map_err(Into::into)
    };
    terms.cancelable = !c.non_cancelable;
    terms.transferable = c.transferable;
    terms.cancel_authority = opt(&c.cancel_authority)?;
    terms.milestone_authority = opt(&c.milestone_authority)?;
    terms.refund_to = opt(&c.refund_to)?;
    Ok(())
}

/// Asks on the terminal; refuses when there is none (use `--yes` after reading the disclosure).
fn confirm(out: &Printer) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "confirmation needed and stdin is not a terminal: read the summary and disclosure above, then rerun with --yes"
        );
    }
    out.prompt("Send this private claim? [y/N] ");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

async fn send_all(
    cli: &Cli,
    out: &Printer,
    client: &mut Client<SequencerBackend>,
    prepared: &[Prepared],
) -> Result<()> {
    if cli.dry_run {
        out.note("dry run: nothing sent");
        return Ok(());
    }
    for p in prepared {
        let receipt = client.send(p).await?;
        out.sent(&receipt);
        if let Outcome::Claim(_) = p.outcome
            && p.tx.is_private()
        {
            out.private_balance(client, p);
        }
    }
    std::io::stdout().flush().ok();
    Ok(())
}
