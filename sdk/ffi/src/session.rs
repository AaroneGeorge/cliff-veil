//! The safe half of the C ABI: one [`Session`] per client handle, and every operation as JSON in, JSON out.
//!
//! [`crate::abi`] only converts C strings, catches panics and frees memory; everything the SDK does happens here.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cliff_veil_sdk::json as shape;
use cliff_veil_sdk::{
    Backend as _, ClaimTarget, Client, Error, ListFilter, MemoryBackend, Outcome, Prepared, Programs, ScanCache,
    SequencerBackend, Standing, core_id, linear_terms, milestone_terms, privacy_disclosure, schedule_id_from_label,
};
use lee_core::account::AccountId;
use serde_json::{Map, Value, json};
use sha2::Digest as _;
use vesting_core::{Asset, BatchItem, BatchShape, BatchTerms, Terms};

/// Prepared transactions a handle keeps; preparing more drops the oldest.
pub const MAX_HELD: usize = 64;

/// The vesting program id of an in-memory chain when the configuration names none.
const MEMORY_VESTING: AccountId = AccountId::new([9; 32]);
/// The fee program id of an in-memory chain with a fee rate.
const MEMORY_FEES: AccountId = AccountId::new([8; 32]);
/// The token program id of an in-memory chain.
const MEMORY_TOKEN: AccountId = AccountId::new([7; 32]);

/// The error object of a failed call (`{"code", "name", "message", "action", ...}`).
#[derive(Debug, Clone, PartialEq)]
pub struct AbiError(pub Value);

impl AbiError {
    /// An error of the C ABI itself (not of the SDK): `code` is null.
    pub fn new(name: &str, message: impl Into<String>, action: Option<&str>) -> Self {
        Self(json!({"code": null, "name": name, "message": message.into(), "action": action}))
    }

    /// A bad argument or request.
    pub fn input(message: impl Into<String>) -> Self {
        Self::new(
            "Input",
            message,
            Some("fix the request; docs/SDK.md lists the fields of every call"),
        )
    }
}

impl From<Error> for AbiError {
    fn from(e: Error) -> Self {
        Self(shape::error(&e))
    }
}

impl From<anyhow::Error> for AbiError {
    fn from(e: anyhow::Error) -> Self {
        match e.downcast::<Error>() {
            Ok(err) => err.into(),
            Err(e) => Self::input(format!("{e:#}")),
        }
    }
}

pub type Answer = Result<Value, AbiError>;

/// The chain a handle talks to.
#[allow(
    clippy::large_enum_variant,
    reason = "one per client handle, behind its lock; never stored in bulk"
)]
enum Chain {
    /// An in-memory chain running the vesting rules (tests, demos, offline module development). Accounts are named
    /// by the labels of the configuration.
    Memory {
        client: Client<MemoryBackend>,
        labels: BTreeMap<String, AccountId>,
    },
    /// LEZ's wallet against a sequencer.
    Sequencer { client: Client<SequencerBackend> },
}

/// Runs `$body` with `$c` bound to the handle's `Client<_>`, whichever backend it has.
macro_rules! on_client {
    ($chain:expr, $c:ident => $body:expr) => {
        match $chain {
            Chain::Memory { client: $c, .. } => $body,
            Chain::Sequencer { client: $c } => $body,
        }
    };
}

/// A prepared transaction waiting for [`Session::send`].
struct Held {
    prepared: Prepared,
    /// For a claim to a private account: the token that acknowledges its disclosure.
    disclosure_ack: Option<String>,
}

/// One client handle.
pub struct Session {
    rt: tokio::runtime::Runtime,
    chain: Chain,
    held: BTreeMap<u64, Held>,
    next_id: u64,
    scan: Option<ScanCache>,
}

// The handle is shared across threads behind a lock (lib.rs, "Threads"): its state must be `Send`.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<Session>();
};

// ---------------------------------------------------------------- request fields

fn field<'a>(req: &'a Value, key: &str) -> Option<&'a Value> {
    req.get(key).filter(|v| !v.is_null())
}

fn text<'a>(req: &'a Value, key: &str) -> Result<&'a str, AbiError> {
    opt_text(req, key)?.ok_or_else(|| AbiError::input(format!("`{key}` is required")))
}

fn opt_text<'a>(req: &'a Value, key: &str) -> Result<Option<&'a str>, AbiError> {
    match field(req, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(AbiError::input(format!("`{key}` must be a string"))),
    }
}

fn flag(req: &Value, key: &str, default: bool) -> Result<bool, AbiError> {
    match field(req, key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(AbiError::input(format!("`{key}` must be true or false"))),
    }
}

/// An amount: a JSON number or a decimal string (amounts are 128-bit).
fn amount_of(v: &Value, what: &str) -> Result<u128, AbiError> {
    match v {
        Value::Number(n) => n.as_u64().map(u128::from),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .ok_or_else(|| AbiError::input(format!("`{what}` must be a whole amount (number or decimal string)")))
}

fn amount(req: &Value, key: &str) -> Result<u128, AbiError> {
    amount_of(
        field(req, key).ok_or_else(|| AbiError::input(format!("`{key}` is required")))?,
        key,
    )
}

fn opt_u64(req: &Value, key: &str) -> Result<Option<u64>, AbiError> {
    field(req, key)
        .map(|v| {
            amount_of(v, key)
                .and_then(|a| u64::try_from(a).map_err(|_| AbiError::input(format!("`{key}` is too large"))))
        })
        .transpose()
}

/// A time: milliseconds since the Unix epoch (number or decimal string), or `"now"` (the chain clock).
fn time(req: &Value, key: &str, now: u64) -> Result<Option<u64>, AbiError> {
    match field(req, key) {
        None => Ok(None),
        Some(Value::String(s)) if s.trim() == "now" => Ok(Some(now)),
        Some(_) => opt_u64(req, key),
    }
}

fn list<'a>(req: &'a Value, key: &str) -> Result<Option<&'a Vec<Value>>, AbiError> {
    match field(req, key) {
        None => Ok(None),
        Some(Value::Array(a)) => Ok(Some(a)),
        Some(_) => Err(AbiError::input(format!("`{key}` must be an array"))),
    }
}

fn schedule_id_hex(text: &str) -> Result<[u8; 32], AbiError> {
    hex::decode(text.trim())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| AbiError::input("`id` must be 64 hex characters (32 bytes)"))
}

/// Parses `json` as an object.
pub fn parse_request(json: &str) -> Result<Value, AbiError> {
    let v: Value =
        serde_json::from_str(json).map_err(|e| AbiError::input(format!("the request is not valid JSON: {e}")))?;
    if v.is_object() {
        Ok(v)
    } else {
        Err(AbiError::input("the request must be a JSON object"))
    }
}

/// The id of an in-memory account named `label` when the configuration gives no id: `SHA-256` of the label.
pub fn memory_account(label: &str) -> AccountId {
    AccountId::new(sha2::Sha256::digest(format!("cliff-veil memory account\n{label}").as_bytes()).into())
}

/// The acknowledgement token of a private claim's disclosure: binds the acknowledgement to this prepared claim
/// (its id and one-time address) and to the exact disclosure text.
fn disclosure_token(id: u64, prepared: &Prepared) -> String {
    let address = match &prepared.outcome {
        Outcome::Claim(c) => match c.destination {
            cliff_veil_sdk::Destination::Private { blinded, .. } => blinded.to_string(),
            cliff_veil_sdk::Destination::Public(a) => a.to_string(),
        },
        _ => String::new(),
    };
    let digest = sha2::Sha256::digest(
        format!(
            "cliff-veil privacy disclosure\n{id}\n{address}\n{}",
            privacy_disclosure()
        )
        .as_bytes(),
    );
    hex::encode(digest)
}

fn programs_json(p: &Programs) -> Value {
    let s = |a: Option<AccountId>| a.map(|a| a.to_string());
    json!({"vesting": p.vesting.to_string(), "vesting_fees": s(p.fees), "token": s(p.token), "ata": s(p.ata)})
}

// ---------------------------------------------------------------- the session

impl Session {
    /// Opens a client from its configuration (docs/SDK.md, "Opening a client").
    ///
    /// # Errors
    /// A bad configuration, a wallet that does not open, or no runtime.
    pub fn open(config_json: &str) -> Result<(Self, Value), AbiError> {
        let config = parse_request(config_json)?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("cliff-veil-sdk")
            .enable_all()
            .build()
            .map_err(|e| AbiError::new("Backend", format!("cannot start the SDK runtime: {e}"), None))?;
        let (chain, info) = match text(&config, "backend")? {
            "memory" => Self::open_memory(&config)?,
            "sequencer" => {
                let _inside = rt.enter();
                Self::open_sequencer(&config)?
            }
            other => {
                return Err(AbiError::input(format!(
                    "unknown backend `{other}`: use \"sequencer\" or \"memory\""
                )));
            }
        };
        let session = Self {
            rt,
            chain,
            held: BTreeMap::new(),
            next_id: 1,
            scan: None,
        };
        let mut info = info;
        info["programs"] = programs_json(session.programs());
        info["dev_mode"] = Value::Bool(cliff_veil_sdk::dev_mode());
        Ok((session, info))
    }

    fn open_memory(config: &Value) -> Result<(Chain, Value), AbiError> {
        let vesting = opt_text(config, "vesting")?
            .map(|v| v.parse::<AccountId>())
            .transpose()
            .map_err(|e| AbiError::input(format!("`vesting` is not an account id: {e:?}")))?
            .unwrap_or(MEMORY_VESTING);
        let rate = opt_u64(config, "fee_rate_bps")?;
        let mut programs = Programs::new(vesting);
        programs.token = Some(MEMORY_TOKEN);
        if rate.is_some() {
            programs.fees = Some(MEMORY_FEES);
        }
        let mut chain = MemoryBackend::new(programs, opt_u64(config, "now_ms")?.unwrap_or(0));
        let mut labels = BTreeMap::new();
        for (key, private) in [("public", false), ("private", true)] {
            for entry in list(config, key)?.into_iter().flatten() {
                let label = text(entry, "label")?;
                let account = match opt_text(entry, "account")? {
                    Some(a) => a
                        .parse()
                        .map_err(|e| AbiError::input(format!("`{a}` is not an account id: {e:?}")))?,
                    None => memory_account(label),
                };
                let balance = field(entry, "balance").map_or(Ok(0), |b| amount_of(b, "balance"))?;
                if private {
                    chain.add_private(account, balance);
                } else {
                    chain.add_public(account, balance);
                }
                labels.insert(label.to_owned(), account);
            }
        }
        if let Some(rate) = rate {
            chain.fees.rate_bps = u16::try_from(rate).map_err(|_| AbiError::input("`fee_rate_bps` is too large"))?;
            chain.fees.deployed = true;
            if let Some(t) = opt_text(config, "treasury")? {
                chain.fees.treasury = labels.get(t).copied().unwrap_or_else(|| memory_account(t));
            }
        }
        let accounts: Map<String, Value> = labels
            .iter()
            .map(|(l, a)| (l.clone(), Value::String(a.to_string())))
            .collect();
        let info = json!({"backend": "memory", "accounts": accounts});
        Ok((
            Chain::Memory {
                client: Client::new(chain),
                labels,
            },
            info,
        ))
    }

    fn open_sequencer(config: &Value) -> Result<(Chain, Value), AbiError> {
        let parse = |key: &str| -> Result<Option<AccountId>, AbiError> {
            opt_text(config, key)?
                .map(|v| {
                    v.parse()
                        .map_err(|e| AbiError::input(format!("`{key}` is not an account id: {e:?}")))
                })
                .transpose()
        };
        let programs = match (opt_text(config, "ids_file")?, parse("vesting")?) {
            (_, Some(vesting)) => Programs {
                vesting,
                fees: parse("vesting_fees")?,
                token: parse("token_program")?,
                ata: parse("ata_program")?,
            },
            (Some(path), None) => Programs::from_ids_file(&PathBuf::from(path))?,
            (None, None) => {
                return Err(AbiError::input(
                    "give `ids_file` (the program ids file deploy_local.sh writes) or `vesting` (the program id)",
                ));
            }
        };
        // The wallet home and password as LEZ's `wallet` finds them, but never a terminal prompt: a library runs
        // inside a host process with no terminal.
        let home = match opt_text(config, "wallet_home")? {
            Some(home) => PathBuf::from(home),
            None => std::env::var_os("LEE_WALLET_HOME_DIR")
                .map(PathBuf::from)
                .or_else(|| std::env::home_dir().map(|h| h.join(".lee").join("wallet")))
                .ok_or_else(|| AbiError::input("give `wallet_home` (or set LEE_WALLET_HOME_DIR)"))?,
        };
        let password = match opt_text(config, "wallet_password")? {
            Some(p) => p.to_owned(),
            None => std::env::var("LEE_WALLET_PASSWORD").unwrap_or_default(),
        };
        let wallet = cliff_veil_sdk::sequencer::Wallet::new_update_chain(
            home.join("wallet_config.json"),
            home.join("storage.json"),
            None,
            &password,
        )
        .map_err(|e| {
            AbiError::new(
                "Backend",
                format!("cannot open the wallet at {}: {e:#}", home.display()),
                Some("check `wallet_home` and `wallet_password` (or LEE_WALLET_HOME_DIR and LEE_WALLET_PASSWORD)"),
            )
        })?;
        let mut backend = SequencerBackend::new(wallet, programs);
        if let Some(blocks) = opt_u64(config, "fee_headroom_blocks")? {
            backend.set_fee_headroom_blocks(blocks);
        }
        Ok((
            Chain::Sequencer {
                client: Client::new(backend),
            },
            json!({"backend": "sequencer"}),
        ))
    }

    fn programs(&self) -> &Programs {
        on_client!(&self.chain, c => c.programs())
    }

    /// An account by label (memory: the configuration's labels; sequencer: wallet labels, `Public/<id>`,
    /// `Private/<id>`) or by base58 id.
    fn resolve(&self, name: &str) -> Result<AccountId, AbiError> {
        match &self.chain {
            Chain::Memory { labels, .. } => labels.get(name).copied().map_or_else(
                || {
                    name.parse().map_err(|e| {
                        AbiError::input(format!(
                            "`{name}` is neither a configured label nor an account id: {e:?}"
                        ))
                    })
                },
                Ok,
            ),
            Chain::Sequencer { client } => Ok(client.backend().resolve(name)?),
        }
    }

    fn opt_account(&self, req: &Value, key: &str) -> Result<Option<AccountId>, AbiError> {
        opt_text(req, key)?.map(|s| self.resolve(s)).transpose()
    }

    fn account(&self, req: &Value, key: &str) -> Result<AccountId, AbiError> {
        self.resolve(text(req, key)?)
    }

    fn now(&self) -> Result<u64, AbiError> {
        let rt = &self.rt;
        on_client!(&self.chain, c => rt.block_on(c.backend().now())).map_err(Into::into)
    }

    /// Keeps `prepared` for [`Self::send`] and returns its transaction object with `"prepared": <id>` and, for a
    /// claim to a private account, `"disclosure_ack"`.
    fn hold(&mut self, op: &str, prepared: Prepared, schedule_id: Option<[u8; 32]>) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut value = shape::prepared(op, &prepared, schedule_id);
        let disclosure_ack = prepared.tx.is_private().then(|| disclosure_token(id, &prepared));
        value["prepared"] = Value::from(id);
        if let Some(token) = &disclosure_ack {
            value["disclosure_ack"] = Value::String(token.clone());
        }
        while self.held.len() >= MAX_HELD {
            self.held.pop_first();
        }
        self.held.insert(
            id,
            Held {
                prepared,
                disclosure_ack,
            },
        );
        value
    }

    fn transactions(&mut self, op: &str, prepared: Vec<Prepared>, schedule_id: Option<[u8; 32]>) -> Value {
        let list: Vec<Value> = prepared.into_iter().map(|p| self.hold(op, p, schedule_id)).collect();
        json!({ "transactions": list })
    }

    // ------------------------------------------------------------ reading

    /// `{"schedule": {...}}`.
    pub fn show(&self, schedule: &str) -> Answer {
        let account = self.resolve(schedule)?;
        let rt = &self.rt;
        let view = on_client!(&self.chain, c => rt.block_on(c.status(account)))?;
        Ok(json!({ "schedule": shape::schedule(&view) }))
    }

    /// `{"claimable": {...}}`.
    pub fn claimable(&self, schedule: &str) -> Answer {
        let account = self.resolve(schedule)?;
        let rt = &self.rt;
        let view = on_client!(&self.chain, c => rt.block_on(c.status(account)))?;
        Ok(json!({ "claimable": shape::claimable(&view) }))
    }

    /// `{"schedules": [...], "scanned_to_block": n}` for `{"creator" | "beneficiary", "from_block"?, "active"?}`.
    pub fn list(&mut self, req: &Value) -> Answer {
        let filter = match (self.opt_account(req, "creator")?, self.opt_account(req, "beneficiary")?) {
            (Some(c), None) => ListFilter::Creator(c),
            (None, Some(b)) => ListFilter::Beneficiary(b),
            _ => return Err(AbiError::input("give exactly one of `creator` and `beneficiary`")),
        };
        let from_block = opt_u64(req, "from_block")?;
        let active = flag(req, "active", false)?;
        let vesting = self.programs().vesting;
        let mut cache = self.scan.take().unwrap_or_else(|| ScanCache::new(vesting));
        let rt = &self.rt;
        let found = on_client!(&self.chain, c => rt.block_on(c.list(filter, &mut cache, from_block)));
        let scanned_to = cache.next_block.saturating_sub(1);
        self.scan = Some(cache);
        let mut views = found?;
        if active {
            views.retain(|v| v.standing == Standing::Active);
        }
        Ok(json!({
            "schedules": views.iter().map(shape::schedule).collect::<Vec<_>>(),
            "scanned_to_block": scanned_to,
        }))
    }

    /// `{"fees": {...}}`.
    pub fn fee_show(&self) -> Answer {
        let rt = &self.rt;
        let config = on_client!(&self.chain, c => rt.block_on(c.fee_config()))?;
        Ok(json!({ "fees": shape::fees(&config, self.programs()) }))
    }

    /// `{"schedule": "<account>"}` for `{"creator", "label" | "id"}`.
    pub fn address(&self, req: &Value) -> Answer {
        let creator = self.account(req, "creator")?;
        let id = match (opt_text(req, "label")?, opt_text(req, "id")?) {
            (Some(l), None) => schedule_id_from_label(l),
            (None, Some(h)) => schedule_id_hex(h)?,
            _ => return Err(AbiError::input("give exactly one of `label` and `id`")),
        };
        Ok(json!({ "schedule": self.programs().schedule_account(creator, &id).to_string() }))
    }

    // ------------------------------------------------------------ preparing

    fn asset(&self, req: &Value) -> Result<Asset, AbiError> {
        Ok(match self.opt_account(req, "token")? {
            None => Asset::Native,
            Some(d) => Asset::Token { definition: core_id(d) },
        })
    }

    /// Applies `cancelable`, `transferable`, `cancel_authority`, `milestone_authority` and `refund_to`.
    fn common_terms(&self, req: &Value, terms: &mut Terms) -> Result<(), AbiError> {
        terms.cancelable = flag(req, "cancelable", true)?;
        terms.transferable = flag(req, "transferable", false)?;
        terms.cancel_authority = self.opt_account(req, "cancel_authority")?.map(core_id);
        terms.milestone_authority = self.opt_account(req, "milestone_authority")?.map(core_id);
        terms.refund_to = self.opt_account(req, "refund_to")?.map(core_id);
        Ok(())
    }

    /// `start` (default now), `cliff` (default start) and `end` (required).
    fn linear_times(req: &Value, now: u64) -> Result<(u64, u64, u64), AbiError> {
        let start = time(req, "start", now)?.unwrap_or(now);
        let cliff = time(req, "cliff", now)?.unwrap_or(start);
        let end = time(req, "end", now)?
            .ok_or_else(|| AbiError::input("`end` is required for a linear schedule (or give `milestones`)"))?;
        Ok((start, cliff, end))
    }

    /// Create one schedule: `{"from", "beneficiary", "total", "start"?, "cliff"?, "end"}` or `{"from",
    /// "beneficiary", "milestones": [...]}`, plus `token`, the common terms, and `label` or `id`.
    pub fn prepare_create(&mut self, req: &Value) -> Answer {
        let creator = self.account(req, "from")?;
        let beneficiary = self.account(req, "beneficiary")?;
        let asset = self.asset(req)?;
        let now = self.now()?;
        let mut terms = match list(req, "milestones")? {
            Some(m) => {
                let amounts = m
                    .iter()
                    .map(|a| amount_of(a, "milestones"))
                    .collect::<Result<Vec<_>, _>>()?;
                let terms = milestone_terms(beneficiary, asset, amounts);
                if field(req, "total").is_some() && amount(req, "total")? != terms.total {
                    return Err(AbiError::input(format!(
                        "`total` differs from the sum of `milestones` ({})",
                        terms.total
                    )));
                }
                terms
            }
            None => {
                let (start, cliff, end) = Self::linear_times(req, now)?;
                linear_terms(beneficiary, asset, amount(req, "total")?, start, cliff, end)
            }
        };
        self.common_terms(req, &mut terms)?;
        let id = match (opt_text(req, "label")?, opt_text(req, "id")?) {
            (Some(_), Some(_)) => return Err(AbiError::input("give `label` or `id`, not both")),
            (Some(l), None) => schedule_id_from_label(l),
            (None, Some(h)) => schedule_id_hex(h)?,
            (None, None) => on_client!(&mut self.chain, c => c.fresh_schedule_id()),
        };
        let rt = &self.rt;
        let prepared = on_client!(&mut self.chain, c => rt.block_on(c.prepare_create(creator, id, terms)))?;
        Ok(self.transactions("create", vec![prepared], Some(id)))
    }

    /// Create one schedule per item, split into transactions that fit: `{"from", "items": [{"beneficiary",
    /// "total", "label"?}], "start"?, "cliff"?, "end"}` or `{..., "milestone_bps": [...]}`, plus `token`, the common
    /// terms and `per_tx`.
    pub fn prepare_create_batch(&mut self, req: &Value) -> Answer {
        let creator = self.account(req, "from")?;
        let now = self.now()?;
        let shape = match list(req, "milestone_bps")? {
            Some(bps) => BatchShape::Milestones {
                bps: bps
                    .iter()
                    .map(|b| {
                        amount_of(b, "milestone_bps")
                            .and_then(|b| u16::try_from(b).map_err(|_| AbiError::input("a weight is too large")))
                    })
                    .collect::<Result<_, _>>()?,
            },
            None => {
                let (start, cliff, end) = Self::linear_times(req, now)?;
                BatchShape::Linear { start, cliff, end }
            }
        };
        let mut probe = linear_terms(creator, self.asset(req)?, 1, 0, 0, 1);
        self.common_terms(req, &mut probe)?;
        let common = BatchTerms {
            asset: probe.asset,
            shape,
            cancelable: probe.cancelable,
            transferable: probe.transferable,
            cancel_authority: probe.cancel_authority,
            milestone_authority: probe.milestone_authority,
            refund_to: probe.refund_to,
        };
        let rows = list(req, "items")?.ok_or_else(|| AbiError::input("`items` is required"))?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let schedule_id = match opt_text(row, "label")? {
                Some(l) => schedule_id_from_label(l),
                None => on_client!(&mut self.chain, c => c.fresh_schedule_id()),
            };
            items.push(BatchItem {
                schedule_id,
                beneficiary: core_id(self.account(row, "beneficiary")?),
                total: amount(row, "total")?,
            });
        }
        let per_tx = opt_u64(req, "per_tx")?
            .map(|n| usize::try_from(n).map_err(|_| AbiError::input("`per_tx` is too large")))
            .transpose()?;
        let rt = &self.rt;
        let prepared =
            on_client!(&mut self.chain, c => rt.block_on(c.prepare_create_batch(creator, common, items, per_tx)))?;
        Ok(self.transactions("create-batch", prepared, None))
    }

    /// Claim everything claimable now: `{"schedule", "as", "to"?}` (public) or `{"schedule", "as", "private":
    /// true, "private_to"?, "fee_payer"?}`.
    pub fn prepare_claim(&mut self, req: &Value) -> Answer {
        let beneficiary = self.account(req, "as")?;
        let schedule = self.account(req, "schedule")?;
        let target = if flag(req, "private", false)? {
            if field(req, "to").is_some() {
                return Err(AbiError::input(
                    "`to` names a public destination; a private claim takes `private_to`",
                ));
            }
            ClaimTarget::Private {
                account: self.opt_account(req, "private_to")?,
                fee_payer: self.opt_account(req, "fee_payer")?,
            }
        } else {
            if field(req, "private_to").is_some() || field(req, "fee_payer").is_some() {
                return Err(AbiError::input("`private_to` and `fee_payer` need `\"private\": true`"));
            }
            ClaimTarget::Public(self.opt_account(req, "to")?.unwrap_or(beneficiary))
        };
        let rt = &self.rt;
        let prepared = on_client!(&mut self.chain, c => rt.block_on(c.prepare_claim(beneficiary, schedule, target)))?;
        Ok(self.transactions("claim", vec![prepared], None))
    }

    /// `{"schedule", "as"}` (the cancel authority).
    pub fn prepare_cancel(&mut self, req: &Value) -> Answer {
        let (signer, schedule) = (self.account(req, "as")?, self.account(req, "schedule")?);
        let rt = &self.rt;
        let prepared = on_client!(&mut self.chain, c => rt.block_on(c.prepare_cancel(signer, schedule)))?;
        Ok(self.transactions("cancel", vec![prepared], None))
    }

    /// `{"schedule", "as"}` (the creator or the cancel authority).
    pub fn prepare_make_non_cancelable(&mut self, req: &Value) -> Answer {
        let (signer, schedule) = (self.account(req, "as")?, self.account(req, "schedule")?);
        let rt = &self.rt;
        let prepared = on_client!(&mut self.chain, c => rt.block_on(c.prepare_make_non_cancelable(signer, schedule)))?;
        Ok(self.transactions("make-non-cancelable", vec![prepared], None))
    }

    /// `{"schedule", "as", "index"}` (the milestone authority).
    pub fn prepare_signal(&mut self, req: &Value) -> Answer {
        let (signer, schedule) = (self.account(req, "as")?, self.account(req, "schedule")?);
        let index = opt_u64(req, "index")?
            .ok_or_else(|| AbiError::input("`index` is required"))
            .and_then(|i| u8::try_from(i).map_err(|_| AbiError::input("`index` is out of range")))?;
        let rt = &self.rt;
        let prepared = on_client!(&mut self.chain, c => rt.block_on(c.prepare_signal(signer, schedule, index)))?;
        Ok(self.transactions("signal", vec![prepared], None))
    }

    /// `{"schedule", "as", "to"}` (the current beneficiary hands the schedule to `to`).
    pub fn prepare_transfer(&mut self, req: &Value) -> Answer {
        let (signer, schedule) = (self.account(req, "as")?, self.account(req, "schedule")?);
        let to = self.account(req, "to")?;
        let rt = &self.rt;
        let prepared = on_client!(&mut self.chain, c => rt.block_on(c.prepare_transfer(signer, schedule, to)))?;
        Ok(self.transactions("transfer", vec![prepared], None))
    }

    // ------------------------------------------------------------ sending

    /// Sends prepared transaction `id` and waits for inclusion. A claim to a private account needs `disclosure_ack`
    /// equal to the token its preparation returned; without it nothing is sent and the transaction stays prepared.
    /// Any other outcome uses the transaction up: prepare again to retry.
    pub fn send(&mut self, id: u64, disclosure_ack: Option<&str>) -> Answer {
        let held = self.held.remove(&id).ok_or_else(|| {
            AbiError::new(
                "UnknownPrepared",
                format!("no prepared transaction {id} on this client (it was sent, discarded or dropped as old)"),
                Some("prepare the operation again"),
            )
        })?;
        if let Some(expected) = &held.disclosure_ack
            && disclosure_ack.map(str::trim) != Some(expected.as_str())
        {
            let given = disclosure_ack.is_some_and(|a| !a.trim().is_empty());
            self.held.insert(id, held);
            return Err(AbiError::new(
                "DisclosureNotAcknowledged",
                if given {
                    "the acknowledgement does not match this claim's privacy disclosure; nothing was sent"
                } else {
                    "a claim to a private account is sent only after its privacy disclosure is acknowledged; nothing \
                     was sent"
                },
                Some(
                    "show `privacy_disclosure` of this claim to the person; once they accept it, send again with this \
                     claim's `disclosure_ack`",
                ),
            ));
        }
        let rt = &self.rt;
        let receipt = on_client!(&mut self.chain, c => rt.block_on(c.send(&held.prepared)))?;
        let mut out = json!({ "prepared": id, "sent": [shape::receipt(&receipt)] });
        if let Outcome::Claim(c) = &held.prepared.outcome
            && let cliff_veil_sdk::Destination::Private { account, .. } = c.destination
        {
            let balance = on_client!(&self.chain, c => c.backend().private_native_balance(account));
            out["private_balance_after"] = shape::private_balance(account, balance);
        }
        Ok(out)
    }

    /// Drops prepared transaction `id` without sending it.
    pub fn discard(&mut self, id: u64) -> Answer {
        Ok(json!({ "prepared": id, "discarded": self.held.remove(&id).is_some() }))
    }

    /// In-memory chains only: sets the clock (ms) the next reads and transactions see.
    pub fn memory_set_clock(&mut self, now_ms: u64) -> Answer {
        match &mut self.chain {
            Chain::Memory { client, .. } => {
                client.backend_mut().now = now_ms;
                Ok(json!({ "clock_ms": now_ms, "clock": cliff_veil_sdk::time::utc(now_ms) }))
            }
            Chain::Sequencer { .. } => Err(AbiError::new(
                "WrongBackend",
                "the clock can be set on an in-memory chain only; a sequencer's clock is its blocks' timestamps",
                None,
            )),
        }
    }
}
