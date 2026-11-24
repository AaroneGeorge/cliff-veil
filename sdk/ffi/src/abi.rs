//! The `extern "C"` functions (declared in `include/cliff_veil.h`). This file holds the crate's only `unsafe`: reading
//! C strings, the client handle, and giving strings back to `cv_string_free`.
//!
//! Every function catches panics, so none crosses into C: a panic becomes `{"ok": false, "error": {"name":
//! "Panic", ...}}`, and the handle it happened on refuses further calls (`ClientPoisoned`).

use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Mutex;

use serde_json::{Map, Value};

use crate::session::{AbiError, Answer, Session, parse_request};

/// Version of this C ABI. Bumped when a function or a field changes meaning.
pub const CV_ABI_VERSION: u32 = 1;

/// A client handle: a chain (a sequencer through LEZ's wallet, or an in-memory chain) and the transactions prepared
/// on it. Opaque to C.
pub struct CvClient {
    session: Mutex<Session>,
}

/// `{"ok": true, ...answer}` or `{"ok": false, "error": {...}}` as an owned C string.
fn answer_to_c(answer: Answer) -> *mut c_char {
    let mut doc = Map::new();
    match answer {
        Ok(Value::Object(fields)) => {
            doc.insert("ok".into(), Value::Bool(true));
            doc.extend(fields);
        }
        Ok(other) => {
            doc.insert("ok".into(), Value::Bool(true));
            doc.insert("value".into(), other);
        }
        Err(AbiError(error)) => {
            doc.insert("ok".into(), Value::Bool(false));
            doc.insert("error".into(), error);
        }
    }
    // serde_json escapes every control character, so the text holds no NUL.
    CString::new(Value::Object(doc).to_string()).map_or(ptr::null_mut(), CString::into_raw)
}

fn panic_error(payload: &(dyn std::any::Any + Send)) -> AbiError {
    let what = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned());
    AbiError::new(
        "Panic",
        format!("the SDK failed inside this call: {what}"),
        Some("report it with the request; close this client and open a new one"),
    )
}

/// Runs `f`, catching any panic, and returns its answer as a C string.
pub(crate) fn guard(f: impl FnOnce() -> Answer) -> *mut c_char {
    answer_to_c(catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|payload| Err(panic_error(payload.as_ref()))))
}

/// Borrows a C string argument.
///
/// # Safety
/// `p` is null or a NUL-terminated string that stays valid for the call.
unsafe fn arg<'a>(p: *const c_char, name: &str) -> Result<&'a str, AbiError> {
    if p.is_null() {
        return Err(AbiError::input(format!("argument `{name}` is null")));
    }
    // SAFETY: non-null, and the caller guarantees a NUL-terminated string valid for the call.
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|_| AbiError::input(format!("argument `{name}` is not UTF-8")))
}

/// Runs `f` on the session of `client`, under its lock.
///
/// # Safety
/// `client` is null or a handle from `cv_client_open` not yet closed.
unsafe fn with_session(client: *mut CvClient, f: impl FnOnce(&mut Session) -> Answer) -> *mut c_char {
    guard(|| {
        if client.is_null() {
            return Err(AbiError::input("argument `client` is null"));
        }
        // SAFETY: non-null, and the caller guarantees a live handle; only shared access is taken (the lock gives
        // exclusive access to the session).
        let client = unsafe { &*client };
        let mut session = client.session.lock().map_err(|_| {
            AbiError::new(
                "ClientPoisoned",
                "an earlier call on this client failed inside the SDK, so its state is not trusted",
                Some("close this client and open a new one"),
            )
        })?;
        f(&mut session)
    })
}

/// As [`with_session`], with one string argument.
///
/// # Safety
/// As [`with_session`] and [`arg`].
unsafe fn with_arg(
    client: *mut CvClient,
    p: *const c_char,
    name: &'static str,
    f: impl FnOnce(&mut Session, &str) -> Answer,
) -> *mut c_char {
    // SAFETY: forwarded from the caller.
    unsafe {
        with_session(client, |s| {
            let a = arg(p, name)?;
            f(s, a)
        })
    }
}

/// As [`with_session`], with a JSON request object.
///
/// # Safety
/// As [`with_session`] and [`arg`].
unsafe fn with_request(
    client: *mut CvClient,
    request_json: *const c_char,
    f: impl FnOnce(&mut Session, &Value) -> Answer,
) -> *mut c_char {
    // SAFETY: forwarded from the caller.
    unsafe {
        with_session(client, |s| {
            let req = parse_request(arg(request_json, "request_json")?)?;
            f(s, &req)
        })
    }
}

// ---------------------------------------------------------------- lifetime

/// Opens a client. On success `*out_client` is the handle (close it with `cv_client_close`) and the answer is
/// `{"ok": true, "backend", "programs", "dev_mode", "accounts"? }`; on failure `*out_client` is null.
///
/// # Safety
/// `config_json` is a NUL-terminated string; `out_client` is null or points to writable storage for a pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_client_open(config_json: *const c_char, out_client: *mut *mut CvClient) -> *mut c_char {
    guard(|| {
        if out_client.is_null() {
            return Err(AbiError::input("argument `out_client` is null"));
        }
        // SAFETY: non-null, writable per the contract.
        unsafe { *out_client = ptr::null_mut() };
        // SAFETY: per the contract.
        let config = unsafe { arg(config_json, "config_json") }?;
        let (session, info) = Session::open(config)?;
        let handle = Box::into_raw(Box::new(CvClient {
            session: Mutex::new(session),
        }));
        // SAFETY: as above.
        unsafe { *out_client = handle };
        Ok(info)
    })
}

/// Closes a client and frees it (and every transaction still prepared on it). Null is ignored.
///
/// # Safety
/// `client` is null or a handle from `cv_client_open`, closed once, with no call on it still running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_client_close(client: *mut CvClient) {
    if client.is_null() {
        return;
    }
    // SAFETY: the caller hands back ownership of a live handle created by `Box::into_raw`.
    let handle = unsafe { Box::from_raw(client) };
    // Dropping stops the SDK runtime; never let a panic there reach C.
    let _ = catch_unwind(AssertUnwindSafe(move || drop(handle)));
}

/// Frees a string returned by any `cv_*` function. Null is ignored.
///
/// # Safety
/// `s` is null or a string returned by this library, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_string_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: created by `CString::into_raw` in `answer_to_c`, handed back once.
        drop(unsafe { CString::from_raw(s) });
    }
}

/// The version of this C ABI ([`CV_ABI_VERSION`]).
#[unsafe(no_mangle)]
pub extern "C" fn cv_abi_version() -> u32 {
    CV_ABI_VERSION
}

/// `{"ok": true, "privacy_disclosure": "...", "dev_mode": bool}`: the disclosure shown before every claim to a
/// private account. Needs no client.
#[unsafe(no_mangle)]
pub extern "C" fn cv_disclosure() -> *mut c_char {
    guard(|| Ok(cliff_veil_sdk::json::disclosure()))
}

// ---------------------------------------------------------------- reading

/// `{"ok": true, "schedule": {...}}`: a schedule's terms and position now.
///
/// # Safety
/// `client` is a live handle; `schedule` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_show(client: *mut CvClient, schedule: *const c_char) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_arg(client, schedule, "schedule", |s, a| s.show(a)) }
}

/// `{"ok": true, "claimable": {"schedule", "claimable_now", "next_unlock", "clock"}}`.
///
/// # Safety
/// As [`cv_show`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_claimable(client: *mut CvClient, schedule: *const c_char) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_arg(client, schedule, "schedule", |s, a| s.claimable(a)) }
}

/// `{"ok": true, "schedules": [...], "scanned_to_block": n}` for `{"creator" | "beneficiary", "from_block"?,
/// "active"?}`.
///
/// # Safety
/// `client` is a live handle; `request_json` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_list(client: *mut CvClient, request_json: *const c_char) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_request(client, request_json, Session::list) }
}

/// `{"ok": true, "fees": {"deployed", "program", "config_account", "rate_bps", "max_fee_bps", "treasury"}}`.
///
/// # Safety
/// `client` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_fee_show(client: *mut CvClient) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_session(client, |s| s.fee_show()) }
}

/// `{"ok": true, "schedule": "<account>"}` for `{"creator", "label" | "id"}`.
///
/// # Safety
/// As [`cv_list`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_address(client: *mut CvClient, request_json: *const c_char) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_request(client, request_json, |s, r| s.address(r)) }
}

// ---------------------------------------------------------------- preparing

/// Prepares a creation. Answer: `{"ok": true, "transactions": [{..., "prepared": id}]}`.
///
/// # Safety
/// As [`cv_list`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_prepare_create(client: *mut CvClient, request_json: *const c_char) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_request(client, request_json, Session::prepare_create) }
}

/// Prepares a batch creation, split into transactions that fit; one `prepared` id per transaction.
///
/// # Safety
/// As [`cv_list`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_prepare_create_batch(client: *mut CvClient, request_json: *const c_char) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_request(client, request_json, Session::prepare_create_batch) }
}

/// Prepares a claim (public or private): the pre-claim summary, the dry-run fee estimate and, for a claim to a
/// private account, `privacy_disclosure`, `dev_mode` and `disclosure_ack`.
///
/// # Safety
/// As [`cv_list`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_prepare_claim(client: *mut CvClient, request_json: *const c_char) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_request(client, request_json, Session::prepare_claim) }
}

/// Prepares a cancel.
///
/// # Safety
/// As [`cv_list`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_prepare_cancel(client: *mut CvClient, request_json: *const c_char) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_request(client, request_json, Session::prepare_cancel) }
}

/// Prepares making a schedule non-cancelable.
///
/// # Safety
/// As [`cv_list`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_prepare_make_non_cancelable(
    client: *mut CvClient,
    request_json: *const c_char,
) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_request(client, request_json, Session::prepare_make_non_cancelable) }
}

/// Prepares a milestone signal.
///
/// # Safety
/// As [`cv_list`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_prepare_signal(client: *mut CvClient, request_json: *const c_char) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_request(client, request_json, Session::prepare_signal) }
}

/// Prepares a beneficiary transfer.
///
/// # Safety
/// As [`cv_list`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_prepare_transfer(client: *mut CvClient, request_json: *const c_char) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_request(client, request_json, Session::prepare_transfer) }
}

// ---------------------------------------------------------------- sending

/// Sends prepared transaction `prepared` and waits for inclusion: `{"ok": true, "prepared", "sent": [{"hash",
/// "block", "declared_fee"}], "private_balance_after"?}`. A claim to a private account is refused
/// (`DisclosureNotAcknowledged`, nothing sent, still prepared) unless `disclosure_ack` is the token its preparation
/// returned. Pass null for other transactions.
///
/// # Safety
/// `client` is a live handle; `disclosure_ack` is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_send_prepared(
    client: *mut CvClient,
    prepared: u64,
    disclosure_ack: *const c_char,
) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe {
        with_session(client, |s| {
            let ack = if disclosure_ack.is_null() {
                None
            } else {
                Some(arg(disclosure_ack, "disclosure_ack")?)
            };
            s.send(prepared, ack)
        })
    }
}

/// Drops a prepared transaction without sending it: `{"ok": true, "prepared", "discarded": bool}`.
///
/// # Safety
/// `client` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_discard_prepared(client: *mut CvClient, prepared: u64) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_session(client, |s| s.discard(prepared)) }
}

/// In-memory chains only: sets the clock (ms since the Unix epoch). `{"ok": true, "clock_ms", "clock"}`.
///
/// # Safety
/// `client` is a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cv_memory_set_clock(client: *mut CvClient, now_ms: u64) -> *mut c_char {
    // SAFETY: per the contract.
    unsafe { with_session(client, |s| s.memory_set_clock(now_ms)) }
}
