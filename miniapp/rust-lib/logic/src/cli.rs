//! Reads the `cliff-veil --json` documents (`cli/src/output.rs`) into the mini-app's types ([`crate::model`]).
//!
//! The command line prints one object per run: `{"ok": true, "schedule": {...}}` for `show`,
//! `{"ok": true, "transactions": [{...}], "sent": [{...}]?}` for an operation (`--dry-run` stops before `sent`),
//! `{"ok": true, "privacy_disclosure": "..."}` for `disclosure`, and `{"ok": false, "error": "...", "code": n|null}`
//! for a refusal. Some fields are rendered text there (the schedule's kind, the next unlock, times in UTC); they are
//! read back exactly here, with [`crate::time::parse_utc`] for every UTC time the SDK prints.

use serde_json::Value;

use crate::model::{
    Asset, Cancellation, Destination, ErrorInfo, Kind, NextUnlock, Outcome, PreparedClaim, PreparedOp, Receipt,
    ScheduleStatus, Standing,
};
use crate::time::parse_utc;

fn bad(what: &str, detail: impl std::fmt::Display) -> ErrorInfo {
    ErrorInfo::new(
        "backend_bad_output",
        format!("the {what} printed by the cliff-veil command line does not read: {detail}"),
        Some("use the cliff-veil command line of the same release as this mini-app"),
    )
}

fn field<'a>(v: &'a Value, what: &str, key: &str) -> Result<&'a Value, ErrorInfo> {
    v.get(key)
        .filter(|x| !x.is_null())
        .ok_or_else(|| bad(what, format!("no `{key}`")))
}

fn text(v: &Value, what: &str, key: &str) -> Result<String, ErrorInfo> {
    field(v, what, key)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| bad(what, format!("`{key}` is not text")))
}

fn amount(v: &Value, what: &str, key: &str) -> Result<u128, ErrorInfo> {
    match field(v, what, key)? {
        Value::String(s) => s.parse().map_err(|_| bad(what, format!("`{key}` is not an amount"))),
        Value::Number(n) => n
            .as_u64()
            .map(u128::from)
            .ok_or_else(|| bad(what, format!("`{key}` is not an amount"))),
        _ => Err(bad(what, format!("`{key}` is not an amount"))),
    }
}

fn int(v: &Value, what: &str, key: &str) -> Result<u64, ErrorInfo> {
    field(v, what, key)?
        .as_u64()
        .ok_or_else(|| bad(what, format!("`{key}` is not an integer")))
}

fn flag(v: &Value, what: &str, key: &str) -> Result<bool, ErrorInfo> {
    field(v, what, key)?
        .as_bool()
        .ok_or_else(|| bad(what, format!("`{key}` is not true or false")))
}

fn utc_field(v: &Value, what: &str, key: &str) -> Result<u64, ErrorInfo> {
    parse_utc(&text(v, what, key)?).map_err(|e| bad(what, e))
}

/// `native` or `token <definition>`.
fn asset(s: &str) -> Result<Asset, ErrorInfo> {
    match s.split_once(' ') {
        None if s == "native" => Ok(Asset::Native),
        Some(("token", d)) => Ok(Asset::Token {
            definition: d.trim().to_owned(),
        }),
        _ => Err(bad("schedule", format!("unknown asset `{s}`"))),
    }
}

/// `linear: start <utc>, cliff <utc>, end <utc>` or `milestones: #0 300 (signalled), #1 700`.
fn kind(s: &str) -> Result<Kind, ErrorInfo> {
    let err = || bad("schedule", format!("unknown kind `{s}`"));
    if let Some(rest) = s.strip_prefix("linear: ") {
        let mut t = [0u64; 3];
        for (slot, (part, name)) in t.iter_mut().zip(rest.split(", ").zip(["start ", "cliff ", "end "])) {
            *slot = parse_utc(part.strip_prefix(name).ok_or_else(err)?).map_err(|_| err())?;
        }
        return Ok(Kind::Linear {
            start: t[0],
            cliff: t[1],
            end: t[2],
        });
    }
    let rest = s.strip_prefix("milestones: ").ok_or_else(err)?;
    let (mut amounts, mut signalled) = (Vec::new(), 0u64);
    for (i, part) in rest.split(", ").enumerate() {
        let mut words = part.split(' ');
        if words.next() != Some(&format!("#{i}")) {
            return Err(err());
        }
        amounts.push(words.next().and_then(|a| a.parse().ok()).ok_or_else(err)?);
        match words.next() {
            None => {}
            Some("(signalled)") if i < 64 => signalled |= 1 << i,
            Some(_) => return Err(err()),
        }
    }
    Ok(Kind::Milestones { amounts, signalled })
}

/// The SDK's `next_unlock_text()`: a UTC time, the milestone wording, or none.
fn next_unlock(s: &str) -> Result<NextUnlock, ErrorInfo> {
    if let Ok(t) = parse_utc(s) {
        return Ok(NextUnlock::At(t));
    }
    if s.starts_with("when the milestone authority signals") {
        return Ok(NextUnlock::WhenMilestoneSignalled);
    }
    if s.starts_with("none") {
        return Ok(NextUnlock::Never);
    }
    Err(bad("schedule", format!("unknown next unlock `{s}`")))
}

/// `show <schedule>`.
///
/// # Errors
/// `backend_bad_output` when the document is not a schedule.
pub fn status(doc: &Value) -> Result<ScheduleStatus, ErrorInfo> {
    const W: &str = "schedule";
    let s = field(doc, W, "schedule")?;
    let standing = match text(s, W, "standing")?.as_str() {
        "active" => Standing::Active,
        "cancelled" => Standing::Cancelled,
        "settled" => Standing::Settled,
        other => return Err(bad(W, format!("unknown standing `{other}`"))),
    };
    let cancelled = match s.get("cancelled") {
        None | Some(Value::Null) => None,
        Some(c) => Some(Cancellation {
            at: utc_field(c, W, "at")?,
            vested_at_cancel: amount(c, W, "vested_at_cancel")?,
            refunded: amount(c, W, "refunded")?,
        }),
    };
    Ok(ScheduleStatus {
        account: text(s, W, "account")?,
        creator: text(s, W, "creator")?,
        beneficiary: text(s, W, "beneficiary")?,
        asset: asset(&text(s, W, "asset")?)?,
        total: amount(s, W, "total")?,
        claimed: amount(s, W, "claimed")?,
        vested: amount(s, W, "vested")?,
        claimable: amount(s, W, "claimable_now")?,
        locked: amount(s, W, "locked")?,
        now: int(s, W, "clock_ms")?,
        next_unlock: next_unlock(&text(s, W, "next_unlock")?)?,
        standing,
        kind: kind(&text(s, W, "kind")?)?,
        cancelable: flag(s, W, "cancelable")?,
        transferable: flag(s, W, "transferable")?,
        cancel_authority: text(s, W, "cancel_authority")?,
        milestone_authority: text(s, W, "milestone_authority")?,
        refund_to: text(s, W, "refund_to")?,
        cancelled,
    })
}

fn first_tx<'a>(doc: &'a Value, what: &str) -> Result<&'a Value, ErrorInfo> {
    field(doc, what, "transactions")?
        .get(0)
        .ok_or_else(|| bad(what, "no transaction"))
}

/// `claim ... --dry-run` (or the prepared part of a sent claim).
///
/// # Errors
/// `backend_bad_output` when the document is not a claim.
pub fn prepared_claim(doc: &Value) -> Result<PreparedClaim, ErrorInfo> {
    const W: &str = "claim";
    let t = first_tx(doc, W)?;
    if t.get("operation").and_then(Value::as_str) != Some("claim") {
        return Err(bad(W, "the transaction is not a claim"));
    }
    let d = field(t, W, "destination")?;
    let destination = match text(d, W, "kind")?.as_str() {
        "public" => Destination::Public {
            account: text(d, W, "account")?,
        },
        "private" => Destination::Private {
            account: text(d, W, "account")?,
            blinded: text(d, W, "one_time_address")?,
            fresh: flag(d, W, "fresh")?,
        },
        other => return Err(bad(W, format!("unknown destination kind `{other}`"))),
    };
    let e = field(t, W, "estimate")?;
    Ok(PreparedClaim {
        schedule: text(t, W, "schedule")?,
        beneficiary: text(t, W, "beneficiary")?,
        amount: amount(t, W, "amount")?,
        at: int(t, W, "at_ms")?,
        claimed_total: amount(t, W, "claimed_after")?,
        remaining: amount(t, W, "still_to_come")?,
        destination,
        fee: amount(e, W, "network_fee")?,
        fee_payer: text(e, W, "payer")?,
        fee_payer_holds: amount(t, W, "fee_payer_holds")?,
        disclosure: t.get("privacy_disclosure").and_then(Value::as_str).map(str::to_owned),
        dev_mode: t.get("dev_mode").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// `create`, `cancel` or `signal` with `--dry-run` (or the prepared part of a sent one).
///
/// # Errors
/// `backend_bad_output` for another operation or a malformed document.
pub fn prepared_op(doc: &Value) -> Result<PreparedOp, ErrorInfo> {
    const W: &str = "operation";
    let t = first_tx(doc, W)?;
    let outcome = match t.get("operation").and_then(Value::as_str) {
        Some("create") => Outcome::Created {
            schedules: field(t, W, "schedules")?
                .as_array()
                .ok_or_else(|| bad(W, "`schedules` is not a list"))?
                .iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect(),
            total: amount(t, W, "total")?,
            protocol_fee: amount(t, W, "creation_fee")?,
            treasury: text(t, W, "treasury")?,
        },
        Some("cancel") => Outcome::Cancelled {
            at: utc_field(t, W, "at")?,
            vested_at_cancel: amount(t, W, "vested_at_cancel")?,
            refunded: amount(t, W, "refunded")?,
            refund_to: text(t, W, "refund_to")?,
            still_claimable: amount(t, W, "still_claimable")?,
        },
        Some("signal") => Outcome::Signalled {
            index: u8::try_from(int(t, W, "index")?).map_err(|_| bad(W, "`index` out of range"))?,
            amount: amount(t, W, "unlocks")?,
        },
        other => return Err(bad(W, format!("unexpected operation {other:?}"))),
    };
    let e = field(t, W, "estimate")?;
    Ok(PreparedOp {
        outcome,
        fee: amount(e, W, "network_fee")?,
        fee_payer: text(e, W, "payer")?,
    })
}

/// The first `sent` entry of a sent operation.
///
/// # Errors
/// `backend_bad_output` when nothing was reported sent.
pub fn receipt(doc: &Value) -> Result<Receipt, ErrorInfo> {
    const W: &str = "receipt";
    let r = field(doc, W, "sent")?.get(0).ok_or_else(|| bad(W, "nothing sent"))?;
    Ok(Receipt {
        hash: text(r, W, "hash")?,
        block: int(r, W, "block")?,
    })
}

/// `disclosure`: the SDK's privacy disclosure text.
///
/// # Errors
/// `backend_bad_output` without one.
pub fn disclosure(doc: &Value) -> Result<String, ErrorInfo> {
    text(doc, "disclosure", "privacy_disclosure")
}

/// `PrivateFeeShortfall` -> `private_fee_shortfall`.
fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// A refusal: `{"ok": false, "error": {"code": <program code or null>, "name": "<SDK error>", "message", "action"}}`.
/// The SDK's error name becomes the stable code in snake case (`PrivateFeeShortfall` -> `private_fee_shortfall`).
/// An older command line printed the error as one text with `What to do: <action>` in it; that is read too.
#[must_use]
pub fn error(doc: &Value) -> Option<ErrorInfo> {
    if doc.get("ok").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let err = doc.get("error")?;
    let code_of = |v: Option<&Value>| v.and_then(Value::as_u64).and_then(|c| u32::try_from(c).ok());
    if let Some(e) = err.as_object() {
        let program_code = code_of(e.get("code"));
        let name = e.get("name").and_then(Value::as_str).unwrap_or("Error");
        return Some(ErrorInfo {
            code: snake(name),
            program_code,
            message: e
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the command line refused")
                .to_owned(),
            action: e.get("action").and_then(Value::as_str).map(str::to_owned),
        });
    }
    let full = err.as_str()?.to_owned();
    let (message, action) = match full.split_once("What to do: ") {
        Some((m, a)) => (m.trim_end().trim_end_matches('.').to_owned(), Some(a.trim().to_owned())),
        None => (full.clone(), None),
    };
    let program_code = code_of(doc.get("code"));
    let code = match program_code {
        Some(14) => "nothing_claimable",
        Some(8) => "no_such_schedule",
        Some(_) => "program_refused",
        None if full.contains("cannot pay the network fee of this claim") => "private_fee_shortfall",
        None if full.contains("cannot pay this operation") => "insufficient_balance",
        None if full.contains("is not a private account of this wallet") => "not_private_account",
        None => "input",
    };
    Some(ErrorInfo {
        code: code.to_owned(),
        program_code,
        message,
        action,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::account::tests::id;
    use crate::disclosure::tests::sdk_text;
    use crate::time::utc;
    use serde_json::json;

    /// `show` as `cli/src/output.rs` prints it, for the linear sample of `model::tests`.
    pub(crate) fn show_doc() -> Value {
        json!({ "ok": true, "schedule": {
            "account": id(1), "standing": "active", "creator": id(2), "beneficiary": id(3), "asset": "native",
            "kind": format!("linear: start {}, cliff {}, end {}", utc(1_000_000), utc(1_250_000), utc(2_000_000)),
            "total": "1000", "vested": "500", "claimed": "100", "claimable_now": "400", "locked": "500",
            "next_unlock": utc(1_501_000), "escrow": "900", "cancelable": true, "transferable": false,
            "cancel_authority": id(2), "milestone_authority": id(2), "refund_to": id(2), "cancelled": null,
            "clock": utc(1_500_000), "clock_ms": 1_500_000 } })
    }

    /// `claim --private --dry-run`, for the private sample of `claim::tests`.
    pub(crate) fn claim_doc() -> Value {
        json!({ "ok": true, "note": "dry run: nothing sent", "transactions": [{
            "operation": "claim", "schedule": id(1), "beneficiary": id(3), "amount": "400", "at": utc(1_500_000),
            "at_ms": 1_500_000, "claimed_after": "500", "still_to_come": "500",
            "destination": { "kind": "private", "account": id(8), "one_time_address": id(9), "fresh": true },
            "fee_payer_holds": "400",
            "estimate": { "network_fee": "30", "reserve": "30", "payer": id(8), "private": true, "cycles": 0 },
            "privacy_disclosure": sdk_text(), "dev_mode": true }] })
    }

    #[test]
    fn reads_show_exactly() {
        assert_eq!(status(&show_doc()).unwrap(), crate::model::tests::linear_status());
        let mut m = show_doc();
        m["schedule"]["kind"] = json!("milestones: #0 100 (signalled), #1 200, #2 300");
        m["schedule"]["next_unlock"] = json!("when the milestone authority signals a milestone");
        let s = status(&m).unwrap();
        assert_eq!(
            s.kind,
            Kind::Milestones {
                amounts: vec![100, 200, 300],
                signalled: 1
            }
        );
        assert_eq!(s.next_unlock, NextUnlock::WhenMilestoneSignalled);
        m["schedule"]["next_unlock"] = json!("none (nothing more vests)");
        m["schedule"]["asset"] = json!(format!("token {}", id(6)));
        m["schedule"]["cancelled"] = json!({ "at": utc(7), "vested_at_cancel": "5", "refunded": "995" });
        let s = status(&m).unwrap();
        assert_eq!(
            (s.next_unlock, s.asset),
            (NextUnlock::Never, Asset::Token { definition: id(6) })
        );
        assert_eq!(
            s.cancelled,
            Some(Cancellation {
                at: 7,
                vested_at_cancel: 5,
                refunded: 995
            })
        );
        m["schedule"]["kind"] = json!("spiral");
        assert_eq!(status(&m).unwrap_err().code, "backend_bad_output");
    }

    #[test]
    fn u4_reads_a_dry_run_claim_with_its_fee_and_disclosure() {
        assert_eq!(
            prepared_claim(&claim_doc()).unwrap(),
            crate::claim::tests::prepared_private()
        );
        let mut public = claim_doc();
        public["transactions"][0]["destination"] = json!({ "kind": "public", "account": id(3) });
        public["transactions"][0]["estimate"]["payer"] = json!(id(3));
        public["transactions"][0]["fee_payer_holds"] = json!("50");
        public["transactions"][0]
            .as_object_mut()
            .unwrap()
            .remove("privacy_disclosure");
        public["transactions"][0]["dev_mode"] = json!(false);
        assert_eq!(prepared_claim(&public).unwrap(), crate::claim::tests::prepared_public());
    }

    #[test]
    fn reads_operations_and_receipts() {
        let est = json!({ "network_fee": "12", "reserve": "12", "payer": id(2), "private": false, "cycles": 1 });
        let create = json!({ "ok": true, "transactions": [{ "operation": "create", "schedules": [id(1)], "total": "1000",
            "creation_fee": "0", "treasury": id(2), "estimate": est, "schedule_id": "00" }],
            "sent": [{ "hash": "ab", "block": 9, "declared_fee": "12" }] });
        let p = prepared_op(&create).unwrap();
        assert_eq!(p.fee, 12);
        assert!(matches!(p.outcome, Outcome::Created { total: 1000, .. }));
        assert_eq!(
            receipt(&create).unwrap(),
            Receipt {
                hash: "ab".into(),
                block: 9
            }
        );
        let cancel = json!({ "ok": true, "transactions": [{ "operation": "cancel", "at": utc(5), "vested_at_cancel": "1",
            "refunded": "2", "refund_to": id(2), "still_claimable": "1", "estimate": est }] });
        assert!(matches!(
            prepared_op(&cancel).unwrap().outcome,
            Outcome::Cancelled { at: 5, refunded: 2, .. }
        ));
        let signal = json!({ "ok": true, "transactions": [{ "operation": "signal", "index": 2, "unlocks": "300", "estimate": est }] });
        assert_eq!(
            prepared_op(&signal).unwrap().outcome,
            Outcome::Signalled { index: 2, amount: 300 }
        );
        assert_eq!(receipt(&signal).unwrap_err().code, "backend_bad_output");
        assert_eq!(
            disclosure(&json!({ "ok": true, "privacy_disclosure": "x" })).unwrap(),
            "x"
        );
    }

    #[test]
    fn u4_reads_refusals_with_their_action() {
        // As the command line prints them now (`cli/src/output.rs`, `error_json`).
        let e = error(
            &json!({ "ok": false, "error": { "code": null, "name": "PrivateFeeShortfall",
            "message": "the private account P cannot pay the network fee of this claim", "action": "claim later" } }),
        )
        .unwrap();
        assert_eq!(e.code, "private_fee_shortfall");
        assert_eq!(e.action.as_deref(), Some("claim later"));
        let e = error(
            &json!({ "ok": false, "error": { "code": 18, "name": "MilestoneAlreadySignalled",
            "message": "this milestone was already signalled", "action": null } }),
        )
        .unwrap();
        assert_eq!(
            (e.code.as_str(), e.program_code, e.action),
            ("milestone_already_signalled", Some(18), None)
        );
        // The earlier one-text form.
        let e = error(
            &json!({ "ok": false, "code": null, "error": "the private account P cannot pay the network fee \
            of this claim: it needs 30, it will hold 10 (short by 20). What to do: claim later" }),
        )
        .unwrap();
        assert_eq!(
            (e.code.as_str(), e.action.as_deref()),
            ("private_fee_shortfall", Some("claim later"))
        );
        assert!(e.message.ends_with("(short by 20)"), "{}", e.message);
        let e =
            error(&json!({ "ok": false, "code": 14, "error": "cliff-veil error 14: nothing claimable yet" })).unwrap();
        assert_eq!((e.code.as_str(), e.program_code), ("nothing_claimable", Some(14)));
        assert_eq!(error(&json!({ "ok": true })), None);
    }
}
