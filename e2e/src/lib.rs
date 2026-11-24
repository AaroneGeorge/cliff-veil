//! Client harness for the end-to-end tests in `tests/`: a standalone LEZ v0.4.0-rc3 sequencer, the deployed programs,
//! funded test accounts, transactions sent and waited for, account state read back.
//!
//! # Environment
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `CV_SEQ_RPC` | `http://127.0.0.1:3050` | RPC endpoint of the sequencer (`scripts/seq.sh dev start`) |
//! | `CV_IDS` | `deploy/ids-local-dev.toml` | program ids written by `scripts/deploy_local.sh` (relative to the repo root) |
//! | `CV_IDS_FEES` | `deploy/ids-local-dev-fees.toml` | the fee-enabled pair of `e2e/deploy_fees.sh` (`tests/fees_on.rs`) |
//! | `CV_E2E_RECORD` | unset | a file the tests append measured figures to (`test<TAB>key<TAB>value` lines) |
//! | `CV_LEZ_DIR` | `../vendor/lez` | LEZ checkout; the funding key is LEZ's published local test key from its `Justfile` |
//! | `CV_E2E_FUNDER_INDEX` | `1` | which of the `Justfile` test keys funds the tests (0 is the one deployment pays with) |
//! | `CV_E2E_FUND` | `1000000000` | native units given to every test account |
//! | `CV_E2E_TX_TIMEOUT` | `180` | seconds to wait for a transaction to be included |
//!
//! Every test that needs the sequencer is `#[ignore]`d, so `cargo test` stays offline and fast; the CI job `e2e`
//! (and `e2e/run.sh`) runs them with `--ignored`. Tests that wait for a program message not yet built carry
//! `#[ignore = "pending: ..."]` and are skipped by `e2e/run.sh` until then.
//!
//! # Isolation
//!
//! Each test gets fresh accounts with fresh random keys ([`Ctx::new`]), funded from the genesis test account, and
//! its own schedule ids, so tests run concurrently against one sequencer without sharing any account. The funder
//! is the only shared account: its transfers are numbered with explicit nonces under one lock, so many tests are
//! funded in the same block.
//!
//! # Time
//!
//! The sequencer ticks the LEZ clock as the last transaction of every block, so a transaction in block `n` reads the
//! timestamp of block `n - 1`. [`Ctx::clock`] returns exactly that value (the clock account), and
//! [`Ctx::wait_clock`] waits until the clock has reached a time.

pub mod private;
pub mod sdk;
pub mod token;
pub mod vesting;

use std::borrow::Borrow as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use common::{HashType, transaction::LeeTransaction};
use lee::{
    FeeDeclaration, PrivateKey, PublicAccountEvidence, PublicKey, PublicTransaction,
    public_transaction::{Message as TxMessage, WitnessSet},
};
use lee_core::{
    account::{AccountId, Actor, Nonce},
    native_token::{self, NATIVE_TOKEN_PROGRAM_ID},
};
use sequencer_service_rpc::{RpcClient as _, SequencerClient, SequencerClientBuilder};
use wallet::{AccountIdentity, AccountMention, ExecutionFailureKind, WalletCore};

/// RPC endpoint of the dev sequencer started by `scripts/seq.sh dev start`.
pub const DEFAULT_RPC: &str = "http://127.0.0.1:3050";
/// Program ids file of the dev sequencer, relative to the repository root.
pub const DEFAULT_IDS: &str = "deploy/ids-local-dev.toml";
/// Program ids of the fee-enabled pair (`e2e/deploy_fees.sh`), relative to the repository root.
pub const DEFAULT_IDS_FEES: &str = "deploy/ids-local-dev-fees.toml";
/// The chain's execution cap per transaction and per block (rc3 fee market `MAX_GAS_EXEC`).
pub const MAX_GAS_EXEC: u64 = 10_000_000;
/// Blocks of the steepest base-fee climb a fee is priced to cover (the wallet's default is 5). Tests fill blocks, so
/// the base fee climbs fast; a private claim's reserve is fixed inside its proof, and with the default a claim that
/// waits a block or two can find its reserve buying too little gas and fail settlement (seen on the local network).
const FEE_HEADROOM_BLOCKS: u64 = 12;
/// Gas limit of a funding transfer (a native transfer runs as protocol code, far below this).
const FUND_GAS: u64 = 1_000_000;

/// The sequencer RPC endpoint the tests use: `CV_SEQ_RPC`, else [`DEFAULT_RPC`].
#[must_use]
pub fn rpc_url() -> String {
    std::env::var("CV_SEQ_RPC").unwrap_or_else(|_| DEFAULT_RPC.to_owned())
}

/// `host:port` of an `http://` endpoint, for a plain TCP reachability check.
#[must_use]
pub fn host_port(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("http://")?;
    let hp = rest.split('/').next()?;
    hp.contains(':').then_some(hp)
}

/// The `cliff-veil` CLI binary: `CV_CLI_BIN`, else `$CARGO_TARGET_DIR/debug/cliff-veil` (`e2e/run.sh` builds it).
#[must_use]
pub fn cli_bin() -> PathBuf {
    std::env::var("CV_CLI_BIN").map_or_else(
        |_| {
            let target = std::env::var("CARGO_TARGET_DIR").map_or_else(|_| repo_root().join("target"), PathBuf::from);
            target.join("debug").join("cliff-veil")
        },
        PathBuf::from,
    )
}

/// The repository root (this crate's parent directory).
#[must_use]
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

// ---------------------------------------------------------------- program ids

/// Program ids of one network, from the `[programs]` and `[accounts]` tables of a `deploy/ids-*.toml` file.
#[derive(Debug, Clone, Default)]
pub struct Ids {
    /// `name -> id` for every line of `[programs]` and `[accounts]` (`fees_config`).
    pub programs: Vec<(String, AccountId)>,
}

impl Ids {
    /// Parses the `[programs]` and `[accounts]` tables (`name = "<base58 id>"   # comment`); other tables are
    /// ignored.
    ///
    /// # Errors
    /// A line of the table whose value is not an account id.
    pub fn parse(text: &str) -> Result<Self> {
        let mut programs = Vec::new();
        let mut in_programs = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') {
                in_programs = line == "[programs]" || line == "[accounts]";
                continue;
            }
            if !in_programs || line.is_empty() {
                continue;
            }
            let (name, value) = line
                .split_once('=')
                .with_context(|| format!("bad line in [programs]: {line}"))?;
            let value = value.trim().trim_matches('"');
            let id: AccountId = value
                .parse()
                .map_err(|e| anyhow!("{}: not an account id: {e:?}", name.trim()))?;
            programs.push((name.trim().to_owned(), id));
        }
        Ok(Self { programs })
    }

    /// The ids file: `CV_IDS`, else [`DEFAULT_IDS`] under the repository root.
    #[must_use]
    pub fn path() -> PathBuf {
        std::env::var("CV_IDS").map_or_else(|_| repo_root().join(DEFAULT_IDS), PathBuf::from)
    }

    /// Reads [`Self::path`].
    ///
    /// # Errors
    /// The file is missing or does not parse.
    pub fn load() -> Result<Self> {
        let path = Self::path();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read {} (deploy with scripts/deploy_local.sh)", path.display()))?;
        Self::parse(&text)
    }

    /// The fee-enabled pair's ids file: `CV_IDS_FEES`, else [`DEFAULT_IDS_FEES`] under the repository root.
    #[must_use]
    pub fn fees_path() -> PathBuf {
        std::env::var("CV_IDS_FEES").map_or_else(|_| repo_root().join(DEFAULT_IDS_FEES), PathBuf::from)
    }

    /// The id of program `name`, if deployed.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<AccountId> {
        self.programs.iter().find(|(n, _)| n == name).map(|(_, id)| *id)
    }

    /// The id of program `name`.
    ///
    /// # Errors
    /// The program is not in the ids file.
    pub fn require(&self, name: &str) -> Result<AccountId> {
        self.get(name)
            .with_context(|| format!("program `{name}` is not deployed on this sequencer (ids file)"))
    }
}

// ---------------------------------------------------------------- shared funder

/// The one account shared by every test: one of LEZ's genesis test accounts, which funds each test's accounts.
struct Funder {
    key: PrivateKey,
    id: AccountId,
    /// Next nonce to use; `None` re-reads it from the chain.
    next_nonce: tokio::sync::Mutex<Option<u128>>,
}

fn funder() -> Result<&'static Funder> {
    static FUNDER: OnceLock<Funder> = OnceLock::new();
    if let Some(f) = FUNDER.get() {
        return Ok(f);
    }
    let key = funder_key()?;
    let id = account_of(&key);
    Ok(FUNDER.get_or_init(|| Funder {
        key,
        id,
        next_nonce: tokio::sync::Mutex::new(None),
    }))
}

/// LEZ's published local test key, read from its `Justfile` at run time (the same key `scripts/deploy_local.sh`
/// imports). Never a secret: it funds only local test networks.
fn funder_key() -> Result<PrivateKey> {
    let lez = std::env::var("CV_LEZ_DIR").map_or_else(|_| repo_root().join("../vendor/lez"), PathBuf::from);
    let justfile = lez.join("Justfile");
    let text = std::fs::read_to_string(&justfile).with_context(|| format!("read {}", justfile.display()))?;
    // The Justfile lists two genesis accounts; deployment (scripts/deploy_local.sh) pays with the first, the tests
    // fund from the second (CV_E2E_FUNDER_INDEX), so a deployment during a test run never races the tests' nonces.
    let index = usize::try_from(env_u64("CV_E2E_FUNDER_INDEX", 1)).unwrap_or(1);
    let hex_key = text
        .lines()
        .filter_map(|l| l.split("account import public --private-key ").nth(1))
        .map(|rest| rest.trim().chars().take(64).collect::<String>())
        .nth(index)
        .context("no such local test key in the LEZ Justfile")?;
    let bytes: [u8; 32] = hex::decode(&hex_key)?
        .try_into()
        .map_err(|_| anyhow!("test key is not 32 bytes"))?;
    PrivateKey::try_new(bytes).map_err(|e| anyhow!("test key: {e:?}"))
}

/// The account of the shared funder (LEZ's local test key `CV_E2E_FUNDER_INDEX`).
///
/// # Errors
/// The LEZ `Justfile` or the key is missing.
pub fn funder_account() -> Result<AccountId> {
    Ok(funder()?.id)
}

/// Appends a measured figure to the file `CV_E2E_RECORD` names (`test<TAB>key<TAB>value`), and prints it (shown by
/// libtest on failure or with `--nocapture`). The run script turns the lines into the run's JSON summary.
pub fn record(test: &str, key: &str, value: impl std::fmt::Display) {
    use std::io::Write as _;
    let line = format!("{test}\t{key}\t{value}");
    println!("record: {line}");
    if let Ok(path) = std::env::var("CV_E2E_RECORD")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// The account id a key controls.
#[must_use]
pub fn account_of(key: &PrivateKey) -> AccountId {
    AccountId::from(&PublicKey::new_from_private_key(key))
}

// ---------------------------------------------------------------- per-test context

/// A test's view of the chain: its own wallet (in a fresh directory), its own funded accounts, the program ids.
pub struct Ctx {
    /// Test name (directory and schedule-id prefix).
    pub name: String,
    /// Program ids of the sequencer.
    pub ids: Ids,
    ids_path: PathBuf,
    rpc: String,
    client: SequencerClient,
    wallet: WalletCore<SequencerClient>,
    parties: Vec<(String, PrivateKey)>,
    dir: PathBuf,
    counter: u32,
}

/// A transaction that was included.
#[derive(Debug, Clone)]
pub struct Included {
    /// Its hash.
    pub hash: HashType,
    /// The block that holds it.
    pub block: u64,
    /// The fee cap the transaction declared (public transactions sent through the test's wallet).
    pub max_fee: Option<u128>,
}

/// Why a send did not produce an included transaction.
#[derive(Debug)]
pub enum SendError {
    /// The wallet simulated the transaction against current state and it reverted; nothing was sent. `code` is the
    /// program's error code when the reason carries `cliff-veil error <code>`.
    Refused {
        /// Program error code (SPEC section 5), if the refusal came from the vesting program.
        code: Option<u32>,
        /// The full revert reason.
        reason: String,
    },
    /// Anything else (RPC, signing, not included in time).
    Other(anyhow::Error),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { code, reason } => write!(f, "refused (code {code:?}): {reason}"),
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for SendError {}

impl SendError {
    /// The program error code of a refusal.
    #[must_use]
    pub const fn code(&self) -> Option<u32> {
        match self {
            Self::Refused { code, .. } => *code,
            Self::Other(_) => None,
        }
    }
}

impl Ctx {
    /// A context with one funded account per name in `parties` (each gets `CV_E2E_FUND` native units).
    ///
    /// # Errors
    /// The sequencer is unreachable, the ids file or the funding key is missing, or funding was not included.
    pub async fn new(name: &str, parties: &[&str]) -> Result<Self> {
        Self::with_rpc(&rpc_url(), name, parties).await
    }

    /// As [`Self::new`], with the programs of the ids file at `ids` (for example [`Ids::fees_path`]).
    ///
    /// # Errors
    /// As [`Self::new`]; the ids file is missing.
    pub async fn new_with_ids(ids: &Path, name: &str, parties: &[&str]) -> Result<Self> {
        ensure!(ids.exists(), "{} is missing", ids.display());
        Self::open(&rpc_url(), ids.to_path_buf(), name, parties).await
    }

    /// As [`Self::new`], against the sequencer at `rpc`.
    ///
    /// # Errors
    /// As [`Self::new`].
    pub async fn with_rpc(rpc: &str, name: &str, parties: &[&str]) -> Result<Self> {
        Self::open(rpc, Ids::path(), name, parties).await
    }

    async fn open(rpc: &str, ids_path: PathBuf, name: &str, parties: &[&str]) -> Result<Self> {
        let rpc = rpc.to_owned();
        let client = SequencerClientBuilder::default()
            .build(&rpc)
            .with_context(|| format!("rpc client for {rpc}"))?;
        client
            .check_health()
            .await
            .map_err(|e| anyhow!("sequencer {rpc} not healthy: {e}"))?;
        let ids = std::fs::read_to_string(&ids_path)
            .ok()
            .and_then(|text| Ids::parse(&text).ok())
            .unwrap_or_default();

        let dir = std::env::temp_dir().join(format!("cv-e2e-{}-{name}-{}", std::process::id(), nonce_seed()));
        let mut wallet = open_wallet(&rpc, &dir)?;

        let parties: Vec<(String, PrivateKey)> = parties
            .iter()
            .map(|p| ((*p).to_owned(), PrivateKey::new_os_random()))
            .collect();
        for (_, key) in &parties {
            wallet
                .storage_mut()
                .key_chain_mut()
                .add_imported_public_account(key.clone());
        }
        let ctx = Self {
            name: name.to_owned(),
            rpc,
            ids,
            ids_path,
            client,
            wallet,
            parties,
            dir,
            counter: 0,
        };
        let amount = u128::from(env_u64("CV_E2E_FUND", 1_000_000_000));
        let keys: Vec<PrivateKey> = ctx.parties.iter().map(|(_, k)| k.clone()).collect();
        ctx.fund(&keys, amount).await?;
        Ok(ctx)
    }

    /// The account of party `name`.
    ///
    /// # Panics
    /// No such party in this context.
    #[must_use]
    pub fn id(&self, name: &str) -> AccountId {
        account_of(self.key(name))
    }

    /// The key of party `name`.
    ///
    /// # Panics
    /// No such party in this context.
    #[must_use]
    pub fn key(&self, name: &str) -> &PrivateKey {
        &self
            .parties
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no party {name}"))
            .1
    }

    /// The ids file this context reads its programs from.
    #[must_use]
    pub fn ids_path(&self) -> &Path {
        &self.ids_path
    }

    /// A fresh 32-byte id unique to this test run (schedule ids).
    pub fn fresh_id(&mut self) -> [u8; 32] {
        use sha2::Digest as _;
        self.counter += 1;
        let mut h = sha2::Sha256::new();
        h.update(self.name.as_bytes());
        h.update(self.dir.as_os_str().as_encoded_bytes());
        h.update(self.counter.to_le_bytes());
        h.finalize().into()
    }

    /// A fresh account with a key nobody else holds, not funded and not yet on chain.
    #[must_use]
    pub fn fresh_key() -> PrivateKey {
        PrivateKey::new_os_random()
    }

    /// The raw RPC client.
    #[must_use]
    pub const fn client(&self) -> &SequencerClient {
        &self.client
    }

    /// The test's wallet (holds the keys of every party).
    pub const fn wallet_mut(&mut self) -> &mut WalletCore<SequencerClient> {
        &mut self.wallet
    }

    // ------------------------------------------------------------ funding

    /// Transfers `amount` native units from the genesis test account to each key's account, in as few blocks as
    /// possible (explicit consecutive nonces), and waits until every transfer is included.
    ///
    /// # Errors
    /// A transfer was refused or not included in time.
    pub async fn fund(&self, keys: &[PrivateKey], amount: u128) -> Result<()> {
        let f = funder()?;
        for attempt in 0..2 {
            let mut hashes = Vec::new();
            {
                let mut next = f.next_nonce.lock().await;
                let mut nonce = match *next {
                    Some(n) => n,
                    None => self.nonce(f.id).await?,
                };
                for key in keys {
                    let tx = funding_tx(f, key, amount, nonce);
                    let hash = self
                        .client
                        .send_transaction(LeeTransaction::Public(tx))
                        .await
                        .map_err(|e| anyhow!("funding transfer: {e}"))?;
                    hashes.push(hash);
                    nonce += 1;
                }
                *next = Some(nonce);
            }
            let mut ok = true;
            for h in &hashes {
                if self.wait_included(*h).await.is_err() {
                    ok = false;
                }
            }
            if ok {
                return Ok(());
            }
            // A dropped transfer leaves a nonce gap: re-read the nonce and send again.
            *f.next_nonce.lock().await = None;
            if attempt == 1 {
                bail!("funding transfers were not included");
            }
        }
        Ok(())
    }

    async fn nonce(&self, id: AccountId) -> Result<u128> {
        let nonces = self
            .client
            .get_accounts_nonces(vec![id])
            .await
            .map_err(|e| anyhow!("nonces: {e}"))?;
        Ok(nonces.first().map_or(0, |n: &Nonce| n.0))
    }

    // ------------------------------------------------------------ sending

    /// Sends a public transaction over `accounts` (root: the first) through the test's wallet, which simulates it
    /// first, prices and signs it, and waits until it is included.
    ///
    /// # Errors
    /// [`SendError::Refused`] when the simulation reverts (nothing is sent), else [`SendError::Other`].
    pub async fn send(&self, accounts: Vec<AccountMention>, message: Vec<u8>) -> Result<Included, SendError> {
        let hash = self.wallet.send_pub_tx(accounts, 0, message).await.map_err(refusal)?;
        let max_fee = match self.wallet.take_declared_fee() {
            Some(wallet::DeclaredFee::Public(d)) => Some(d.max_fee),
            _ => None,
        };
        let mut included = self.wait_included(hash).await.map_err(SendError::Other)?;
        included.max_fee = max_fee;
        Ok(included)
    }

    /// As [`Self::send`], signed by the shared funder (LEZ's local test account): used where an account fixed
    /// outside the test must sign, such as the admin nominee of the fee-enabled pair (`e2e/deploy_fees.sh`).
    /// `accounts` gets the funder's account id. The funder's transfers are held back meanwhile, and the next one
    /// re-reads its nonce.
    ///
    /// # Errors
    /// As [`Self::send`]; the funder's earlier transfers were not included within `CV_E2E_TX_TIMEOUT`.
    pub async fn send_as_funder(
        &mut self,
        accounts: impl FnOnce(AccountId) -> Vec<AccountMention>,
        message: Vec<u8>,
    ) -> Result<Included, SendError> {
        let f = funder().map_err(SendError::Other)?;
        let mut next = f.next_nonce.lock().await;
        // The wallet reads the nonce from the chain: wait until every transfer numbered so far is included.
        if let Some(n) = *next {
            let deadline = Instant::now() + Duration::from_secs(env_u64("CV_E2E_TX_TIMEOUT", 180));
            while self.nonce(f.id).await.map_err(SendError::Other)? < n {
                if Instant::now() > deadline {
                    *next = None;
                    return Err(SendError::Other(anyhow!(
                        "funder transfers up to nonce {n} not included"
                    )));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        self.counter += 1;
        let mut wallet =
            open_wallet(&self.rpc, &self.dir.join(format!("funder-{}", self.counter))).map_err(SendError::Other)?;
        wallet
            .storage_mut()
            .key_chain_mut()
            .add_imported_public_account(f.key.clone());
        let sent = wallet.send_pub_tx(accounts(f.id), 0, message).await.map_err(refusal);
        // Whatever happened, the next transfer re-reads the nonce; the lock is held until this one is settled.
        *next = None;
        self.wait_included(sent?).await.map_err(SendError::Other)
    }

    /// As [`Self::send`], but a transaction whose simulation reverts is sent anyway and pays its fee: used to show
    /// that a failed operation which reaches the chain changes nothing (R1, R2).
    ///
    /// # Errors
    /// The transaction was not included.
    pub async fn send_even_if_reverting(
        &mut self,
        accounts: Vec<AccountMention>,
        message: Vec<u8>,
    ) -> Result<Included, SendError> {
        self.wallet.set_allow_revert(true);
        let r = self.wallet.send_pub_tx(accounts, 0, message).await;
        self.wallet.set_allow_revert(false);
        let hash = r.map_err(|e| SendError::Other(anyhow!("{e:#}")))?;
        self.wait_included(hash).await.map_err(SendError::Other)
    }

    /// Waits until `hash` is in a block (`CV_E2E_TX_TIMEOUT` seconds).
    ///
    /// # Errors
    /// Not included in time.
    pub async fn wait_included(&self, hash: HashType) -> Result<Included> {
        let deadline = Instant::now() + Duration::from_secs(env_u64("CV_E2E_TX_TIMEOUT", 180));
        loop {
            if let Ok(Some((_, block))) = self.client.get_transaction(hash).await {
                return Ok(Included {
                    hash,
                    block,
                    max_fee: None,
                });
            }
            if Instant::now() > deadline {
                bail!("transaction {hash} not included in time");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    // ------------------------------------------------------------ reading

    /// The actor state of `program` on `account` (empty when none).
    ///
    /// # Errors
    /// RPC failure.
    pub async fn actor_state(&self, account: AccountId, program: AccountId) -> Result<Vec<u8>> {
        let view = self
            .client
            .get_account_view(Actor::new(account, program))
            .await
            .map_err(|e| anyhow!("getAccountView: {e}"))?;
        Ok(view
            .map(|a| {
                let bytes: &[u8] = a.data.actor_state(program).borrow();
                bytes.to_vec()
            })
            .unwrap_or_default())
    }

    /// Native balance of `account` (0 when it does not exist).
    ///
    /// # Errors
    /// RPC failure.
    pub async fn native(&self, account: AccountId) -> Result<u128> {
        let bytes = self.actor_state(account, NATIVE_TOKEN_PROGRAM_ID).await?;
        if bytes.is_empty() {
            return Ok(0);
        }
        native_token::decode_balance(&bytes).map_err(|e| anyhow!("balance of {account}: {e:?}"))
    }

    /// The current clock time (ms): the timestamp the next transaction will read.
    ///
    /// # Errors
    /// RPC failure.
    pub async fn clock(&self) -> Result<u64> {
        let bytes = self
            .actor_state(clock_core::CLOCK_PROGRAM_ACCOUNT_ID, clock_core::clock_account_id())
            .await?;
        if bytes.is_empty() {
            bail!("clock account is empty");
        }
        Ok(clock_core::ClockAccountData::from_bytes(&bytes).timestamp)
    }

    /// Waits until the clock reads at least `t` (ms), i.e. until a transaction sent now sees a time `>= t`.
    ///
    /// # Errors
    /// The clock did not get there within `t - now` plus `CV_E2E_TX_TIMEOUT`.
    pub async fn wait_clock(&self, t: u64) -> Result<u64> {
        let now = self.clock().await?;
        let deadline = Instant::now()
            + Duration::from_millis(t.saturating_sub(now))
            + Duration::from_secs(env_u64("CV_E2E_TX_TIMEOUT", 180));
        loop {
            let now = self.clock().await?;
            if now >= t {
                return Ok(now);
            }
            if Instant::now() > deadline {
                bail!("clock at {now}, did not reach {t}");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Waits for `n` more blocks.
    ///
    /// # Errors
    /// RPC failure or no progress.
    pub async fn wait_blocks(&self, n: u64) -> Result<u64> {
        let start = self.client.get_last_block_id().await.map_err(|e| anyhow!("{e}"))?;
        let deadline = Instant::now() + Duration::from_secs(env_u64("CV_E2E_TX_TIMEOUT", 180) * n.max(1));
        loop {
            let b = self.client.get_last_block_id().await.map_err(|e| anyhow!("{e}"))?;
            if b >= start + n {
                return Ok(b);
            }
            if Instant::now() > deadline {
                bail!("no block after {start}");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// The transactions of block `block`, in order: `(hash, reverted)`.
    ///
    /// # Errors
    /// RPC failure or no such block.
    pub async fn block_transactions(&self, block: u64) -> Result<Vec<(HashType, bool)>> {
        let b = self
            .client
            .get_block(block)
            .await
            .map_err(|e| anyhow!("getBlock {block}: {e}"))?
            .with_context(|| format!("block {block} not found"))?;
        Ok(b.body
            .transactions
            .iter()
            .enumerate()
            .map(|(i, tx)| (tx.hash(), b.body.is_reverted(i)))
            .collect())
    }

    /// The Borsh bytes of an included transaction, exactly as any client reads it from the sequencer (`hash`: hex).
    ///
    /// # Errors
    /// Not a hash, RPC failure, or no such transaction.
    pub async fn transaction_bytes(&self, hash: &str) -> Result<Vec<u8>> {
        let hash: HashType = hash.parse().map_err(|e| anyhow!("transaction hash {hash}: {e:?}"))?;
        let (tx, _block) = self
            .client
            .get_transaction(hash)
            .await
            .map_err(|e| anyhow!("getTransaction: {e}"))?
            .with_context(|| format!("transaction {hash} not found"))?;
        Ok(borsh::to_vec(&tx)?)
    }

    /// A wallet home (config and stored keys of every party, each labelled with its party name) for LEZ's `wallet`
    /// and the `cliff-veil` CLI (`LEE_WALLET_HOME_DIR`, empty password).
    ///
    /// # Errors
    /// The wallet cannot be written.
    pub fn wallet_home(&mut self) -> Result<PathBuf> {
        self.counter += 1;
        let dir = self.dir.join(format!("home-{}", self.counter));
        let mut wallet = open_wallet(&self.rpc, &dir)?;
        for (name, key) in &self.parties {
            wallet
                .storage_mut()
                .key_chain_mut()
                .add_imported_public_account(key.clone());
            let id = wallet::account::AccountIdWithPrivacy::Public(account_of(key));
            wallet
                .storage_mut()
                .add_label(wallet::account::Label::new(name), id)
                .map_err(|e| anyhow!("label {name}: {e:#}"))?;
        }
        wallet
            .store_persistent_data()
            .map_err(|e| anyhow!("store wallet: {e:#}"))?;
        Ok(dir)
    }

    /// Mention of a party's own account selecting `program`'s actor state; the party signs.
    #[must_use]
    pub fn signer(&self, party: &str, program: AccountId) -> AccountMention {
        AccountIdentity::Public(self.id(party)).select_program_actor_state(program)
    }

    /// A native transfer from a party, through the wallet (used by the harness's own tests and by the cost tool).
    ///
    /// # Errors
    /// As [`Self::send`].
    pub async fn transfer_native(&self, from: &str, to: AccountId, amount: u128) -> Result<Included, SendError> {
        let accounts = vec![
            AccountIdentity::Public(self.id(from)).balance(),
            AccountIdentity::PublicNoSign(to).balance(),
        ];
        let msg =
            borsh::to_vec(&native_token::Message::Transfer { to, amount }).map_err(|e| SendError::Other(e.into()))?;
        self.send(accounts, msg).await
    }
}

/// A wallet failure as a [`SendError`]: a simulated revert is a refusal, with the program's code when it has one.
fn refusal(e: ExecutionFailureKind) -> SendError {
    match e {
        ExecutionFailureKind::SimulatedRevert(reason) => SendError::Refused {
            code: vesting_core::parse_code(&reason),
            reason,
        },
        other => SendError::Other(anyhow!("{other:#}")),
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A fresh wallet in `dir` (created) against the sequencer at `rpc`, with no accounts.
fn open_wallet(rpc: &str, dir: &Path) -> Result<WalletCore<SequencerClient>> {
    std::fs::create_dir_all(dir)?;
    let config = dir.join("wallet_config.json");
    std::fs::write(
        &config,
        format!(
            r#"{{"sequencer":{{"sequencer_addr":"{rpc}"}},"seq_poll_timeout":"2s","seq_tx_poll_max_blocks":20,
"seq_poll_max_retries":10,"seq_block_poll_max_amount":100,"gas_limit":{MAX_GAS_EXEC},
"fee_headroom_blocks":{FEE_HEADROOM_BLOCKS}}}"#
        ),
    )?;
    let (wallet, _mnemonic) = WalletCore::new_init_storage(config, dir.join("storage.json"), None, "")
        .map_err(|e| anyhow!("wallet: {e:#}"))?;
    Ok(wallet)
}

fn nonce_seed() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

fn funding_tx(f: &Funder, key: &PrivateKey, amount: u128, nonce: u128) -> PublicTransaction {
    let to = account_of(key);
    let root = Actor::native_balance(f.id);
    let message = borsh::to_vec(&native_token::Message::Transfer { to, amount }).expect("transfer encodes");
    // The cap is generous: the fee settled is the base fee times what is used, and the surplus is refunded.
    let fee = FeeDeclaration::new(f.id, FUND_GAS, 0, wallet::max_fee_for(FUND_GAS).saturating_mul(16));
    let msg = TxMessage::new(
        root,
        message,
        [root, Actor::native_balance(to)],
        [(f.id, Nonce(nonce))].into_iter().collect(),
        Some(fee),
        vec![PublicAccountEvidence::Key(PublicKey::new_from_private_key(key))],
    );
    let witness = WitnessSet::for_message(&msg, &[&f.key]);
    PublicTransaction::new(msg, witness)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_of_default() {
        assert_eq!(host_port(DEFAULT_RPC), Some("127.0.0.1:3050"));
    }

    #[test]
    fn host_port_needs_scheme_and_port() {
        assert_eq!(host_port("127.0.0.1:3050"), None);
        assert_eq!(host_port("http://localhost/"), None);
        assert_eq!(host_port("http://localhost:3051/rpc"), Some("localhost:3051"));
    }

    #[test]
    fn ids_file_parses_the_programs_and_accounts_tables_only() {
        let id = AccountId::new([7; 32]);
        let text = format!(
            "# header\n[deploy]\nnetwork = \"seq\"\n\n[programs]\nvesting = \"{id}\"   # sha256 ab, 1 bytes\n\n[accounts]\nfees_config = \"{id}\"\n\n[image_ids]\nvesting = \"ab\"\n"
        );
        let ids = Ids::parse(&text).expect("parses");
        assert_eq!(ids.get("vesting"), Some(id));
        assert_eq!(ids.get("fees_config"), Some(id));
        assert_eq!(ids.programs.len(), 2);
        assert!(ids.require("token").is_err());
    }

    #[test]
    fn funding_transfer_is_signed_by_the_funder_with_the_given_nonce() {
        let f = Funder {
            key: PrivateKey::try_new([9; 32]).expect("key"),
            id: account_of(&PrivateKey::try_new([9; 32]).expect("key")),
            next_nonce: tokio::sync::Mutex::new(None),
        };
        let to = PrivateKey::try_new([4; 32]).expect("key");
        let tx = funding_tx(&f, &to, 5, 41);
        let bytes = borsh::to_vec(&LeeTransaction::Public(tx)).expect("encodes");
        assert!(!bytes.is_empty());
    }
}
