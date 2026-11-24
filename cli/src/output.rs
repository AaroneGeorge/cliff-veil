//! What the CLI prints: text for people, or (with `--json`) one JSON object collected over the command.

use std::cell::RefCell;

use cliff_veil_sdk::backend::Backend as _;
use cliff_veil_sdk::time::utc;
use cliff_veil_sdk::{
    Client, DEV_MODE_NOTE, Destination, Error, FeeSettings, Outcome, Prepared, Programs, Receipt, ScheduleView,
    SequencerBackend, dev_mode, privacy_disclosure,
};
use serde_json::{Value, json};
use vesting_core::{Asset, Kind, MAX_FEE_BPS, NextUnlock};

/// Prints text, or collects one JSON object printed by [`Printer::finish`].
pub struct Printer {
    /// JSON instead of text.
    pub json: bool,
}

thread_local! {
    static DOC: RefCell<serde_json::Map<String, Value>> = RefCell::new(serde_json::Map::new());
}

fn put(key: &str, value: Value) {
    DOC.with(|d| {
        d.borrow_mut().insert(key.to_owned(), value);
    });
}

/// The JSON shape of every error: `{"code": N | null, "name": "...", "message": "...", "action": "..." | null}`.
/// `code` is the program's stable error code (SPEC 5) when the error is a program refusal.
pub fn error_json(e: &anyhow::Error) -> Value {
    match e.downcast_ref::<Error>() {
        Some(err) => {
            let mut v = json!({
                "code": err.code(),
                "name": err.name(),
                "message": err.message(),
                "action": err.action(),
            });
            let amounts = match err {
                Error::PrivateFeeShortfall { payer, need, have, .. }
                | Error::InsufficientBalance {
                    account: payer,
                    need,
                    have,
                    ..
                } => Some((payer, need, have)),
                _ => None,
            };
            if let (Some((payer, need, have)), Value::Object(m)) = (amounts, &mut v) {
                m.insert("payer".into(), Value::String(payer.to_string()));
                m.insert("need".into(), n(*need));
                m.insert("have".into(), n(*have));
                m.insert("short".into(), n(need.saturating_sub(*have)));
            }
            v
        }
        None => json!({ "code": null, "name": "Input", "message": format!("{e:#}"), "action": null }),
    }
}

fn kind_text(kind: &Kind) -> String {
    match kind {
        Kind::Linear { start, cliff, end } => {
            format!(
                "linear: start {}, cliff {}, end {}",
                utc(*start),
                utc(*cliff),
                utc(*end)
            )
        }
        Kind::Milestones { amounts, signalled } => {
            let parts: Vec<String> = amounts
                .iter()
                .enumerate()
                .map(|(i, a)| format!("#{i} {a}{}", if signalled >> i & 1 == 1 { " (signalled)" } else { "" }))
                .collect();
            format!("milestones: {}", parts.join(", "))
        }
    }
}

fn standing_text(v: &ScheduleView) -> String {
    format!("{:?}", v.standing).to_lowercase()
}

/// The JSON object of one schedule: printed by `show` under "schedule", and one per entry by `list`. Amounts are
/// decimal strings (they are 128-bit); times are ISO-8601 UTC text with the raw milliseconds next to them.
pub fn schedule_json(v: &ScheduleView) -> Value {
    let s = &v.state;
    let id = |a: vesting_core::AccountId| cliff_veil_sdk::lez_id(a).to_string();
    let kind_detail = match &s.kind {
        Kind::Linear { start, cliff, end } => json!({"linear": {"start": start, "cliff": cliff, "end": end}}),
        Kind::Milestones { amounts, signalled } => json!({"milestones": {
            "amounts": amounts.iter().map(|a| n(*a)).collect::<Vec<_>>(),
            "signalled": signalled,
        }}),
    };
    let (next_unlock_kind, next_unlock_ms) = match v.next_unlock {
        NextUnlock::At(t) => ("at", Some(t)),
        NextUnlock::WhenMilestoneSignalled => ("milestone", None),
        NextUnlock::Never => ("never", None),
    };
    let (asset, token_definition) = match s.asset {
        Asset::Native => ("native", None),
        Asset::Token { definition } => ("token", Some(id(definition))),
    };
    json!({
        "account": v.account.to_string(),
        "schedule_id": hex_id(&s.schedule_id),
        "standing": standing_text(v),
        "creator": id(s.creator),
        "beneficiary": id(s.beneficiary),
        "asset": asset,
        "token_definition": token_definition,
        "kind": kind_text(&s.kind),
        "kind_detail": kind_detail,
        "total": n(s.total), "vested": n(v.vested), "claimed": n(s.claimed),
        "claimable_now": n(v.claimable), "locked": n(v.locked),
        "next_unlock": v.next_unlock_text(),
        "next_unlock_kind": next_unlock_kind,
        "next_unlock_ms": next_unlock_ms,
        "escrow": v.escrow.map(n),
        "cancelable": s.cancelable, "transferable": s.transferable,
        "cancel_authority": id(s.cancel_authority),
        "milestone_authority": id(s.milestone_authority),
        "refund_to": id(s.refund_to),
        "last_time_ms": s.last_time,
        "cancelled": s.cancelled.map(|c| json!({
            "at": utc(c.at), "at_ms": c.at, "vested_at_cancel": n(c.vested_at_cancel), "refunded": n(c.refunded),
        })),
        "clock": utc(v.now), "clock_ms": v.now,
    })
}

fn n(v: u128) -> Value {
    Value::String(v.to_string())
}

fn asset_text(asset: &Asset) -> String {
    match asset {
        Asset::Native => "native".to_owned(),
        Asset::Token { definition } => format!("token {}", cliff_veil_sdk::lez_id(*definition)),
    }
}

impl Printer {
    pub fn error(&self, e: &anyhow::Error) {
        if self.json {
            DOC.with(|d| {
                let mut d = d.borrow_mut();
                d.insert("ok".into(), Value::Bool(false));
                d.insert("error".into(), error_json(e));
                println!("{}", Value::Object(d.clone()));
            });
        } else {
            eprintln!("error: {e:#}");
        }
    }

    pub fn note(&self, text: &str) {
        if self.json {
            put("note", Value::String(text.to_owned()));
        } else {
            println!("{text}");
        }
    }

    pub fn prompt(&self, text: &str) {
        use std::io::Write as _;
        eprint!("{text}");
        std::io::stderr().flush().ok();
    }

    pub fn finish(&self) {
        if self.json {
            DOC.with(|d| {
                let mut d = d.borrow_mut();
                d.insert("ok".into(), Value::Bool(true));
                println!(
                    "{}",
                    serde_json::to_string_pretty(&Value::Object(d.clone())).unwrap_or_default()
                );
            });
        }
    }

    pub fn disclosure(&self) {
        if self.json {
            println!("{}", json!({ "ok": true, "privacy_disclosure": privacy_disclosure() }));
        } else {
            println!("{}", privacy_disclosure());
        }
    }

    pub fn schedule(&self, v: &ScheduleView) {
        let s = &v.state;
        let kind = kind_text(&s.kind);
        let standing = standing_text(v);
        if self.json {
            put("schedule", schedule_json(v));
            return;
        }
        println!("schedule            {}", v.account);
        println!("standing            {standing}");
        println!("creator             {}", cliff_veil_sdk::lez_id(s.creator));
        println!("beneficiary         {}", cliff_veil_sdk::lez_id(s.beneficiary));
        println!("asset               {}", asset_text(&s.asset));
        println!("kind                {kind}");
        println!("total               {}", s.total);
        println!("vested              {}", v.vested);
        println!("claimed             {}", s.claimed);
        println!("claimable now       {}", v.claimable);
        println!("locked              {}", v.locked);
        println!("next unlock         {}", v.next_unlock_text());
        if let Some(e) = v.escrow {
            println!("escrow              {e}");
        }
        println!("cancelable          {}", if s.cancelable { "yes" } else { "no" });
        println!("transferable        {}", if s.transferable { "yes" } else { "no" });
        println!("cancel authority    {}", cliff_veil_sdk::lez_id(s.cancel_authority));
        println!("milestone authority {}", cliff_veil_sdk::lez_id(s.milestone_authority));
        println!("refund to           {}", cliff_veil_sdk::lez_id(s.refund_to));
        if let Some(c) = s.cancelled {
            println!(
                "cancelled           at {}: vested {}, refunded {}",
                utc(c.at),
                c.vested_at_cancel,
                c.refunded
            );
        }
        println!("clock               {} ({} ms)", utc(v.now), v.now);
    }

    pub fn address(&self, account: lee_core::account::AccountId) {
        if self.json {
            put("schedule", Value::String(account.to_string()));
        } else {
            println!("{account}");
        }
    }

    pub fn list(&self, views: &[ScheduleView], scanned_to: u64) {
        if self.json {
            put("schedules", Value::Array(views.iter().map(schedule_json).collect()));
            put("scanned_to_block", Value::from(scanned_to));
            return;
        }
        println!("{} schedule(s), blocks scanned to {scanned_to}", views.len());
        for v in views {
            println!(
                "{}  {:<9}  total {}  claimed {}  claimable now {}  next unlock {}",
                v.account,
                standing_text(v),
                v.state.total,
                v.state.claimed,
                v.claimable,
                v.next_unlock_text()
            );
        }
    }

    pub fn claimable(&self, v: &ScheduleView) {
        if self.json {
            put(
                "claimable",
                json!({"schedule": v.account.to_string(), "claimable_now": n(v.claimable), "next_unlock": v.next_unlock_text(), "clock": utc(v.now)}),
            );
        } else {
            println!(
                "claimable now {} (clock {}); next unlock {}",
                v.claimable,
                utc(v.now),
                v.next_unlock_text()
            );
        }
    }

    pub fn fees(&self, f: &FeeSettings, p: &Programs) {
        let treasury = if f.treasury == lee_core::account::AccountId::new([0; 32]) {
            "not set".to_owned()
        } else {
            f.treasury.to_string()
        };
        if self.json {
            put(
                "fees",
                json!({"deployed": f.deployed, "program": p.fees.map(|a| a.to_string()), "config_account": p.fees_config().map(|a| a.to_string()),
                       "rate_bps": f.rate_bps, "max_fee_bps": MAX_FEE_BPS, "treasury": treasury}),
            );
        } else if !f.deployed {
            println!("no vesting_fees program configured: creations carry a zero fee");
        } else {
            println!(
                "creation fee rate   {} bps (hard cap {MAX_FEE_BPS} bps = 1%)",
                f.rate_bps
            );
            println!("treasury            {treasury}");
            println!("charged             at creation only, from the funding account, in the schedule's asset");
            if let Some(c) = p.fees_config() {
                println!("config account      {c}");
            }
        }
    }

    pub fn prepared(&self, op: &str, p: &Prepared, schedule_id: Option<[u8; 32]>) {
        let e = &p.estimate;
        let (mut value, text) = match &p.outcome {
            Outcome::Created {
                schedules,
                total,
                protocol_fee,
                treasury,
            } => (
                json!({"schedules": schedules.iter().map(ToString::to_string).collect::<Vec<_>>(), "total": n(*total),
                       "creation_fee": n(*protocol_fee), "treasury": treasury.to_string()}),
                format!(
                    "{op}: {} schedule(s), escrow {total}, creation fee {protocol_fee}\n{}",
                    schedules.len(),
                    schedules
                        .iter()
                        .map(|s| format!("  schedule {s}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            ),
            Outcome::Claim(c) => {
                let dest = match c.destination {
                    Destination::Public(a) => json!({"kind": "public", "account": a.to_string()}),
                    Destination::Private {
                        account,
                        blinded,
                        fresh,
                    } => {
                        json!({"kind": "private", "account": account.to_string(), "one_time_address": blinded.to_string(),
                               "fresh": fresh, "saved_in_wallet": true})
                    }
                };
                let mut text = c.render();
                if let Destination::Private {
                    account, fresh: true, ..
                } = c.destination
                {
                    text.push_str(&format!(
                        "\n  new private account {account} is saved in this wallet (also on a dry run); a later claim \
                         can name it with --private-to {account}"
                    ));
                }
                if p.tx.is_private() {
                    text.push_str("\n\n");
                    text.push_str(privacy_disclosure());
                    if dev_mode() {
                        text.push_str("\n\n");
                        text.push_str(DEV_MODE_NOTE);
                    }
                }
                (
                    json!({"schedule": c.schedule.to_string(), "beneficiary": c.beneficiary.to_string(), "amount": n(c.amount),
                           "at": utc(c.at), "at_ms": c.at, "claimed_after": n(c.claimed_total), "still_to_come": n(c.remaining),
                           "destination": dest, "fee_payer_holds": n(c.fee_payer_holds),
                           "reserve": n(c.reserve), "headroom_blocks": c.headroom_blocks,
                           "fee_payer": c.fee_payer.to_string(), "fee_payer_choice": c.fee_payer_choice.tag(),
                           "fee_payer_reason": c.fee_payer_reason(),
                           "headroom_reserve": n(c.headroom_reserve), "headroom_covered": c.headroom_covered()}),
                    text,
                )
            }
            Outcome::Cancelled {
                at,
                vested_at_cancel,
                refunded,
                refund_to,
                still_claimable,
            } => (
                json!({"at": utc(*at), "at_ms": at, "vested_at_cancel": n(*vested_at_cancel), "refunded": n(*refunded),
                       "refund_to": refund_to.to_string(), "still_claimable": n(*still_claimable)}),
                format!(
                    "cancel: refunds {refunded} to {refund_to}; vested {vested_at_cancel} (of it {still_claimable} unclaimed, \
                     stays claimable); computed at {} or the later clock time of inclusion",
                    utc(*at)
                ),
            ),
            Outcome::MadeNonCancelable => (
                json!({}),
                "make-non-cancelable: the schedule can no longer be cancelled".to_owned(),
            ),
            Outcome::Signalled { index, amount } => (
                json!({"index": index, "unlocks": n(*amount)}),
                format!("signal: milestone #{index} unlocks {amount}"),
            ),
            Outcome::Transferred { from, to } => (
                json!({"from": from.to_string(), "to": to.to_string()}),
                format!("transfer: beneficiary {from} -> {to}"),
            ),
        };
        if self.json {
            if let Value::Object(m) = &mut value {
                m.insert("operation".into(), Value::String(op.to_owned()));
                m.insert(
                    "estimate".into(),
                    json!({"network_fee": n(e.fee), "reserve": n(e.reserve), "payer": e.payer.to_string(), "private": e.private, "cycles": e.cycles,
                           "headroom_blocks": e.headroom_blocks, "headroom_reserve": n(e.headroom_reserve)}),
                );
                if let Some(id) = schedule_id {
                    m.insert("schedule_id".into(), Value::String(hex_id(&id)));
                }
                if p.tx.is_private() {
                    m.insert(
                        "privacy_disclosure".into(),
                        Value::String(privacy_disclosure().to_owned()),
                    );
                    m.insert("dev_mode".into(), Value::Bool(dev_mode()));
                }
            }
            DOC.with(|d| {
                let mut d = d.borrow_mut();
                let list = d.entry("transactions").or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(l) = list {
                    l.push(value);
                }
            });
            return;
        }
        println!("{text}");
        if !matches!(p.outcome, Outcome::Claim(_)) {
            println!("network fee         ~{} (estimated), paid by {}", e.fee, e.payer);
        }
        if let Some(id) = schedule_id {
            println!("schedule id         {}", hex_id(&id));
        }
    }

    pub fn sent(&self, r: &Receipt) {
        if self.json {
            DOC.with(|d| {
                let mut d = d.borrow_mut();
                let list = d.entry("sent").or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(l) = list {
                    l.push(json!({"hash": r.hash, "block": r.block, "declared_fee": r.declared_fee.map(n)}));
                }
            });
        } else {
            println!("sent: tx {} in block {}", r.hash, r.block);
        }
    }

    pub fn private_balance(&self, client: &Client<SequencerBackend>, p: &Prepared) {
        let Outcome::Claim(c) = &p.outcome else { return };
        let Destination::Private { account, .. } = c.destination else {
            return;
        };
        let balance = client.backend().private_native_balance(account);
        if self.json {
            put(
                "private_balance_after",
                json!({"account": account.to_string(), "native": balance.map(n)}),
            );
        } else if let Some(b) = balance {
            println!("private account {account} now holds {b} (wallet view, synced)");
        }
    }
}

fn hex_id(id: &[u8; 32]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u7_json_errors_have_code_name_message_and_action() {
        let e: anyhow::Error = Error::from_text("cliff-veil error 18: this milestone was already signalled").into();
        let v = error_json(&e);
        assert_eq!(v["code"], 18);
        assert_eq!(v["name"], "MilestoneAlreadySignalled");
        assert_eq!(
            v["message"],
            "cliff-veil error 18: this milestone was already signalled"
        );
        assert_eq!(v["action"], "this milestone is already unlocked; signal another one");

        let e: anyhow::Error = Error::from_core(vesting_core::VestingError::NothingClaimable {
            next: vesting_core::NextUnlock::At(0),
        })
        .into();
        let v = error_json(&e);
        assert_eq!(v["code"], 14);
        assert!(v["message"].as_str().unwrap().contains("1970-01-01T00:00:00.000Z"));
        assert!(v["action"].is_string());

        let e: anyhow::Error = Error::PrivateFeeShortfall {
            payer: lee_core::account::AccountId::new([3; 32]),
            need: 50,
            have: 4,
            hint: "fund it",
        }
        .into();
        let v = error_json(&e);
        assert_eq!(
            (v["name"].as_str(), v["need"].as_str(), v["have"].as_str()),
            (Some("PrivateFeeShortfall"), Some("50"), Some("4"))
        );
        assert_eq!(v["short"], "46");
        assert_eq!(v["payer"], lee_core::account::AccountId::new([3; 32]).to_string());

        let v = error_json(&anyhow::anyhow!("bad input"));
        assert!(v["code"].is_null() && v["action"].is_null());
        assert_eq!(v["message"], "bad input");
    }

    fn view(kind: Kind, cancelled: Option<vesting_core::Cancellation>) -> ScheduleView {
        let creator = vesting_core::AccountId::new([1; 32]);
        let terms = vesting_core::Terms {
            beneficiary: vesting_core::AccountId::new([2; 32]),
            asset: Asset::Token {
                definition: vesting_core::AccountId::new([3; 32]),
            },
            total: 1_000,
            kind,
            cancelable: true,
            transferable: false,
            cancel_authority: None,
            milestone_authority: None,
            refund_to: None,
        };
        let mut state = terms.into_state(creator, [4; 32], creator).unwrap();
        state.cancelled = cancelled;
        ScheduleView {
            account: lee_core::account::AccountId::new([5; 32]),
            state,
            now: 10,
            vested: 0,
            claimable: 0,
            locked: 1_000,
            next_unlock: NextUnlock::At(500),
            escrow: None,
            standing: cliff_veil_sdk::Standing::Active,
        }
    }

    #[test]
    fn u3_show_json_has_structured_fields() {
        let v = schedule_json(&view(
            Kind::Linear {
                start: 1,
                cliff: 2,
                end: 3,
            },
            None,
        ));
        assert_eq!(v["kind_detail"]["linear"]["cliff"], 2);
        assert_eq!(
            (v["next_unlock_kind"].as_str(), v["next_unlock_ms"].as_u64()),
            (Some("at"), Some(500))
        );
        assert_eq!(v["asset"], "token");
        assert_eq!(
            v["token_definition"],
            lee_core::account::AccountId::new([3; 32]).to_string()
        );
        assert!(v["cancelled"].is_null());

        let c = vesting_core::Cancellation {
            at: 77,
            vested_at_cancel: 10,
            refunded: 990,
        };
        let mut m = view(
            Kind::Milestones {
                amounts: vec![400, 600],
                signalled: 0,
            },
            Some(c),
        );
        m.next_unlock = NextUnlock::WhenMilestoneSignalled;
        let v = schedule_json(&m);
        assert_eq!(v["kind_detail"]["milestones"]["amounts"], json!(["400", "600"]));
        assert_eq!(v["kind_detail"]["milestones"]["signalled"], 0);
        assert_eq!(
            (v["next_unlock_kind"].as_str(), v["next_unlock_ms"].is_null()),
            (Some("milestone"), true)
        );
        assert_eq!(v["cancelled"]["at_ms"], 77);
        m.next_unlock = NextUnlock::Never;
        assert_eq!(schedule_json(&m)["next_unlock_kind"], "never");
    }
}
