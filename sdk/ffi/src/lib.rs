//! C ABI of the cliff-veil SDK, for Logos core modules (C++/Qt) and any other host that can call C.
//!
//! Every call takes JSON text (or plain strings and integers) and answers one JSON text, in the shapes the
//! `cliff-veil --json` command line prints (they come from the same code, `cliff_veil_sdk::json`):
//!
//! - success: `{"ok": true, ...}`
//! - failure: `{"ok": false, "error": {"code": N | null, "name": "...", "message": "...", "action": "..." | null}}`,
//!   `code` being the vesting program's stable error code (SPEC 5) when the program's rules refuse.
//!
//! The header is `include/cliff_veil.h`; `docs/SDK.md` describes every call and how a Logos module links the
//! library. The lifecycle is the SDK's: every operation that changes state is prepared first (checked against
//! current state, dry-run, priced; nothing is signed), then sent by id with [`abi::cv_send_prepared`]. A claim to a
//! private account is sent only with the acknowledgement token of its own privacy disclosure.
//!
//! # Threads
//!
//! A client handle may be used from any thread. Calls on one handle are serialised by a lock inside it; separate
//! handles are independent. A call blocks its thread until the SDK answers (a send waits for inclusion). Do not call
//! from a thread that is running an async (tokio) runtime, and close a handle only after every call on it returned.
//!
//! # Memory
//!
//! Every `char *` a call returns is owned by the caller and released with [`abi::cv_string_free`]. Input strings are
//! borrowed for the duration of the call only.

#[allow(unsafe_code)]
pub mod abi;
mod session;

#[cfg(test)]
mod tests;

pub use abi::CvClient;
