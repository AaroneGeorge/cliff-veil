//! The JSON shapes of the SDK's answers (feature `json`): the CLI's `--json` output and the C ABI
//! (`sdk/ffi`) both print these, so a script, the mini-app and a C++ module read the same objects.
//!
//! Amounts are decimal strings (they are 128-bit); times are ISO-8601 UTC text with the raw milliseconds next to
//! them; accounts are base58 text.

use lee_core::account::AccountId;
use serde_json::{Value, json};
use vesting_core::{Asset, Kind, MAX_FEE_BPS, NextUnlock};

use crate::time::utc;
use crate::{
    DEV_MODE_NOTE, Destination, Error, FeeSettings, Outcome, Prepared, Programs, Receipt, ScheduleView, dev_mode,
    lez_id, privacy_disclosure,
};

/// An amount as JSON: a decimal string.
#[must_use]
pub fn amount(v: u128) -> Value {
    Value::String(v.to_string())
}

/// A schedule id as 64 hex characters.
#[must_use]
pub fn hex_id(id: &[u8; 32]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// The error object: `{"code": N | null, "name": "...", "message": "...", "action": "..." | null}`, plus
/// `payer`, `need`, `have` and `short` when an account cannot pay. `code` is the program's stable error code
/// (SPEC 5) when the error is a program refusal.
#[must_use]
pub fn error(err: &Error) -> Value {
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
        m.insert("need".into(), amount(*need));
        m.insert("have".into(), amount(*have));
        m.insert("short".into(), amount(need.saturating_sub(*have)));
    }
    v
}

/// A schedule's kind as one line of text (`linear: start ..., cliff ..., end ...` or `milestones: #0 400, ...`).
#[must_use]
pub fn kind_text(kind: &Kind) -> String {
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

/// `active`, `cancelled` or `settled`.
#[must_use]
pub fn standing_text(v: &ScheduleView) -> String {
    format!("{:?}", v.standing).to_lowercase()
}

/// One schedule: `show` prints it under `"schedule"`, `list` one per entry of `"schedules"`.
#[must_use]
pub fn schedule(v: &ScheduleView) -> Value {
    let s = &v.state;
    let id = |a: vesting_core::AccountId| lez_id(a).to_string();
    let kind_detail = match &s.kind {
        Kind::Linear { start, cliff, end } => json!({"linear": {"start": start, "cliff": cliff, "end": end}}),
        Kind::Milestones { amounts, signalled } => json!({"milestones": {
            "amounts": amounts.iter().map(|a| amount(*a)).collect::<Vec<_>>(),
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
        "total": amount(s.total), "vested": amount(v.vested), "claimed": amount(s.claimed),
        "claimable_now": amount(v.claimable), "locked": amount(v.locked),
        "next_unlock": v.next_unlock_text(),
        "next_unlock_kind": next_unlock_kind,
        "next_unlock_ms": next_unlock_ms,
        "escrow": v.escrow.map(amount),
        "cancelable": s.cancelable, "transferable": s.transferable,
        "cancel_authority": id(s.cancel_authority),
        "milestone_authority": id(s.milestone_authority),
        "refund_to": id(s.refund_to),
        "last_time_ms": s.last_time,
        "cancelled": s.cancelled.map(|c| json!({
            "at": utc(c.at), "at_ms": c.at, "vested_at_cancel": amount(c.vested_at_cancel), "refunded": amount(c.refunded),
        })),
        "clock": utc(v.now), "clock_ms": v.now,
    })
}

/// The amount claimable now (`claimable`, under `"claimable"`).
#[must_use]
pub fn claimable(v: &ScheduleView) -> Value {
    json!({"schedule": v.account.to_string(), "claimable_now": amount(v.claimable), "next_unlock": v.next_unlock_text(),
           "clock": utc(v.now)})
}

/// The fee configuration (`fee-show`, under `"fees"`).
#[must_use]
pub fn fees(f: &FeeSettings, p: &Programs) -> Value {
    json!({"deployed": f.deployed, "program": p.fees.map(|a| a.to_string()),
           "config_account": p.fees_config().map(|a| a.to_string()),
           "rate_bps": f.rate_bps, "max_fee_bps": MAX_FEE_BPS, "treasury": treasury_text(f)})
}

/// The fee treasury as text: its id, or `not set` while it is all zero.
#[must_use]
pub fn treasury_text(f: &FeeSettings) -> String {
    if f.treasury == AccountId::new([0; 32]) {
        "not set".to_owned()
    } else {
        f.treasury.to_string()
    }
}

/// One prepared transaction (one entry of `"transactions"`): the predicted outcome of `op`, its `estimate`, the
/// schedule id when given, and for a claim to a private account the `privacy_disclosure` and `dev_mode`.
#[must_use]
pub fn prepared(op: &str, p: &Prepared, schedule_id: Option<[u8; 32]>) -> Value {
    let e = &p.estimate;
    let mut value = match &p.outcome {
        Outcome::Created {
            schedules,
            total,
            protocol_fee,
            treasury,
        } => {
            json!({"schedules": schedules.iter().map(ToString::to_string).collect::<Vec<_>>(), "total": amount(*total),
                    "creation_fee": amount(*protocol_fee), "treasury": treasury.to_string()})
        }
        Outcome::Claim(c) => {
            let dest = match c.destination {
                Destination::Public(a) => json!({"kind": "public", "account": a.to_string()}),
                Destination::Private {
                    account,
                    blinded,
                    fresh,
                } => json!({"kind": "private", "account": account.to_string(), "one_time_address": blinded.to_string(),
                            "fresh": fresh, "saved_in_wallet": true}),
            };
            json!({"schedule": c.schedule.to_string(), "beneficiary": c.beneficiary.to_string(), "amount": amount(c.amount),
                   "at": utc(c.at), "at_ms": c.at, "claimed_after": amount(c.claimed_total),
                   "still_to_come": amount(c.remaining), "destination": dest, "fee_payer_holds": amount(c.fee_payer_holds),
                   "reserve": amount(c.reserve), "headroom_blocks": c.headroom_blocks,
                   "fee_payer": c.fee_payer.to_string(), "fee_payer_choice": c.fee_payer_choice.tag(),
                   "fee_payer_reason": c.fee_payer_reason(),
                   "headroom_reserve": amount(c.headroom_reserve), "headroom_covered": c.headroom_covered()})
        }
        Outcome::Cancelled {
            at,
            vested_at_cancel,
            refunded,
            refund_to,
            still_claimable,
        } => json!({"at": utc(*at), "at_ms": at, "vested_at_cancel": amount(*vested_at_cancel),
                    "refunded": amount(*refunded), "refund_to": refund_to.to_string(),
                    "still_claimable": amount(*still_claimable)}),
        Outcome::MadeNonCancelable => json!({}),
        Outcome::Signalled { index, amount: a } => json!({"index": index, "unlocks": amount(*a)}),
        Outcome::Transferred { from, to } => json!({"from": from.to_string(), "to": to.to_string()}),
    };
    if let Value::Object(m) = &mut value {
        m.insert("operation".into(), Value::String(op.to_owned()));
        m.insert(
            "estimate".into(),
            json!({"network_fee": amount(e.fee), "reserve": amount(e.reserve), "payer": e.payer.to_string(),
                   "private": e.private, "cycles": e.cycles, "headroom_blocks": e.headroom_blocks,
                   "headroom_reserve": amount(e.headroom_reserve)}),
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
            if dev_mode() {
                m.insert("dev_mode_note".into(), Value::String(DEV_MODE_NOTE.to_owned()));
            }
        }
    }
    value
}

/// One included transaction (one entry of `"sent"`).
#[must_use]
pub fn receipt(r: &Receipt) -> Value {
    json!({"hash": r.hash, "block": r.block, "declared_fee": r.declared_fee.map(amount)})
}

/// The private account a claim paid to and its native balance as the wallet sees it after the send
/// (`"private_balance_after"`).
#[must_use]
pub fn private_balance(account: AccountId, native: Option<u128>) -> Value {
    json!({"account": account.to_string(), "native": native.map(amount)})
}

/// The disclosure document (`disclosure`): `{"ok": true, "privacy_disclosure": "..."}` plus `dev_mode`.
#[must_use]
pub fn disclosure() -> Value {
    json!({ "ok": true, "privacy_disclosure": privacy_disclosure(), "dev_mode": dev_mode() })
}
