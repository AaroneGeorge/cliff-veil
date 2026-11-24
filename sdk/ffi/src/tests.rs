//! Tests of the C ABI, calling the `extern "C"` functions as a C host does, against the in-memory chain. Names start
//! with `u1_c_abi_` (RFP Usability 1: an SDK Logos modules can use).

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char};
use std::ptr;

use serde_json::{Value, json};

use crate::abi::*;
use crate::session::memory_account;

/// 2026-01-01T00:00:00Z in ms.
const T0: u64 = 1_767_225_600_000;
const DAY: u64 = 86_400_000;

/// Reads and frees an answer.
fn take(p: *mut c_char) -> Value {
    assert!(!p.is_null(), "every call answers");
    // SAFETY: a string returned by the library, freed once below.
    let text = unsafe { CStr::from_ptr(p) }.to_str().expect("UTF-8").to_owned();
    // SAFETY: as above.
    unsafe { cv_string_free(p) };
    serde_json::from_str(&text).expect("every answer is JSON")
}

fn c(text: &str) -> CString {
    CString::new(text).expect("no NUL")
}

struct Handle(*mut CvClient);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a live handle, closed once.
        unsafe { cv_client_close(self.0) };
    }
}

// SAFETY (for the threads test): the handle serialises calls behind its lock (lib.rs, "Threads").
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    fn open(config: Value) -> (Self, Value) {
        let mut client = ptr::null_mut();
        // SAFETY: valid string and out pointer.
        let info = take(unsafe { cv_client_open(c(&config.to_string()).as_ptr(), &mut client) });
        assert_eq!(info["ok"], true, "{info}");
        assert!(!client.is_null());
        (Self(client), info)
    }

    fn call(&self, f: unsafe extern "C" fn(*mut CvClient, *const c_char) -> *mut c_char, request: Value) -> Value {
        let text = match request {
            Value::String(s) => s,
            other => other.to_string(),
        };
        // SAFETY: live handle, valid string.
        take(unsafe { f(self.0, c(&text).as_ptr()) })
    }

    fn send(&self, prepared: &Value, ack: Option<&str>) -> Value {
        let id = prepared["prepared"].as_u64().expect("a prepared id");
        let ack = ack.map(c);
        // SAFETY: live handle; ack null or valid.
        take(unsafe { cv_send_prepared(self.0, id, ack.as_ref().map_or(ptr::null(), |a| a.as_ptr())) })
    }

    fn set_clock(&self, ms: u64) {
        // SAFETY: live handle.
        let v = take(unsafe { cv_memory_set_clock(self.0, ms) });
        assert_eq!(v["ok"], true, "{v}");
    }

    /// Prepares with `f` and expects exactly one transaction.
    fn prepare_one(
        &self,
        f: unsafe extern "C" fn(*mut CvClient, *const c_char) -> *mut c_char,
        request: Value,
    ) -> Value {
        let v = self.call(f, request);
        assert_eq!(v["ok"], true, "{v}");
        let txs = v["transactions"].as_array().expect("transactions");
        assert_eq!(txs.len(), 1, "{v}");
        txs[0].clone()
    }

    fn show(&self, schedule: &str) -> Value {
        let v = self.call(cv_show, Value::String(schedule.to_owned()));
        assert_eq!(v["ok"], true, "{v}");
        v["schedule"].clone()
    }
}

fn memory() -> (Handle, Value) {
    Handle::open(json!({
        "backend": "memory",
        "now_ms": T0,
        "public": [
            {"label": "creator", "balance": "1000000"},
            {"label": "alice", "balance": 1000},
            {"label": "bob", "balance": 1000},
        ],
        "private": [{"label": "vault", "balance": "0"}],
    }))
}

/// A native schedule of 100 000 from creator to alice: start T0, cliff T0 + 10 days, end T0 + 100 days.
fn create_linear(h: &Handle, label: &str, extra: Value) -> String {
    let mut req = json!({"from": "creator", "beneficiary": "alice", "total": "100000", "start": T0,
                         "cliff": (T0 + 10 * DAY).to_string(), "end": T0 + 100 * DAY, "label": label});
    if let (Value::Object(r), Value::Object(e)) = (&mut req, extra) {
        r.extend(e);
    }
    let tx = h.prepare_one(cv_prepare_create, req);
    assert_eq!(tx["operation"], "create");
    let sent = h.send(&tx, None);
    assert_eq!(sent["ok"], true, "{sent}");
    tx["schedules"][0].as_str().expect("schedule account").to_owned()
}

#[test]
fn u1_c_abi_full_lifecycle_with_public_and_private_claims() {
    let (h, info) = memory();
    assert_eq!(info["backend"], "memory");
    assert_eq!(info["accounts"]["alice"], memory_account("alice").to_string());
    let schedule = create_linear(&h, "grant-1", json!({}));

    // Query: nothing before the cliff; the cliff lump and linear after it.
    let s = h.show(&schedule);
    assert_eq!(
        (s["total"].as_str(), s["claimable_now"].as_str()),
        (Some("100000"), Some("0"))
    );
    assert_eq!(s["next_unlock_ms"], T0 + 10 * DAY);
    h.set_clock(T0 + 50 * DAY);
    let v = h.call(cv_claimable, Value::String(schedule.clone()));
    assert_eq!(v["claimable"]["claimable_now"], "50000", "{v}");

    // Public claim: summary, estimate, sent.
    let claim = h.prepare_one(cv_prepare_claim, json!({"schedule": schedule, "as": "alice"}));
    assert_eq!(claim["amount"], "50000");
    assert_eq!(claim["destination"]["kind"], "public");
    assert!(claim["estimate"]["network_fee"].is_string());
    assert!(claim.get("disclosure_ack").is_none() && claim.get("privacy_disclosure").is_none());
    let sent = h.send(&claim, None);
    assert_eq!(sent["ok"], true, "{sent}");
    assert!(
        sent["sent"][0]["block"].is_u64() && sent["sent"][0]["hash"].is_string(),
        "{sent}"
    );

    // Private claim: the disclosure and its acknowledgement token come with the summary.
    h.set_clock(T0 + 60 * DAY);
    let private = h.prepare_one(
        cv_prepare_claim,
        json!({"schedule": schedule, "as": "alice", "private": true, "private_to": "vault"}),
    );
    assert_eq!(private["amount"], "10000");
    assert_eq!(private["destination"]["kind"], "private");
    assert_eq!(private["destination"]["account"], memory_account("vault").to_string());
    assert!(
        private["privacy_disclosure"]
            .as_str()
            .is_some_and(|d| d.contains("the destination private account"))
    );
    let ack = private["disclosure_ack"].as_str().expect("a token").to_owned();
    assert_eq!(ack.len(), 64);

    // Without the acknowledgement nothing is sent, and the claim stays prepared.
    let refused = h.send(&private, None);
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["name"], "DisclosureNotAcknowledged");
    let wrong = h.send(&private, Some(&"0".repeat(64)));
    assert_eq!(wrong["error"]["name"], "DisclosureNotAcknowledged");
    assert_eq!(h.show(&schedule)["claimed"], "50000", "nothing was claimed");

    let sent = h.send(&private, Some(&ack));
    assert_eq!(sent["ok"], true, "{sent}");
    assert_eq!(
        sent["private_balance_after"]["account"],
        memory_account("vault").to_string()
    );
    assert!(sent["private_balance_after"]["native"].is_string());
    assert_eq!(h.show(&schedule)["claimed"], "60000");

    // Cancel: the unvested part is refunded, the vested part stays claimable.
    h.set_clock(T0 + 70 * DAY);
    let cancel = h.prepare_one(cv_prepare_cancel, json!({"schedule": schedule, "as": "creator"}));
    assert_eq!(
        (cancel["refunded"].as_str(), cancel["still_claimable"].as_str()),
        (Some("30000"), Some("10000"))
    );
    assert_eq!(h.send(&cancel, None)["ok"], true);
    let s = h.show(&schedule);
    assert_eq!(
        (s["standing"].as_str(), s["claimable_now"].as_str()),
        (Some("cancelled"), Some("10000"))
    );
}

#[test]
fn u1_c_abi_milestones_signal_transfer_and_make_non_cancelable() {
    let (h, _) = memory();
    let tx = h.prepare_one(
        cv_prepare_create,
        json!({"from": "creator", "beneficiary": "alice", "milestones": ["400", 600], "transferable": true,
               "label": "m"}),
    );
    assert_eq!(h.send(&tx, None)["ok"], true);
    let schedule = tx["schedules"][0].as_str().unwrap().to_owned();
    assert_eq!(h.show(&schedule)["next_unlock_kind"], "milestone");

    let signal = h.prepare_one(
        cv_prepare_signal,
        json!({"schedule": schedule, "as": "creator", "index": 1}),
    );
    assert_eq!(signal["unlocks"], "600");
    assert_eq!(h.send(&signal, None)["ok"], true);
    assert_eq!(h.show(&schedule)["claimable_now"], "600");

    let transfer = h.prepare_one(
        cv_prepare_transfer,
        json!({"schedule": schedule, "as": "alice", "to": "bob"}),
    );
    assert_eq!(transfer["to"], memory_account("bob").to_string());
    assert_eq!(h.send(&transfer, None)["ok"], true);
    let claim = h.prepare_one(cv_prepare_claim, json!({"schedule": schedule, "as": "bob"}));
    assert_eq!(claim["amount"], "600");

    let mnc = h.prepare_one(
        cv_prepare_make_non_cancelable,
        json!({"schedule": schedule, "as": "creator"}),
    );
    assert_eq!(h.send(&mnc, None)["ok"], true);
    assert_eq!(h.show(&schedule)["cancelable"], false);
    let again = h.call(cv_prepare_cancel, json!({"schedule": schedule, "as": "creator"}));
    assert_eq!(
        (again["error"]["code"].as_u64(), again["error"]["name"].as_str()),
        (Some(15), Some("NotCancelable"))
    );
}

#[test]
fn u1_c_abi_batch_list_address_and_fees() {
    let (h, _) = Handle::open(json!({
        "backend": "memory", "now_ms": T0, "fee_rate_bps": 50, "treasury": "treasury",
        "public": [{"label": "creator", "balance": "10000000"}, {"label": "alice"}, {"label": "bob"},
                   {"label": "treasury"}],
    }));
    let address = h.call(cv_address, json!({"creator": "creator", "label": "b-0"}));
    let first = address["schedule"].as_str().expect("an address").to_owned();
    // SAFETY: live handle.
    let f = take(unsafe { cv_fee_show(h.0) });
    assert_eq!(
        (f["fees"]["rate_bps"].as_u64(), f["fees"]["deployed"].as_bool()),
        (Some(50), Some(true))
    );

    let v = h.call(
        cv_prepare_create_batch,
        json!({"from": "creator", "start": "now", "end": T0 + DAY, "per_tx": 2,
               "items": [{"beneficiary": "alice", "total": 1000, "label": "b-0"},
                         {"beneficiary": "bob", "total": "2000", "label": "b-1"},
                         {"beneficiary": "alice", "total": 3000, "label": "b-2"}]}),
    );
    assert_eq!(v["ok"], true, "{v}");
    let txs = v["transactions"].as_array().unwrap();
    assert_eq!(txs.len(), 2, "three items at two per transaction");
    assert_eq!(
        txs[0]["schedules"][0],
        first.as_str(),
        "the address call names the same account"
    );
    assert_eq!(txs[0]["creation_fee"], "15", "0.5% of 3000");
    for tx in txs {
        assert_eq!(h.send(tx, None)["ok"], true);
    }
    let listed = h.call(cv_list, json!({"beneficiary": "alice"}));
    assert_eq!(listed["schedules"].as_array().map(Vec::len), Some(2), "{listed}");
    let listed = h.call(cv_list, json!({"creator": "creator", "active": true}));
    assert_eq!(listed["schedules"].as_array().map(Vec::len), Some(3));
}

#[test]
fn u1_c_abi_errors_carry_code_name_message_and_action() {
    let (h, _) = memory();
    let schedule = create_linear(&h, "e", json!({}));

    // A program rule: not the beneficiary (3); nothing claimable before the cliff (14), with the next unlock.
    let e = h.call(cv_prepare_claim, json!({"schedule": schedule, "as": "bob"}));
    assert_eq!(e["ok"], false);
    assert_eq!(
        (e["error"]["code"].as_u64(), e["error"]["name"].as_str()),
        (Some(3), Some("NotBeneficiary"))
    );
    assert!(
        e["error"]["message"].is_string() && e["error"]["action"].is_string(),
        "{e}"
    );
    let e = h.call(cv_prepare_claim, json!({"schedule": schedule, "as": "alice"}));
    assert_eq!(e["error"]["code"], 14);
    assert!(
        e["error"]["message"]
            .as_str()
            .unwrap()
            .contains("2026-01-11T00:00:00.000Z"),
        "{e}"
    );

    // A private claim to a public account (Privacy 3), and a fee payer that cannot pay.
    h.set_clock(T0 + 50 * DAY);
    let e = h.call(
        cv_prepare_claim,
        json!({"schedule": schedule, "as": "alice", "private": true, "private_to": "bob"}),
    );
    assert_eq!(e["error"]["name"], "NotPrivateAccount");
    let e = h.call(
        cv_prepare_create,
        json!({"from": "alice", "beneficiary": "bob", "total": 5000, "end": T0 + 60 * DAY}),
    );
    assert_eq!(e["error"]["name"], "InsufficientBalance");
    assert_eq!(
        (e["error"]["need"].as_str(), e["error"]["have"].as_str()),
        (Some("5000"), Some("1000"))
    );
    assert_eq!(e["error"]["short"], "4000");

    // Bad requests: JSON, fields, labels, null pointers.
    for (req, needle) in [
        ("not json", "not valid JSON"),
        ("[1]", "JSON object"),
        (r#"{"as": "alice"}"#, "`schedule` is required"),
        (r#"{"schedule": "nobody", "as": "alice"}"#, "neither a configured label"),
        (r#"{"schedule": 7, "as": "alice"}"#, "must be a string"),
    ] {
        let e = h.call(cv_prepare_claim, Value::String(req.to_owned()));
        assert_eq!(e["error"]["name"], "Input", "{req}");
        assert!(e["error"]["code"].is_null());
        assert!(e["error"]["message"].as_str().unwrap().contains(needle), "{req}: {e}");
    }
    // SAFETY: null arguments are refused, not dereferenced.
    let e = take(unsafe { cv_show(ptr::null_mut(), c("x").as_ptr()) });
    assert_eq!(e["error"]["message"], "argument `client` is null");
    // SAFETY: as above.
    let e = take(unsafe { cv_show(h.0, ptr::null()) });
    assert_eq!(e["error"]["message"], "argument `schedule` is null");
    let mut out = ptr::null_mut();
    // SAFETY: as above.
    let e = take(unsafe { cv_client_open(c(r#"{"backend": "chain"}"#).as_ptr(), &mut out) });
    assert!(out.is_null() && e["error"]["message"].as_str().unwrap().contains("unknown backend"));
    // SAFETY: as above.
    let e = take(unsafe { cv_client_open(c(r#"{"backend": "sequencer"}"#).as_ptr(), &mut out) });
    assert!(
        out.is_null() && e["error"]["message"].as_str().unwrap().contains("ids_file"),
        "{e}"
    );
}

#[test]
fn u1_c_abi_prepared_transactions_are_used_once() {
    let (h, _) = memory();
    let tx = h.prepare_one(
        cv_prepare_create,
        json!({"from": "creator", "beneficiary": "alice", "total": 10, "end": T0 + 1, "label": "once"}),
    );
    assert_eq!(h.send(&tx, None)["ok"], true);
    let again = h.send(&tx, None);
    assert_eq!(again["error"]["name"], "UnknownPrepared");

    // Discarded: never sent. A refused send (the schedule exists now) also uses the transaction up.
    let tx = h.prepare_one(
        cv_prepare_create,
        json!({"from": "creator", "beneficiary": "alice", "total": 10, "end": T0 + 1, "label": "dropped"}),
    );
    let id = tx["prepared"].as_u64().unwrap();
    // SAFETY: live handle.
    let d = take(unsafe { cv_discard_prepared(h.0, id) });
    assert_eq!(d["discarded"], true);
    assert_eq!(h.send(&tx, None)["error"]["name"], "UnknownPrepared");
    let s = h.call(cv_address, json!({"creator": "creator", "label": "dropped"}));
    let e = h.call(cv_show, s["schedule"].clone());
    assert_eq!(e["error"]["code"], 8, "the discarded creation was never sent");

    // Each private claim gets its own token: one claim's acknowledgement does not send another.
    let schedule = create_linear(&h, "two", json!({}));
    h.set_clock(T0 + 50 * DAY);
    let req = json!({"schedule": schedule, "as": "alice", "private": true, "private_to": "vault"});
    let a = h.prepare_one(cv_prepare_claim, req.clone());
    let b = h.prepare_one(cv_prepare_claim, req);
    assert_ne!(a["disclosure_ack"], b["disclosure_ack"]);
    let e = h.send(&b, a["disclosure_ack"].as_str());
    assert_eq!(e["error"]["name"], "DisclosureNotAcknowledged");
}

#[test]
fn u1_c_abi_disclosure_and_version_need_no_client() {
    let d = take(cv_disclosure());
    assert_eq!(d["privacy_disclosure"], cliff_veil_sdk::privacy_disclosure());
    assert!(d["dev_mode"].is_boolean());
    assert_eq!(cv_abi_version(), CV_ABI_VERSION);
}

#[test]
fn u1_c_abi_a_panic_becomes_an_error_and_never_unwinds_into_c() {
    let v = take(crate::abi::guard(|| panic!("boom")));
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["name"], "Panic");
    assert!(v["error"]["message"].as_str().unwrap().contains("boom"));
}

#[test]
fn u1_c_abi_one_handle_serves_many_threads() {
    let (h, _) = memory();
    let schedule = create_linear(&h, "threads", json!({}));
    let h = &h;
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let schedule = schedule.clone();
                scope.spawn(move || {
                    for _ in 0..20 {
                        assert_eq!(h.show(&schedule)["total"], "100000");
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().expect("no thread panics");
        }
    });
}

/// Every `extern "C"` function of `src/abi.rs` is declared in `include/cliff_veil.h`, and the header declares no
/// function the library does not export.
#[test]
fn u1_c_abi_header_declares_every_exported_function() {
    let source = include_str!("abi.rs");
    let header = include_str!("../include/cliff_veil.h");
    let exported: Vec<&str> = source
        .lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix("pub unsafe extern \"C\" fn ")
                .or_else(|| l.trim().strip_prefix("pub extern \"C\" fn "))
        })
        .map(|rest| &rest[..rest.find('(').expect("a signature")])
        .collect();
    assert!(exported.len() >= 20, "found {exported:?}");
    let declares = |name: &str| {
        header.lines().any(|l| {
            let l = l.trim_start();
            !l.starts_with("/*") && !l.starts_with('*') && l.contains(&format!("{name}(")) && l.ends_with(");")
        })
    };
    for name in &exported {
        assert!(declares(name), "`{name}` is not declared in the header");
    }
    let declared: Vec<&str> = header
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with("cv_"))
        .collect();
    for name in declared {
        assert!(
            exported.contains(&name),
            "the header names `{name}`, which the library does not export"
        );
    }
    assert!(header.contains(&format!("#define CV_ABI_VERSION {CV_ABI_VERSION}")));
}

/// The same calls against a running sequencer, read-only: opens LEZ's wallet, reads the fee configuration, prepares
/// a creation (checked and priced by a dry run on the sequencer, never sent) and discards it.
/// `CV_ABI_WALLET_HOME=<wallet home> CV_ABI_IDS=<ids file> cargo test -p vesting_sdk_ffi -- --ignored`.
#[test]
#[ignore = "needs a sequencer: CV_ABI_WALLET_HOME (wallet home with a funded `cv-payer`) and CV_ABI_IDS"]
fn u1_c_abi_dry_runs_against_a_sequencer() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let (h, info) = Handle::open(json!({
        "backend": "sequencer", "wallet_home": var("CV_ABI_WALLET_HOME"), "ids_file": var("CV_ABI_IDS"),
    }));
    assert_eq!(info["backend"], "sequencer");
    println!("open: {info}");
    // SAFETY: live handle.
    let fees = take(unsafe { cv_fee_show(h.0) });
    assert_eq!(fees["ok"], true, "{fees}");
    println!("fees: {fees}");

    let label = format!(
        "c-abi-dry-run-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let tx = h.prepare_one(
        cv_prepare_create,
        json!({"from": "cv-payer", "beneficiary": "cv-payer", "total": "1000", "start": "now",
               "end": "4102444800000", "label": label}),
    );
    assert!(tx["estimate"]["network_fee"].as_str().is_some_and(|f| f != "0"), "{tx}");
    println!("prepared create (dry run, not sent): {tx}");
    let id = tx["prepared"].as_u64().unwrap();
    // SAFETY: live handle.
    assert_eq!(take(unsafe { cv_discard_prepared(h.0, id) })["discarded"], true);
    let schedule = tx["schedules"][0].as_str().unwrap();
    let missing = h.call(cv_show, Value::String(schedule.to_owned()));
    assert_eq!(missing["error"]["code"], 8, "never sent: {missing}");
    println!("show (not created): {missing}");
}
