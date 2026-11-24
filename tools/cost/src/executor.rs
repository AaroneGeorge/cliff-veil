//! Executor mode: every operation through LEZ's public execution on a local state, with its cycle count and the
//! size of the transaction as a sequencer receives it.

use std::borrow::Cow;
use std::path::Path;

use anyhow::{Context as _, Result};
use borsh::BorshSerialize;
use clock_core::{CLOCK_PROGRAM_ACCOUNT_ID, ClockAccountData, clock_account_id, clock_actor};
use common::transaction::LeeTransaction;
use lee::{
    PrivateKey, PublicAccountEvidence, PublicKey, PublicTransaction, V03State, ValidatedStateDiff,
    program::Program,
    public_transaction::{Message as TxMessage, WitnessSet},
};
use lee_core::{
    account::{Account, AccountId, Actor},
    native_token::{self, NATIVE_TOKEN_PROGRAM_ID},
    program::PdaSeed,
};
use lez_admin_authority::{AdminOp, admin_op};
use sha2::Digest as _;
use token_core::{TokenHolding, TokenState};
use vesting_core::{Asset, BatchItem, BatchShape, BatchTerms, Kind, Message, Terms};
use vesting_fees::{CONFIG_SEED, FeeConfig, FeeMessage, config_actor, wire};

use crate::{Row, Run};

/// The chain's execution cap per transaction (rc3 fee market `MAX_GAS_EXEC`).
const CAP: u64 = 10_000_000;
const T0: u64 = 1_700_000_000_000;
const SPAN: u64 = 1_000_000;
const TOTAL: u128 = 1_000_000;
/// Batch sizes measured besides the program's cap.
const BATCHES: [u64; 3] = [1, 8, 32];
/// Fee rate of the fee-path rows, in basis points (0.5%).
const FEE_BPS: u16 = 50;

/// Cycles of a transaction and its size in bytes as sent (`LeeTransaction::Public`, Borsh).
type Measured = Result<(u64, u64), String>;

struct Chain {
    state: V03State,
    vesting: AccountId,
    /// `vesting_fees` program and its config account.
    fees: Option<(AccountId, AccountId)>,
    /// The token program and the ATA program `vesting` was built with.
    token: Option<AccountId>,
    ata: Option<AccountId>,
    block: u64,
    now: u64,
}

fn key(b: u8) -> PrivateKey {
    PrivateKey::try_new([b; 32]).expect("valid test key")
}

fn id_of(k: &PrivateKey) -> AccountId {
    AccountId::from(&PublicKey::new_from_private_key(k))
}

fn core(id: AccountId) -> vesting_core::AccountId {
    vesting_core::AccountId::new(*id.value())
}

impl Chain {
    fn fund(&mut self, id: AccountId, amount: u128) {
        let mut account = self.state.get_account_by_id(id);
        account
            .data
            .set_actor_state(NATIVE_TOKEN_PROGRAM_ID, native_token::encode_balance(amount));
        self.state.force_insert_account(id, account);
    }

    /// Runs one transaction at the current block and clock time under the chain cap; returns its cycles and size.
    fn submit(
        &mut self,
        root: Actor,
        declared: &[Actor],
        signers: &[&PrivateKey],
        evidence: Vec<PublicAccountEvidence>,
        message: &impl BorshSerialize,
    ) -> Measured {
        let nonces = signers
            .iter()
            .map(|k| {
                let id = id_of(k);
                (id, self.state.get_account_by_id(id).nonce)
            })
            .collect();
        let mut msg =
            TxMessage::try_new(root, declared.iter().copied(), nonces, message).map_err(|e| format!("{e:?}"))?;
        msg.admission_evidence = evidence;
        let tx = PublicTransaction::new(msg.clone(), WitnessSet::for_message(&msg, signers));
        let bytes = borsh::to_vec(&LeeTransaction::Public(tx.clone())).map_err(|e| format!("{e}"))?;
        let (diff, charge) =
            ValidatedStateDiff::from_public_transaction_with_cycle_budget(&tx, &self.state, self.block, self.now, CAP)
                .map_err(|e| format!("{e}"))?;
        if let Some(revert) = charge.revert {
            return Err(revert);
        }
        self.state.apply_state_diff(diff).map_err(|e| format!("{e}"))?;
        Ok((charge.cycles, bytes.len() as u64))
    }

    fn tick(&mut self, now: u64) -> Measured {
        self.block += 1;
        self.now = now;
        let msg = clock_core::Message::Tick {
            timestamp: now,
            block_id: self.block,
        };
        self.submit(clock_actor(), &[clock_actor()], &[], Vec::new(), &msg)
    }

    fn schedule(&self, creator: AccountId, id: &[u8; 32]) -> AccountId {
        AccountId::for_public_pda(
            &self.vesting,
            &PdaSeed::new(vesting_core::schedule_seed(&core(creator), id)),
        )
    }

    fn seat(&self, who: AccountId) -> Actor {
        Actor::new(who, self.vesting)
    }

    /// Writes the `vesting_fees` config: rate 0 is the state `Init` leaves on a deployment, a non-zero rate the state
    /// a later `SetFee` leaves.
    fn set_fee_config(&mut self, rate_bps: u16, treasury: AccountId) {
        let Some((program, config)) = self.fees else { return };
        let mut account = self.state.get_account_by_id(config);
        let fee_config = FeeConfig {
            rate_bps,
            treasury,
            ..FeeConfig::default()
        };
        let bytes = borsh::to_vec(&fee_config).expect("config encodes");
        account.data.set_actor_state(program, bytes.into());
        self.state.force_insert_account(config, account);
    }

    /// The balance of `account` in `asset` (0 for an account without one).
    fn balance(&self, asset: Asset, account: AccountId) -> u128 {
        let data = &self.state.get_account_by_id(account).data;
        match (asset, self.token) {
            (Asset::Token { .. }, Some(token)) => match TokenState::decode(data.actor_state(token)) {
                Some(TokenState::Holding(holding)) => holding.balance,
                _ => 0,
            },
            _ => native_token::decode_balance(data.actor_state(NATIVE_TOKEN_PROGRAM_ID)).unwrap_or(0),
        }
    }

    /// The actor holding `asset` on `account`.
    fn asset_actor(&self, asset: Asset, account: AccountId) -> Actor {
        match (asset, self.token) {
            (Asset::Token { .. }, Some(token)) => Actor::new(account, token),
            _ => Actor::native_balance(account),
        }
    }

    /// The account a non-zero fee is paid to (SPEC 6): the treasury, or its associated token account.
    fn fee_destination(&self, asset: Asset, treasury: AccountId) -> Option<AccountId> {
        match asset {
            Asset::Native => Some(treasury),
            Asset::Token { definition } => {
                let seed = vesting_core::ata_seed(&core(self.token?), &core(treasury), &definition);
                Some(AccountId::for_public_pda(&self.ata?, &PdaSeed::new(seed)))
            }
        }
    }

    fn fee_actors(&self) -> Vec<Actor> {
        self.fees.map(|(p, c)| vec![Actor::new(c, p)]).unwrap_or_default()
    }
}

fn row(op: &str, n: u64, r: Measured) -> Row {
    let mut row = Row::new(op, "native", n);
    match r {
        Ok((cycles, bytes)) => {
            row.cycles = Some(cycles);
            row.bytes = Some(bytes);
            row.status = "ok".into();
        }
        Err(e) => row.status = format!("failed: {}", e.lines().next().unwrap_or("")),
    }
    row
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Runs every operation; never fails on a missing program or message (that is recorded in the row).
pub fn run(bins: &Path) -> Result<Run> {
    let ids = std::env::var("CV_IDS").map_or_else(
        |_| crate::repo_root().join(vesting_e2e::DEFAULT_IDS),
        std::path::PathBuf::from,
    );
    let ids = std::fs::read_to_string(&ids)
        .ok()
        .and_then(|t| vesting_e2e::Ids::parse(&t).ok())
        .unwrap_or_default();
    let vesting = ids.get("vesting").unwrap_or_else(|| AccountId::new([0x5e; 32]));
    let mut inputs = vec![(
        "ids".to_owned(),
        if ids.programs.is_empty() {
            "none (fixed local ids)".to_owned()
        } else {
            "deploy ids file".to_owned()
        },
    )];

    let mut named: Vec<(AccountId, Program)> = vec![(clock_account_id(), programs::clock())];
    let vesting_bin = std::fs::read(bins.join("vesting.bin")).ok();
    if let Some(bytes) = &vesting_bin {
        inputs.push(("vesting.bin sha256".into(), sha256_hex(bytes)));
        named.push((
            vesting,
            Program::new(Cow::Owned(bytes.clone())).context("vesting.bin does not load")?,
        ));
    }
    let mut fees = None;
    let fees_bin = std::fs::read(bins.join("vesting_fees.bin")).ok();
    if let (Some(bytes), Some(p), Some(c)) = (&fees_bin, ids.get("vesting_fees"), ids.get("fees_config")) {
        inputs.push(("vesting_fees.bin sha256".into(), sha256_hex(bytes)));
        named.push((
            p,
            Program::new(Cow::Owned(bytes.clone())).context("vesting_fees.bin does not load")?,
        ));
        fees = Some((p, c));
    }
    let mut token = None;
    if let (Ok(bytes), Some(p)) = (std::fs::read(bins.join("token.bin")), ids.get("token")) {
        inputs.push(("token.bin sha256".into(), sha256_hex(&bytes)));
        named.push((p, Program::new(Cow::Owned(bytes)).context("token.bin does not load")?));
        token = Some(p);
    }
    let clock = ClockAccountData {
        block_id: 1,
        timestamp: T0,
    };
    let state = V03State::new().with_named_programs(named).with_public_accounts([(
        CLOCK_PROGRAM_ACCOUNT_ID,
        Account::default().with_actor_state(clock_account_id(), clock.to_bytes().into()),
    )]);
    let mut c = Chain {
        state,
        vesting,
        fees,
        token,
        ata: ids.get("ata"),
        block: 1,
        now: T0,
    };
    // The state `Init` leaves on a deployment (scripts/deploy_local.sh sends it right after deploying vesting_fees).
    c.set_fee_config(0, AccountId::new([0; 32]));
    let (creator_k, ben_k, to_k, heir_k) = (key(1), key(2), key(3), key(4));
    let (creator, ben, to, heir) = (id_of(&creator_k), id_of(&ben_k), id_of(&to_k), id_of(&heir_k));
    c.fund(creator, u128::MAX / 4);
    c.fund(ben, 1);
    c.fund(heir, 1);
    let ev = |k: &PrivateKey| PublicAccountEvidence::Key(PublicKey::new_from_private_key(k));

    let mut rows = Vec::new();
    // Protocol baselines: run on any state, so the tool always produces numbers.
    let mut base = row(
        "native_transfer",
        1,
        c.submit(
            Actor::native_balance(creator),
            &[Actor::native_balance(creator), Actor::native_balance(to)],
            &[&creator_k],
            vec![ev(&to_k)],
            &native_token::Message::Transfer { to, amount: 1 },
        ),
    );
    base.asset = "-".into();
    rows.push(base);
    let mut tick = row("clock_tick", 1, c.tick(T0));
    tick.asset = "-".into();
    rows.push(tick);

    if vesting_bin.is_none() {
        for op in [
            "create",
            "claim",
            "claim_as_of",
            "cancel",
            "make_non_cancelable",
            "signal_milestone",
            "transfer_beneficiary",
            "create_batch",
        ] {
            let mut r = Row::new(op, "native", 1);
            r.status = "not built (no vesting.bin: make guests)".into();
            rows.push(r);
        }
    } else {
        vesting_rows(&mut c, &mut rows, [&creator_k, &ben_k, &heir_k], to);
    }
    if vesting_bin.is_some() && c.token.is_some() {
        token_rows(&mut c, &mut rows, &creator_k, &ben_k, &to_k);
    } else {
        for op in ["create", "claim", "cancel", "create_batch"] {
            let mut r = Row::new(op, "token", 1);
            r.status = "not built (needs vesting.bin, token.bin and the token id in the ids file)".into();
            rows.push(r);
        }
    }
    if vesting_bin.is_some() && c.fees.is_some() {
        fee_path_rows(&mut c, &mut rows, &creator_k, &ben_k);
    } else {
        for asset in ["native", "token"] {
            let mut r = Row::new("create_with_fee", asset, 1);
            r.status = "not built (needs vesting.bin, vesting_fees.bin and their ids)".into();
            rows.push(r);
        }
    }
    fee_program_rows(fees_bin.as_deref(), &mut rows)?;
    Ok(Run {
        mode: "executor".into(),
        lez: "v0.4.0-rc3".into(),
        inputs,
        rows,
    })
}

fn linear_terms(ben: AccountId, transferable: bool) -> Terms {
    Terms {
        beneficiary: core(ben),
        asset: Asset::Native,
        total: TOTAL,
        kind: Kind::Linear {
            start: T0,
            cliff: T0,
            end: T0 + SPAN,
        },
        cancelable: true,
        transferable,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    }
}

fn create(c: &mut Chain, creator_k: &PrivateKey, id: [u8; 32], terms: Terms) -> Measured {
    let creator = id_of(creator_k);
    create_paying(c, creator_k, id, terms, 0, creator)
}

/// `Create` funded from the creator's own account, quoting `fee` to `treasury`.
fn create_paying(
    c: &mut Chain,
    creator_k: &PrivateKey,
    id: [u8; 32],
    terms: Terms,
    fee: u128,
    treasury: AccountId,
) -> Measured {
    let creator = id_of(creator_k);
    let schedule = c.schedule(creator, &id);
    let asset = terms.asset;
    let mut declared = vec![c.seat(creator), Actor::new(schedule, c.vesting)];
    match asset {
        Asset::Native => {
            declared.push(Actor::native_balance(creator));
            declared.push(Actor::native_balance(schedule));
        }
        Asset::Token { definition } => {
            declared.push(c.asset_actor(asset, schedule));
            declared.push(c.asset_actor(asset, creator));
            declared.push(c.asset_actor(asset, AccountId::new(*definition.value())));
        }
    }
    declared.extend(c.fee_actors());
    if fee > 0 {
        let destination = c.fee_destination(asset, treasury).ok_or("no fee destination")?;
        declared.push(c.asset_actor(asset, destination));
    }
    let msg = Message::Create {
        schedule_id: id,
        terms,
        source: core(creator),
        fee,
        treasury: core(treasury),
    };
    c.submit(c.seat(creator), &declared, &[creator_k], Vec::new(), &msg)
}

fn sid(n: u32) -> [u8; 32] {
    let mut id = [0; 32];
    id[..4].copy_from_slice(&n.to_le_bytes());
    id
}

fn vesting_rows(c: &mut Chain, rows: &mut Vec<Row>, [creator_k, ben_k, heir_k]: [&PrivateKey; 3], to: AccountId) {
    let (creator, ben, heir) = (id_of(creator_k), id_of(ben_k), id_of(heir_k));
    rows.push(row("create", 1, create(c, creator_k, sid(1), linear_terms(ben, true))));
    let s1 = c.schedule(creator, &sid(1));
    let _ = c.tick(T0 + SPAN / 4);
    let claim = |c: &mut Chain, signer: &PrivateKey, schedule: AccountId| {
        let who = id_of(signer);
        let declared = [
            c.seat(who),
            Actor::new(schedule, c.vesting),
            clock_actor(),
            Actor::native_balance(schedule),
            Actor::native_balance(to),
        ];
        c.submit(
            c.seat(who),
            &declared,
            &[signer],
            Vec::new(),
            &Message::Claim {
                schedule: core(schedule),
                to: core(to),
            },
        )
    };
    rows.push(row("claim", 1, claim(c, ben_k, s1)));
    let _ = c.tick(T0 + SPAN / 2);
    let declared = [
        c.seat(ben),
        Actor::new(s1, c.vesting),
        Actor::native_balance(s1),
        Actor::native_balance(to),
    ];
    let msg = Message::ClaimAsOf {
        schedule: core(s1),
        to: core(to),
        as_of: T0 + SPAN / 2,
    };
    rows.push(row(
        "claim_as_of",
        1,
        c.submit(c.seat(ben), &declared, &[ben_k], Vec::new(), &msg),
    ));
    let simple = |c: &mut Chain, k: &PrivateKey, s: AccountId, m: Message| {
        let who = id_of(k);
        c.submit(
            c.seat(who),
            &[c.seat(who), Actor::new(s, c.vesting)],
            &[k],
            Vec::new(),
            &m,
        )
    };
    rows.push(row(
        "transfer_beneficiary",
        1,
        simple(
            c,
            ben_k,
            s1,
            Message::TransferBeneficiary {
                schedule: core(s1),
                new_beneficiary: core(heir),
            },
        ),
    ));
    let declared = [
        c.seat(creator),
        Actor::new(s1, c.vesting),
        clock_actor(),
        Actor::native_balance(s1),
        Actor::native_balance(creator),
    ];
    rows.push(row(
        "cancel",
        1,
        c.submit(
            c.seat(creator),
            &declared,
            &[creator_k],
            Vec::new(),
            &Message::Cancel { schedule: core(s1) },
        ),
    ));

    let _ = create(c, creator_k, sid(2), linear_terms(ben, false));
    let s2 = c.schedule(creator, &sid(2));
    rows.push(row(
        "make_non_cancelable",
        1,
        simple(c, creator_k, s2, Message::MakeNonCancelable { schedule: core(s2) }),
    ));

    let mut ms = linear_terms(ben, false);
    ms.kind = Kind::Milestones {
        amounts: vec![TOTAL / 2, TOTAL / 2],
        signalled: 0,
    };
    let _ = create(c, creator_k, sid(3), ms);
    let s3 = c.schedule(creator, &sid(3));
    rows.push(row(
        "signal_milestone",
        1,
        simple(
            c,
            creator_k,
            s3,
            Message::SignalMilestone {
                schedule: core(s3),
                index: 0,
            },
        ),
    ));

    let mut next_id = 1000;
    let mut ok = false;
    for n in BATCHES {
        let r = batch(c, creator_k, Asset::Native, &mut next_id, n);
        ok |= r.is_ok();
        rows.push(row("create_batch", n, r));
    }
    batch_cap_rows(c, rows, creator_k, Asset::Native, &mut next_id, ok);
}

/// `CreateBatch` of `n` linear items with fresh ids, funded from the creator's own account.
fn batch(c: &mut Chain, creator_k: &PrivateKey, asset: Asset, next_id: &mut u32, n: u64) -> Measured {
    let creator = id_of(creator_k);
    let ben = id_of(&key(2));
    let items: Vec<BatchItem> = (0..n)
        .map(|_| {
            *next_id += 1;
            BatchItem {
                schedule_id: sid(*next_id),
                beneficiary: core(ben),
                total: TOTAL,
            }
        })
        .collect();
    let mut declared = vec![c.seat(creator)];
    for it in &items {
        let s = c.schedule(creator, &it.schedule_id);
        declared.push(Actor::new(s, c.vesting));
        declared.push(c.asset_actor(asset, s));
    }
    declared.push(c.asset_actor(asset, creator));
    if let Asset::Token { definition } = asset {
        declared.push(c.asset_actor(asset, AccountId::new(*definition.value())));
    }
    declared.extend(c.fee_actors());
    let common = BatchTerms {
        asset,
        shape: BatchShape::Linear {
            start: T0,
            cliff: T0,
            end: T0 + SPAN,
        },
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    };
    let msg = Message::CreateBatch {
        common,
        items,
        source: core(creator),
        fee: 0,
        treasury: core(creator),
    };
    c.submit(c.seat(creator), &declared, &[creator_k], Vec::new(), &msg)
}

/// A batch at the program's cap for `asset` (accepted, with its size in bytes) and one item more (refused, 25).
fn batch_cap_rows(
    c: &mut Chain,
    rows: &mut Vec<Row>,
    creator_k: &PrivateKey,
    asset: Asset,
    next_id: &mut u32,
    smaller_ok: bool,
) {
    let (cap, name) = match asset {
        Asset::Native => (vesting_core::MAX_BATCH_NATIVE as u64, "native"),
        Asset::Token { .. } => (vesting_core::MAX_BATCH_TOKEN as u64, "token"),
    };
    let mut r = row(
        "create_batch (program cap)",
        cap,
        batch(c, creator_k, asset, next_id, cap),
    );
    r.asset = name.into();
    rows.push(r);
    if smaller_ok {
        let mut r = Row::new("create_batch (cap + 1, must be refused)", name, cap + 1);
        r.status = match batch(c, creator_k, asset, next_id, cap + 1) {
            Ok((cy, _)) => format!("ACCEPTED ({cy} cycles): the cap does not hold"),
            Err(e) => format!("refused: {}", e.lines().next().unwrap_or("")),
        };
        rows.push(r);
    }
}

/// Token rows: a token definition and holdings set up as a user would (`NewDefinition`, `InitializeHolding`), then
/// create, claim and cancel of one token schedule, and token batches.
fn token_rows(c: &mut Chain, rows: &mut Vec<Row>, creator_k: &PrivateKey, ben_k: &PrivateKey, to_k: &PrivateKey) {
    let Some(token) = c.token else { return };
    let (creator, ben, to) = (id_of(creator_k), id_of(ben_k), id_of(to_k));
    let def_k = key(10);
    let def = id_of(&def_k);
    let root = Actor::new(def, token);
    let setup = c
        .submit(
            root,
            &[root, Actor::new(creator, token)],
            &[&def_k, creator_k],
            Vec::new(),
            &token_core::Message::NewDefinition {
                name: "VEST".to_owned(),
                total_supply: u128::MAX / 4,
                mint_authority: None,
                holding: creator,
            },
        )
        .and_then(|_| {
            let holding = Actor::new(to, token);
            c.submit(
                holding,
                &[holding, Actor::new(def, token)],
                &[to_k],
                Vec::new(),
                &token_core::Message::InitializeHolding { definition: def },
            )
        });
    let asset = Asset::Token { definition: core(def) };
    let tok = |mut r: Row| {
        r.asset = "token".into();
        r
    };
    if let Err(e) = setup {
        for op in ["create", "claim", "cancel", "create_batch"] {
            let mut r = Row::new(op, "token", 1);
            r.status = format!("failed: token setup: {}", e.lines().next().unwrap_or(""));
            rows.push(r);
        }
        return;
    }
    let mut terms = linear_terms(ben, false);
    terms.asset = asset;
    let start = c.now;
    if let Kind::Linear { start: s, cliff, end } = &mut terms.kind {
        (*s, *cliff, *end) = (start, start, start + SPAN);
    }
    rows.push(tok(row("create", 1, create(c, creator_k, sid(100), terms))));
    let s = c.schedule(creator, &sid(100));
    let _ = c.tick(start + SPAN / 4);
    let declared = [
        c.seat(ben),
        Actor::new(s, c.vesting),
        clock_actor(),
        c.asset_actor(asset, s),
        c.asset_actor(asset, to),
    ];
    let claim = Message::Claim {
        schedule: core(s),
        to: core(to),
    };
    rows.push(tok(row(
        "claim",
        1,
        c.submit(c.seat(ben), &declared, &[ben_k], Vec::new(), &claim),
    )));
    let _ = c.tick(start + SPAN / 2);
    // The refund goes back to the source holding (the default `refund_to`).
    let declared = [
        c.seat(creator),
        Actor::new(s, c.vesting),
        clock_actor(),
        c.asset_actor(asset, s),
        c.asset_actor(asset, creator),
    ];
    rows.push(tok(row(
        "cancel",
        1,
        c.submit(
            c.seat(creator),
            &declared,
            &[creator_k],
            Vec::new(),
            &Message::Cancel { schedule: core(s) },
        ),
    )));
    let mut next_id = 5000;
    let r = batch(c, creator_k, asset, &mut next_id, 8);
    let ok = r.is_ok();
    rows.push(tok(row("create_batch", 8, r)));
    batch_cap_rows(c, rows, creator_k, asset, &mut next_id, ok);
}

/// The fee path inside a creation: the fee checked by `vesting_fees` (`CheckFee`, a chained call) and paid to the
/// treasury (native) or to the treasury's associated token account (token), at [`FEE_BPS`].
fn fee_path_rows(c: &mut Chain, rows: &mut Vec<Row>, creator_k: &PrivateKey, ben_k: &PrivateKey) {
    let treasury_k = key(7);
    let treasury = id_of(&treasury_k);
    let ben = id_of(ben_k);
    c.fund(treasury, 1);
    c.set_fee_config(FEE_BPS, treasury);
    let fee = vesting_core::fee_for(TOTAL, FEE_BPS).unwrap_or(0);
    let label = format!("create_with_fee ({FEE_BPS} bps)");
    let before = c.balance(Asset::Native, treasury);
    let mut r = row(
        &label,
        1,
        create_paying(c, creator_k, sid(200), linear_terms(ben, false), fee, treasury),
    );
    if r.status == "ok" {
        r.status = paid(fee, c.balance(Asset::Native, treasury) - before, "the treasury");
    }
    rows.push(r);

    let mut r = Row::new(&label, "token", 1);
    let token_def = id_of(&key(10));
    let asset = Asset::Token {
        definition: core(token_def),
    };
    match (c.token, c.fee_destination(asset, treasury)) {
        (Some(token), Some(ata)) if !c.state.get_account_by_id(token_def).data.actor_state(token).is_empty() => {
            // The state the ATA program's `Create` leaves: an empty holding of the definition at the treasury's ATA.
            let holding = TokenState::Holding(TokenHolding {
                definition_id: token_def,
                balance: 0,
            });
            let mut account = c.state.get_account_by_id(ata);
            account.data.set_actor_state(token, holding.encode().into());
            c.state.force_insert_account(ata, account);
            let mut terms = linear_terms(ben, false);
            terms.asset = asset;
            r = row(&label, 1, create_paying(c, creator_k, sid(201), terms, fee, treasury));
            r.asset = "token".into();
            if r.status == "ok" {
                r.status = paid(fee, c.balance(asset, ata), "the treasury's associated token account");
            }
        }
        _ => r.status = "not built (needs token.bin and the ata id in the ids file)".into(),
    }
    rows.push(r);
    c.set_fee_config(0, AccountId::new([0; 32]));
}

/// The status of a fee row: `ok` only when `to` received exactly the quoted fee.
fn paid(fee: u128, received: u128, to: &str) -> String {
    if received == fee {
        format!("ok (fee {fee} received by {to})")
    } else {
        format!("fee not paid as quoted: {to} received {received}, quoted {fee}")
    }
}

/// `vesting_fees`'s own messages, sent alone to a deployment whose admin key the run holds: `Init`, `SetFee`, and
/// `CheckFee` (which `vesting` otherwise sends inside every creation).
fn fee_program_rows(bin: Option<&[u8]>, rows: &mut Vec<Row>) -> Result<()> {
    let ops = ["init", "set_fee", "check_fee"];
    let Some(bytes) = bin else {
        for op in ops {
            let mut r = Row::new(op, "-", 1);
            r.status = "not built (no vesting_fees.bin)".into();
            rows.push(r);
        }
        return Ok(());
    };
    // On LEZ 0.4 the program id is the loader header account and the deployer holds its key; `Init` makes the
    // deployer the admin. The ids file names the local deployment, whose key this tool does not hold.
    let deployer_k = key(0x11);
    let program = id_of(&deployer_k);
    let guest = Program::new(Cow::Owned(bytes.to_vec())).context("vesting_fees.bin does not load")?;
    let mut f = Chain {
        state: V03State::new().with_named_programs(vec![(program, guest)]),
        vesting: program,
        fees: None,
        token: None,
        ata: None,
        block: 1,
        now: T0,
    };
    let config = config_actor(program);
    let seat = Actor::new(program, program);
    let evidence = || {
        vec![PublicAccountEvidence::Pda {
            program,
            seed: CONFIG_SEED,
        }]
    };
    let mut fee_row = |op: &str, r: Measured| {
        let mut r = row(op, 1, r);
        r.asset = "-".into();
        rows.push(r);
    };
    let init = f.submit(
        seat,
        &[seat, config],
        &[&deployer_k],
        evidence(),
        &admin_op::<FeeMessage>(AdminOp::Init),
    );
    fee_row("init", init);
    let set = wire(FeeMessage::SetFee {
        rate_bps: FEE_BPS,
        treasury: core(id_of(&key(7))),
    });
    let set = f.submit(seat, &[seat, config], &[&deployer_k], evidence(), &set);
    fee_row("set_fee", set);
    let payer_k = key(0x12);
    let check = wire(FeeMessage::CheckFee {
        total: TOTAL,
        fee: vesting_core::fee_for(TOTAL, FEE_BPS).unwrap_or(0),
        treasury: core(id_of(&key(7))),
        asset: Asset::Native,
    });
    let check = f.submit(config, &[config], &[&payer_k], evidence(), &check);
    fee_row("check_fee", check);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five operations the RFP names (Performance 2) always get a row, measured or with the reason they are not,
    /// and the protocol baselines are measured on any state.
    #[test]
    fn p2_every_rfp_operation_has_a_cost_row() {
        let empty = std::env::temp_dir().join(format!("cv-cost-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).expect("temp dir");
        let run = run(&empty).expect("runs without guests");
        for op in [
            "create",
            "claim",
            "cancel",
            "signal_milestone",
            "transfer_beneficiary",
            "create_batch",
        ] {
            assert!(
                run.rows.iter().any(|r| r.op == op && r.asset == "native"),
                "no row for {op}"
            );
        }
        let tick = run.rows.iter().find(|r| r.op == "clock_tick").expect("clock row");
        assert!(tick.cycles.is_some_and(|c| c > 0), "{tick:?}");
        let _ = std::fs::remove_dir_all(empty);
    }
}
