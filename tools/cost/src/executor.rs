//! Executor mode: every operation through LEZ's public execution on a local state, with its cycle count.

use std::borrow::Cow;
use std::path::Path;

use anyhow::{Context as _, Result};
use borsh::BorshSerialize;
use clock_core::{CLOCK_PROGRAM_ACCOUNT_ID, ClockAccountData, clock_account_id, clock_actor};
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
use sha2::Digest as _;
use vesting_core::{Asset, BatchItem, BatchShape, BatchTerms, Kind, Message, Terms};

use crate::{Row, Run};

/// The chain's execution cap per transaction (rc3 fee market `MAX_GAS_EXEC`).
const CAP: u64 = 10_000_000;
const T0: u64 = 1_700_000_000_000;
const SPAN: u64 = 1_000_000;
const TOTAL: u128 = 1_000_000;
/// Batch sizes measured besides the maximum search.
const BATCHES: [u64; 3] = [1, 8, 32];

struct Chain {
    state: V03State,
    vesting: AccountId,
    fees: Option<(AccountId, AccountId)>,
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

    /// Runs one transaction at the current block and clock time under the chain cap; returns its cycles.
    fn submit(
        &mut self,
        root: Actor,
        declared: &[Actor],
        signers: &[&PrivateKey],
        evidence: Vec<PublicAccountEvidence>,
        message: &impl BorshSerialize,
    ) -> Result<u64, String> {
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
        let (diff, charge) =
            ValidatedStateDiff::from_public_transaction_with_cycle_budget(&tx, &self.state, self.block, self.now, CAP)
                .map_err(|e| format!("{e}"))?;
        if let Some(revert) = charge.revert {
            return Err(revert);
        }
        self.state.apply_state_diff(diff).map_err(|e| format!("{e}"))?;
        Ok(charge.cycles)
    }

    fn tick(&mut self, now: u64) -> Result<u64, String> {
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

    /// Writes an initialised `vesting_fees` config at rate 0, as on a deployment after `Init`.
    fn init_fee_config(&mut self) {
        let Some((program, config)) = self.fees else { return };
        let mut account = self.state.get_account_by_id(config);
        let bytes = borsh::to_vec(&vesting_fees::FeeConfig::default()).expect("config encodes");
        account.data.set_actor_state(program, bytes.into());
        self.state.force_insert_account(config, account);
    }

    fn fee_actors(&self) -> Vec<Actor> {
        self.fees.map(|(p, c)| vec![Actor::new(c, p)]).unwrap_or_default()
    }
}

fn row(op: &str, n: u64, r: Result<u64, String>) -> Row {
    let mut row = Row::new(op, "native", n);
    match r {
        Ok(c) => {
            row.cycles = Some(c);
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
    if let (Ok(bytes), Some(p), Some(c)) = (
        std::fs::read(bins.join("vesting_fees.bin")),
        ids.get("vesting_fees"),
        ids.get("fees_config"),
    ) {
        inputs.push(("vesting_fees.bin sha256".into(), sha256_hex(&bytes)));
        named.push((
            p,
            Program::new(Cow::Owned(bytes)).context("vesting_fees.bin does not load")?,
        ));
        fees = Some((p, c));
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
        block: 1,
        now: T0,
    };
    // The state `Init` leaves on a deployment (scripts/deploy_local.sh sends it right after deploying vesting_fees).
    c.init_fee_config();
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
    for op in ["create", "claim", "cancel"] {
        let mut r = Row::new(op, "token", 1);
        r.status = "not measured yet (token setup: definition and holdings)".into();
        rows.push(r);
    }
    fee_rows(&mut rows);
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

fn create(c: &mut Chain, creator_k: &PrivateKey, id: [u8; 32], terms: Terms) -> Result<u64, String> {
    let creator = id_of(creator_k);
    let schedule = c.schedule(creator, &id);
    let mut declared = vec![
        c.seat(creator),
        Actor::new(schedule, c.vesting),
        Actor::native_balance(creator),
        Actor::native_balance(schedule),
    ];
    declared.extend(c.fee_actors());
    let msg = Message::Create {
        schedule_id: id,
        terms,
        source: core(creator),
        fee: 0,
        treasury: core(creator),
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
    let mut batch = |c: &mut Chain, n: u64| {
        let items: Vec<BatchItem> = (0..n)
            .map(|_| {
                next_id += 1;
                BatchItem {
                    schedule_id: sid(next_id),
                    beneficiary: core(ben),
                    total: TOTAL,
                }
            })
            .collect();
        let mut declared = vec![c.seat(creator)];
        for it in &items {
            let s = c.schedule(creator, &it.schedule_id);
            declared.push(Actor::new(s, c.vesting));
            declared.push(Actor::native_balance(s));
        }
        declared.push(Actor::native_balance(creator));
        declared.extend(c.fee_actors());
        let common = BatchTerms {
            asset: Asset::Native,
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
    };
    let mut ok = false;
    for n in BATCHES {
        let r = batch(c, n);
        ok |= r.is_ok();
        rows.push(row("create_batch", n, r));
    }
    let cap = vesting_core::MAX_BATCH_NATIVE as u64;
    let r = batch(c, cap);
    rows.push(row("create_batch (program cap)", cap, r));
    if ok {
        let mut r = Row::new("create_batch (cap + 1, must be refused)", "native", cap + 1);
        r.status = match batch(c, cap + 1) {
            Ok(cy) => format!("ACCEPTED ({cy} cycles): the cap does not hold"),
            Err(e) => format!("refused: {}", e.lines().next().unwrap_or("")),
        };
        rows.push(r);
    }
}

#[cfg(feature = "fees")]
fn fee_rows(rows: &mut Vec<Row>) {
    for op in ["check_fee", "set_fee"] {
        let mut r = Row::new(op, "-", 1);
        r.status = "not measured yet (vesting_fees messages)".into();
        rows.push(r);
    }
}

#[cfg(not(feature = "fees"))]
fn fee_rows(rows: &mut Vec<Row>) {
    for op in ["check_fee", "set_fee"] {
        let mut r = Row::new(op, "-", 1);
        r.status = "not built (vesting_fees; build with --features fees)".into();
        rows.push(r);
    }
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
