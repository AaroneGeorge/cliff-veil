//! The module's API, as the QML view calls it (`logos.callModule("cliff_veil_module", method, args)`).
//!
//! Every method takes strings and returns one JSON text: `{"ok": true, ...}` or `{"ok": false, "error": {"code",
//! "message", "action"?, "program_code"?}}` (or `"errors": [...]` for form fields). Two-step operations keep their
//! state here, not in QML: a prepared claim is a [`ClaimFlow`] (summary, disclosure, acknowledgement) and a prepared
//! creator or authority operation is its dry-run request; the view only holds their ids. The command line is run
//! with `--dry-run` to prepare and with `--yes` to send; nothing else sends.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::amount::display;
use crate::backend::{Backend, CliBackend, Mode, Request, Role};
use crate::claim::{ClaimFlow, ClaimRequest};
use crate::cli;
use crate::disclosure;
use crate::form::{self, CreateForm};
use crate::model::{Asset, ErrorInfo, ScheduleStatus};
use crate::views::{creator_rows, outcome_lines, totals, CreatorRow, PositionView};

/// Settings sent by the view: `{"cli": "<path of cliff-veil>", "env": {"NAME": "value", ...}}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    /// Path of the `cliff-veil` command line.
    #[serde(default)]
    pub cli: String,
    /// Extra environment for it.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// The mini-app state over a backend.
pub struct App<B> {
    backend: B,
    next_id: u64,
    claims: BTreeMap<u64, ClaimFlow>,
    ops: BTreeMap<u64, Request>,
}

fn fail(e: &ErrorInfo) -> String {
    json!({ "ok": false, "error": e }).to_string()
}

fn ok(mut v: Value) -> String {
    v["ok"] = Value::Bool(true);
    v.to_string()
}

fn bad_input(what: &str, e: impl std::fmt::Display) -> ErrorInfo {
    ErrorInfo::new(
        "bad_request",
        format!("the view sent a {what} that does not read: {e}"),
        None,
    )
}

fn unknown(what: &str) -> ErrorInfo {
    ErrorInfo::new(
        "unknown_flow",
        format!("no prepared {what} with this id (already confirmed, or the module restarted)"),
        Some("review it again"),
    )
}

/// Schedule accounts typed into a view: one per line, or separated by commas or spaces.
fn schedule_list(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 32 random bytes as hex, for a schedule id when the creator gives no label. Not secret: it only has to differ
/// between creations (a repeated id is refused with code 7 and nothing is lost).
fn random_id_hex() -> String {
    use std::hash::{BuildHasher as _, Hasher as _};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    (0..4u64)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(i);
            h.write_u128(nanos);
            format!("{:016x}", h.finish())
        })
        .collect()
}

impl<B: Backend> App<B> {
    /// A mini-app over `backend`.
    pub const fn new(backend: B) -> Self {
        Self {
            backend,
            next_id: 1,
            claims: BTreeMap::new(),
            ops: BTreeMap::new(),
        }
    }

    /// The backend.
    pub const fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    fn id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn status(&mut self, schedule: &str) -> Result<ScheduleStatus, ErrorInfo> {
        cli::status(&self.backend.call(&Request::Status {
            schedule: schedule.trim().to_owned(),
        })?)
    }

    /// The schedules of `account` in `role`: those typed into the view (`schedules`, checked one by one with `show`;
    /// the ones that do not belong to `account` are returned as skipped, with the reason), or, when none is typed,
    /// the command line's `list`.
    fn statuses(
        &mut self,
        role: Role,
        account: &str,
        schedules: &str,
    ) -> Result<(Vec<ScheduleStatus>, Vec<Value>), ErrorInfo> {
        let account = crate::account::parse(account).map_err(|m| ErrorInfo::new("bad_account", m, None))?;
        let typed = schedule_list(schedules);
        if typed.is_empty() {
            let doc = self.backend.call(&Request::List { role, account }).map_err(|e| {
                if e.message.contains("unrecognized subcommand") {
                    ErrorInfo::new(
                        "list_unavailable",
                        "this cliff-veil command line cannot list schedules by account yet",
                        Some("enter the schedule accounts (one per line) and load again"),
                    )
                } else {
                    e
                }
            })?;
            let list = doc
                .get("schedules")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let statuses = list
                .iter()
                .map(|s| cli::status(&json!({ "schedule": s })))
                .collect::<Result<_, _>>()?;
            return Ok((statuses, Vec::new()));
        }
        let (mut found, mut skipped, mut first_error) = (Vec::new(), Vec::new(), None);
        let count = typed.len();
        for schedule in typed {
            match self.status(&schedule) {
                Ok(s) => {
                    let mine = match role {
                        Role::Beneficiary => s.beneficiary == account,
                        Role::Creator => {
                            s.creator == account || s.cancel_authority == account || s.milestone_authority == account
                        }
                    };
                    if mine {
                        found.push(s);
                    } else {
                        let who = match role {
                            Role::Beneficiary => "its beneficiary is another account",
                            Role::Creator => "this account is not its creator, cancel authority or milestone authority",
                        };
                        skipped.push(json!({ "schedule": schedule, "reason": who }));
                    }
                }
                Err(e) => {
                    skipped.push(json!({ "schedule": schedule, "reason": e.message }));
                    first_error.get_or_insert(e);
                }
            }
        }
        // Every schedule failed to read: most likely the Settings (path, wallet, program ids), so say that once.
        match first_error {
            Some(e) if skipped.len() == count => Err(e),
            _ => Ok((found, skipped)),
        }
    }

    /// Recipient view: the schedules paying `beneficiary`, with totals per asset. `schedules`: schedule accounts to
    /// show (empty: list them with the command line).
    #[must_use]
    pub fn recipient_view(&mut self, beneficiary: &str, schedules: &str) -> String {
        match self.statuses(Role::Beneficiary, beneficiary, schedules) {
            Ok((list, skipped)) => {
                let positions: Vec<PositionView> = list.iter().map(PositionView::new).collect();
                ok(json!({ "totals": totals(&list), "positions": positions, "skipped": skipped }))
            }
            Err(e) => fail(&e),
        }
    }

    /// Recipient view of one schedule.
    #[must_use]
    pub fn position(&mut self, schedule: &str) -> String {
        match self.status(schedule) {
            Ok(s) => ok(json!({ "position": PositionView::new(&s) })),
            Err(e) => fail(&e),
        }
    }

    /// Creator view: the active schedules of `creator` (as creator or authority), with the actions it may take.
    /// `schedules` as for [`Self::recipient_view`].
    #[must_use]
    pub fn creator_view(&mut self, creator: &str, schedules: &str) -> String {
        match self.statuses(Role::Creator, creator, schedules) {
            Ok((list, skipped)) => {
                let rows: Vec<CreatorRow> = creator_rows(&list, creator.trim());
                ok(json!({ "schedules": rows, "skipped": skipped }))
            }
            Err(e) => fail(&e),
        }
    }

    /// The SDK's privacy disclosure, from the command line (also a check that the Settings work).
    #[must_use]
    pub fn disclosure(&mut self) -> String {
        match self
            .backend
            .call(&Request::Disclosure)
            .and_then(|d| cli::disclosure(&d))
        {
            Ok(text) => match disclosure::check(&text) {
                Ok(()) => ok(json!({ "text": text, "digest": disclosure::digest(&text) })),
                Err(e) => fail(&e),
            },
            Err(e) => fail(&e),
        }
    }

    /// Step 1 of a claim: checks and prices it (`claim --dry-run`) and returns the pre-claim summary and, on the
    /// private path, the disclosure with the digest its acknowledgement must carry.
    #[must_use]
    pub fn prepare_claim(&mut self, request_json: &str) -> String {
        match self.try_prepare_claim(request_json) {
            Ok(v) => ok(v),
            Err(e) => fail(&e),
        }
    }

    fn try_prepare_claim(&mut self, request_json: &str) -> Result<Value, ErrorInfo> {
        let request: ClaimRequest = serde_json::from_str(request_json).map_err(|e| bad_input("claim", e))?;
        let native = self.status(&request.schedule)?.asset == Asset::Native;
        let doc = self.backend.call(&Request::Claim {
            request: request.clone(),
            mode: Mode::DryRun,
        })?;
        let flow = ClaimFlow::new(request, cli::prepared_claim(&doc)?, native)?;
        let id = self.id();
        let v = Self::claim_json(id, &flow);
        self.claims.insert(id, flow);
        Ok(v)
    }

    fn claim_json(id: u64, flow: &ClaimFlow) -> Value {
        json!({
            "flow": id,
            "summary": flow.summary(),
            "disclosure": flow.disclosure().map(|t| json!({ "text": t, "digest": disclosure::digest(t) })),
            "stage": flow.stage(),
            "blocked": flow.blocked(),
        })
    }

    /// Records the acknowledgement of the disclosure shown for claim `flow`.
    #[must_use]
    pub fn acknowledge_disclosure(&mut self, flow: u64, digest: &str) -> String {
        let Some(f) = self.claims.get_mut(&flow) else {
            return fail(&unknown("claim"));
        };
        match f.acknowledge(digest) {
            Ok(()) => ok(Self::claim_json(flow, f)),
            Err(e) => fail(&e),
        }
    }

    /// Step 2: sends claim `flow` (`claim --yes`), if nothing blocks it: on the private path only once its
    /// disclosure is acknowledged. The flow is spent once sent.
    #[must_use]
    pub fn confirm_claim(&mut self, flow: u64) -> String {
        let Some(f) = self.claims.get_mut(&flow) else {
            return fail(&unknown("claim"));
        };
        let request = match f.confirm() {
            Ok(r) => r,
            Err(e) => return fail(&e),
        };
        self.claims.remove(&flow);
        let sent = self
            .backend
            .call(&Request::Claim {
                request,
                mode: Mode::Send,
            })
            .and_then(|d| {
                let receipt = cli::receipt(&d)?;
                Ok(json!({ "receipt": receipt, "claim": cli::prepared_claim(&d).ok() }))
            });
        match sent {
            Ok(v) => ok(v),
            Err(e) => fail(&e),
        }
    }

    /// Checks the create form without asking the chain: the review lines, or every field error.
    #[must_use]
    pub fn validate_create(&mut self, form_json: &str) -> String {
        let form: CreateForm = match serde_json::from_str(form_json) {
            Ok(f) => f,
            Err(e) => return fail(&bad_input("create form", e)),
        };
        match form::validate(&form) {
            Ok(r) => ok(json!({ "lines": r.lines() })),
            Err(errors) => json!({ "ok": false, "errors": errors }).to_string(),
        }
    }

    /// Step 1 of a creation: the form checked, then `create --dry-run` (escrow, creation fee, network fee). Without
    /// a label the mini-app picks the schedule id, so the send creates the schedule account the review showed.
    #[must_use]
    pub fn prepare_create(&mut self, form_json: &str) -> String {
        let form: CreateForm = match serde_json::from_str(form_json) {
            Ok(f) => f,
            Err(e) => return fail(&bad_input("create form", e)),
        };
        match form::validate(&form) {
            Ok(mut request) => {
                if request.label.is_none() {
                    request.id = Some(random_id_hex());
                }
                let lines = request.lines();
                self.prepare_op(
                    Request::Create {
                        request,
                        mode: Mode::DryRun,
                    },
                    json!(lines),
                )
            }
            Err(errors) => json!({ "ok": false, "errors": errors }).to_string(),
        }
    }

    /// Step 1 of a cancel by `authority`.
    #[must_use]
    pub fn prepare_cancel(&mut self, schedule: &str, authority: &str) -> String {
        let r = Request::Cancel {
            schedule: schedule.trim().into(),
            authority: authority.trim().into(),
            mode: Mode::DryRun,
        };
        self.prepare_op(r, Value::Null)
    }

    /// Step 1 of a milestone signal by `authority`.
    #[must_use]
    pub fn prepare_signal(&mut self, schedule: &str, authority: &str, index: u8) -> String {
        let r = Request::Signal {
            schedule: schedule.trim().into(),
            authority: authority.trim().into(),
            index,
            mode: Mode::DryRun,
        };
        self.prepare_op(r, Value::Null)
    }

    fn prepare_op(&mut self, request: Request, lines: Value) -> String {
        match self.backend.call(&request).and_then(|d| cli::prepared_op(&d)) {
            Ok(p) => {
                let id = self.id();
                self.ops.insert(id, request);
                ok(json!({
                    "op": id,
                    "lines": lines,
                    "outcome": p.outcome,
                    "outcome_lines": outcome_lines(&p.outcome),
                    "fee": display(p.fee),
                    "fee_payer": p.fee_payer,
                }))
            }
            Err(e) => fail(&e),
        }
    }

    /// Step 2 of a creation, cancel or signal: sends it with `--yes` (the command line checks and prices it again
    /// first).
    #[must_use]
    pub fn confirm_op(&mut self, op: u64) -> String {
        let Some(mut request) = self.ops.remove(&op) else {
            return fail(&unknown("operation"));
        };
        match &mut request {
            Request::Create { mode, .. } | Request::Cancel { mode, .. } | Request::Signal { mode, .. } => {
                *mode = Mode::Send
            }
            _ => return fail(&unknown("operation")),
        }
        let sent = self.backend.call(&request).and_then(|d| {
            let receipt = cli::receipt(&d)?;
            let outcome = cli::prepared_op(&d).ok().map(|p| outcome_lines(&p.outcome));
            Ok(json!({ "receipt": receipt, "outcome_lines": outcome }))
        });
        match sent {
            Ok(v) => ok(v),
            Err(e) => fail(&e),
        }
    }
}

impl App<CliBackend> {
    /// Sets where the `cliff-veil` command line is and its environment ([`Settings`]).
    #[must_use]
    pub fn configure(&mut self, settings_json: &str) -> String {
        match serde_json::from_str::<Settings>(settings_json) {
            Ok(s) => {
                let b = self.backend_mut();
                b.program = s.cli.trim().to_owned();
                b.env = s.env.into_iter().collect();
                ok(json!({ "cli": b.program, "env": b.env.iter().map(|(k, _)| k).collect::<Vec<_>>() }))
            }
            Err(e) => fail(&bad_input("settings", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::id;
    use crate::claim::tests::{private_request, public_request};
    use crate::claim::ClaimPath;
    use crate::cli::tests::{claim_doc, show_doc};
    use crate::form::tests::linear_form;

    /// Answers from a script and records every request.
    #[derive(Default)]
    struct Fake {
        answers: Vec<Result<Value, ErrorInfo>>,
        seen: Vec<Request>,
    }

    impl Backend for Fake {
        fn call(&mut self, request: &Request) -> Result<Value, ErrorInfo> {
            self.seen.push(request.clone());
            if self.answers.is_empty() {
                Err(ErrorInfo::new("no_answer", "script ran out", None))
            } else {
                self.answers.remove(0)
            }
        }
    }

    fn app(answers: Vec<Result<Value, ErrorInfo>>) -> App<Fake> {
        App::new(Fake {
            answers,
            seen: Vec::new(),
        })
    }

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn milestone_doc() -> Value {
        let mut m = show_doc();
        m["schedule"]["account"] = json!(id(4));
        m["schedule"]["milestone_authority"] = json!(id(5));
        m["schedule"]["kind"] = json!("milestones: #0 100 (signalled), #1 200, #2 300");
        m["schedule"]["next_unlock"] = json!("when the milestone authority signals a milestone");
        m["schedule"]["claimable_now"] = json!("0");
        m
    }

    fn sent(mut doc: Value) -> Result<Value, ErrorInfo> {
        doc["sent"] = json!([{ "hash": "ab", "block": 7, "declared_fee": "30" }]);
        Ok(doc)
    }

    fn est(payer: &str) -> Value {
        json!({ "network_fee": "12", "reserve": "12", "payer": payer, "private": false, "cycles": 1 })
    }

    #[test]
    fn u2_recipient_and_creator_views_from_typed_schedules() {
        let mut other = show_doc();
        other["schedule"]["account"] = json!(id(7));
        other["schedule"]["beneficiary"] = json!(id(9));
        let mut a = app(vec![
            Ok(show_doc()),
            Ok(milestone_doc()),
            Ok(other),
            Ok(show_doc()),
            Ok(milestone_doc()),
        ]);
        let schedules = format!("{}\n{}, {}", id(1), id(4), id(7));
        let r = v(&a.recipient_view(&id(3), &schedules));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["positions"].as_array().unwrap().len(), 2);
        assert_eq!(r["positions"][0]["claimable_now"], "400");
        assert_eq!(
            r["positions"][1]["next_unlock"],
            "when the milestone authority signals a milestone"
        );
        assert_eq!(r["skipped"][0]["schedule"], id(7));
        assert_eq!(r["totals"]["locked"][0][1], "1,000");
        assert_eq!(a.backend.seen[0], Request::Status { schedule: id(1) });
        let c = v(&a.creator_view(&id(5), &format!("{} {}", id(1), id(4))));
        // id(5) is only the milestone authority of the second schedule.
        assert_eq!(c["schedules"].as_array().unwrap().len(), 1);
        assert_eq!(c["schedules"][0]["signal_indices"], json!([1, 2]));
        assert_eq!(c["skipped"][0]["schedule"], id(1));
        assert_eq!(v(&a.recipient_view("nope", ""))["error"]["code"], "bad_account");
    }

    #[test]
    fn u2_a_view_whose_every_schedule_fails_reports_the_error() {
        let ids = ErrorInfo::new("input", "program ids: give --vesting, or an ids file with --ids", None);
        let mut a = app(vec![
            Err(ids.clone()),
            Err(ids),
            Ok(show_doc()),
            Err(ErrorInfo::new("no_such_schedule", "no schedule", None)),
        ]);
        let r = v(&a.recipient_view(&id(3), &format!("{} {}", id(1), id(4))));
        assert_eq!(r["error"]["code"], "input");
        // One readable schedule: shown, and the other listed as skipped.
        let r = v(&a.recipient_view(&id(3), &format!("{} {}", id(1), id(4))));
        assert_eq!(r["positions"].as_array().unwrap().len(), 1);
        assert_eq!(r["skipped"][0]["reason"], "no schedule");
    }

    #[test]
    fn u2_listing_without_typed_schedules_says_what_to_do() {
        let unknown_cmd = crate::cli::error(&json!({ "ok": false, "error": { "code": null, "name": "Usage",
            "message": "error: unrecognized subcommand 'list'", "action": "see `cliff-veil --help`" } }))
        .unwrap();
        let mut a = app(vec![
            Err(unknown_cmd),
            Ok(json!({ "ok": true, "schedules": [show_doc()["schedule"]] })),
        ]);
        let r = v(&a.recipient_view(&id(3), ""));
        assert_eq!(r["error"]["code"], "list_unavailable");
        assert!(r["error"]["action"]
            .as_str()
            .unwrap()
            .contains("enter the schedule accounts"));
        // Once the command line lists, the same view reads it.
        let r = v(&a.creator_view(&id(2), " "));
        assert_eq!(r["schedules"][0]["schedule"], id(1));
        assert_eq!(
            a.backend.seen[1],
            Request::List {
                role: Role::Creator,
                account: id(2)
            }
        );
    }

    #[test]
    fn u5_private_claim_is_not_sent_before_the_ack() {
        let mut a = app(vec![Ok(show_doc()), Ok(claim_doc()), sent(claim_doc())]);
        let p = v(&a.prepare_claim(&serde_json::to_string(&private_request()).unwrap()));
        assert_eq!(p["ok"], true, "{p}");
        assert_eq!(p["stage"], "review");
        assert_eq!(p["blocked"]["code"], "disclosure_not_acknowledged");
        let flow = p["flow"].as_u64().unwrap();
        assert!(p["disclosure"]["text"].as_str().unwrap().contains("Visible on-chain"));
        assert_eq!(
            a.backend.seen[1],
            Request::Claim {
                request: private_request(),
                mode: Mode::DryRun
            }
        );

        // Confirm before the ack: refused, and nothing reaches the command line.
        assert_eq!(
            v(&a.confirm_claim(flow))["error"]["code"],
            "disclosure_not_acknowledged"
        );
        assert_eq!(a.backend.seen.len(), 2, "show and the dry run only");

        assert_eq!(
            v(&a.acknowledge_disclosure(flow, "0000"))["error"]["code"],
            "disclosure_changed"
        );
        let ack = v(&a.acknowledge_disclosure(flow, p["disclosure"]["digest"].as_str().unwrap()));
        assert_eq!(ack["stage"], "ready");
        let c = v(&a.confirm_claim(flow));
        assert_eq!(c["receipt"]["block"], 7, "{c}");
        // `--yes` only now, to the private account the summary named.
        let Request::Claim { request, mode } = &a.backend.seen[2] else {
            panic!()
        };
        assert_eq!(*mode, Mode::Send);
        assert_eq!(
            request.path,
            ClaimPath::Private {
                account: Some(id(8)),
                fee_payer: None
            }
        );
        assert_eq!(v(&a.confirm_claim(flow))["error"]["code"], "unknown_flow");
    }

    #[test]
    fn u4_prepare_claim_returns_the_summary_and_gas_check() {
        let mut short = claim_doc();
        short["transactions"][0]["fee_payer_holds"] = json!("3");
        let mut a = app(vec![Ok(show_doc()), Ok(short)]);
        let p = v(&a.prepare_claim(&serde_json::to_string(&private_request()).unwrap()));
        assert_eq!(p["summary"]["fee"], "30");
        assert_eq!(p["summary"]["gas"]["status"], "insufficient");
        assert!(p["summary"]["gas"]["action"]
            .as_str()
            .unwrap()
            .contains("funded private account"));
        assert_eq!(p["blocked"]["code"], "private_fee_shortfall");
    }

    #[test]
    fn u4_cli_refusal_reaches_the_view_with_its_action() {
        let refusal = crate::cli::error(
            &json!({ "ok": false, "error": { "code": null, "name": "PrivateFeeShortfall",
            "message": "the private account P cannot pay the network fee of this claim: it needs 30, it will hold 0",
            "action": "name a funded private account as fee payer" } }),
        )
        .unwrap();
        let mut a = app(vec![Ok(show_doc()), Err(refusal)]);
        let p = v(&a.prepare_claim(&serde_json::to_string(&private_request()).unwrap()));
        assert_eq!(p["ok"], false);
        assert_eq!(p["error"]["code"], "private_fee_shortfall");
        assert_eq!(p["error"]["action"], "name a funded private account as fee payer");
    }

    #[test]
    fn u5_public_claim_is_sent_without_a_disclosure() {
        let mut public = claim_doc();
        public["transactions"][0]["destination"] = json!({ "kind": "public", "account": id(3) });
        public["transactions"][0]["estimate"]["payer"] = json!(id(3));
        public["transactions"][0]["fee_payer_holds"] = json!("50");
        public["transactions"][0]
            .as_object_mut()
            .unwrap()
            .remove("privacy_disclosure");
        let mut a = app(vec![Ok(show_doc()), Ok(public.clone()), sent(public)]);
        let p = v(&a.prepare_claim(&serde_json::to_string(&public_request()).unwrap()));
        assert_eq!(p["disclosure"], Value::Null);
        assert_eq!(p["stage"], "ready");
        assert_eq!(v(&a.confirm_claim(p["flow"].as_u64().unwrap()))["ok"], true);
        assert_eq!(
            a.backend.seen[2],
            Request::Claim {
                request: public_request(),
                mode: Mode::Send
            }
        );
    }

    #[test]
    fn u2_create_cancel_and_signal_are_prepared_then_confirmed() {
        let created = json!({ "ok": true, "transactions": [{ "operation": "create", "schedules": [id(1)], "total": "1000",
            "creation_fee": "0", "treasury": id(2), "estimate": est(&id(2)) }] });
        let cancelled = json!({ "ok": true, "transactions": [{ "operation": "cancel", "at": "1970-01-01T00:00:00.005Z",
            "vested_at_cancel": "500", "refunded": "500", "refund_to": id(2), "still_claimable": "400", "estimate": est(&id(2)) }] });
        let signalled = json!({ "ok": true, "transactions": [{ "operation": "signal", "index": 1, "unlocks": "200",
            "estimate": est(&id(5)) }] });
        let mut a = app(vec![
            Ok(created.clone()),
            sent(created),
            Ok(cancelled),
            Ok(signalled.clone()),
            sent(signalled),
        ]);

        let p = v(&a.prepare_create(&serde_json::to_string(&linear_form()).unwrap()));
        assert_eq!(p["fee"], "12");
        assert_eq!(p["outcome_lines"][0], json!(["Schedules created", id(1)]));
        let id_line = p["lines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l[0] == "Schedule id")
            .unwrap()
            .clone();
        assert!(id_line[1].as_str().unwrap().starts_with("random: "), "{id_line}");
        let c = v(&a.confirm_op(p["op"].as_u64().unwrap()));
        assert_eq!(c["receipt"]["block"], 7, "{c}");
        // No label: the dry run and the send carry the same schedule id.
        let (
            Request::Create {
                request: dry,
                mode: Mode::DryRun,
            },
            Request::Create {
                request: send,
                mode: Mode::Send,
            },
        ) = (&a.backend.seen[0], &a.backend.seen[1])
        else {
            panic!("{:?}", a.backend.seen)
        };
        assert_eq!(dry.id.as_ref().map(String::len), Some(64));
        assert_eq!(dry.id, send.id);

        let c = v(&a.prepare_cancel(&id(1), &id(2)));
        assert_eq!(c["outcome"]["cancelled"]["refunded"], "500");
        assert!(c["outcome_lines"]
            .as_array()
            .unwrap()
            .contains(&json!(["Stays claimable by the beneficiary", "400"])));
        let s = v(&a.prepare_signal(&id(4), &id(5), 1));
        assert_eq!(s["outcome"]["signalled"]["amount"], "200");
        assert_eq!(v(&a.confirm_op(s["op"].as_u64().unwrap()))["ok"], true);
        assert!(matches!(
            a.backend.seen[4],
            Request::Signal {
                mode: Mode::Send,
                index: 1,
                ..
            }
        ));
        // The cancel was never confirmed: nothing was sent for it.
        assert!(!a
            .backend
            .seen
            .iter()
            .any(|r| matches!(r, Request::Cancel { mode: Mode::Send, .. })));
    }

    #[test]
    fn u2_create_form_errors_do_not_reach_the_backend() {
        let mut a = app(vec![]);
        let bad = CreateForm {
            total: "0".into(),
            ..linear_form()
        };
        let r = v(&a.prepare_create(&serde_json::to_string(&bad).unwrap()));
        assert_eq!(r["errors"][0]["program_code"], 10);
        assert!(a.backend.seen.is_empty());
        assert_eq!(v(&a.validate_create("{}"))["ok"], false);
        assert_eq!(
            v(&a.validate_create(&serde_json::to_string(&linear_form()).unwrap()))["ok"],
            true
        );
    }

    #[test]
    fn pr2_disclosure_from_the_command_line_is_checked() {
        let text = crate::disclosure::tests::sdk_text();
        let mut a = app(vec![
            Ok(json!({ "ok": true, "privacy_disclosure": text })),
            Ok(json!({ "ok": true, "privacy_disclosure": "Visible on-chain: nothing" })),
        ]);
        let d = v(&a.disclosure());
        assert_eq!(d["digest"], crate::disclosure::digest(&text));
        assert_eq!(v(&a.disclosure())["error"]["code"], "disclosure_incomplete");
    }

    #[test]
    fn random_schedule_ids_are_64_hex_and_differ() {
        let (x, y) = (random_id_hex(), random_id_hex());
        assert_eq!(x.len(), 64);
        assert!(x.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(x, y);
    }
}
