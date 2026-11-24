/*
 * cliff_veil.h: C ABI of the cliff-veil SDK (vesting schedules on LEZ v0.4.0-rc3).
 *
 * Library: libcliff_veil.so (shared) or libcliff_veil.a (static), built by `cargo build -p vesting_sdk_ffi`.
 * Licence: MIT OR Apache-2.0. Reference: docs/SDK.md.
 *
 * JSON in, JSON out. Every function that returns `char *` answers one JSON object, in the shapes the
 * `cliff-veil --json` command line prints:
 *   success  {"ok": true, ...}
 *   failure  {"ok": false, "error": {"code": N | null, "name": "...", "message": "...", "action": "..." | null}}
 * `code` is the vesting program's stable error code (SPEC section 5) when its rules refuse; `name` is stable.
 * Amounts are decimal strings (128-bit), times are milliseconds since the Unix epoch, accounts are base58 text or
 * labels (wallet labels on a sequencer, configured labels on the in-memory chain).
 *
 * Lifecycle: every operation that changes state is prepared first (checked against current state, dry-run and
 * priced; nothing is signed) and answers {"transactions": [{..., "prepared": <id>}]}; then cv_send_prepared sends
 * one id. A claim to a private account also answers "privacy_disclosure" and "disclosure_ack": show the disclosure to
 * the person, and pass "disclosure_ack" to cv_send_prepared only once they accept it. Without it nothing is sent.
 *
 * Memory: every returned `char *` belongs to the caller; free it with cv_string_free. Input strings are borrowed
 * for the call only. No function returns NULL except on allocation failure.
 *
 * Threads: a client may be used from any thread; calls on one client are serialised by a lock inside it. A call
 * blocks until the SDK answers (a send waits for inclusion). Do not call from a thread running an async runtime.
 * Close a client only after every call on it has returned. No panic crosses this boundary.
 */
#ifndef CLIFF_VEIL_H
#define CLIFF_VEIL_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Version of this ABI; equals cv_abi_version() of a matching library. */
#define CV_ABI_VERSION 1

/* An open client: a chain (a sequencer through LEZ's wallet, or an in-memory chain) and its prepared transactions. */
typedef struct CvClient CvClient;

/* ------------------------------------------------------------------ lifetime */

/*
 * Opens a client. config_json:
 *   {"backend": "sequencer", "ids_file": "<path>" | "vesting": "<id>" (+ "vesting_fees", "token_program",
 *    "ata_program"), "wallet_home"?: "<dir>", "wallet_password"?: "...", "fee_headroom_blocks"?: n}
 *     (defaults: LEE_WALLET_HOME_DIR else ~/.lee/wallet, and LEE_WALLET_PASSWORD; never a terminal prompt)
 *   {"backend": "memory", "now_ms"?: n, "public"?: [{"label", "balance"?, "account"?}],
 *    "private"?: [{"label", "balance"?, "account"?}], "fee_rate_bps"?: n, "treasury"?: "<label>", "vesting"?: "<id>"}
 * Answer: {"ok": true, "backend", "programs", "dev_mode", "accounts"? (memory: label -> id)}. *out_client is the
 * handle on success, NULL on failure.
 */
char *cv_client_open(const char *config_json, CvClient **out_client);

/* Closes and frees a client and every transaction still prepared on it. NULL is ignored. */
void cv_client_close(CvClient *client);

/* Frees a string returned by this library. NULL is ignored. */
void cv_string_free(char *s);

/* CV_ABI_VERSION of the library. */
uint32_t cv_abi_version(void);

/* {"ok": true, "privacy_disclosure": "...", "dev_mode": bool}. Needs no client. */
char *cv_disclosure(void);

/* ------------------------------------------------------------------ reading */

/* {"ok": true, "schedule": {account, standing, beneficiary, total, vested, claimed, claimable_now, locked,
 *  next_unlock, next_unlock_kind, next_unlock_ms, kind, kind_detail, cancelable, transferable, ..., clock_ms}} */
char *cv_show(CvClient *client, const char *schedule);

/* {"ok": true, "claimable": {"schedule", "claimable_now", "next_unlock", "clock"}} */
char *cv_claimable(CvClient *client, const char *schedule);

/* request: {"creator" | "beneficiary": "<account>", "from_block"?: n, "active"?: bool}
 * {"ok": true, "schedules": [<as cv_show>...], "scanned_to_block": n}. The block scan is cached in the client. */
char *cv_list(CvClient *client, const char *request_json);

/* {"ok": true, "fees": {"deployed", "program", "config_account", "rate_bps", "max_fee_bps", "treasury"}} */
char *cv_fee_show(CvClient *client);

/* request: {"creator", "label" | "id": "<64 hex>"}  ->  {"ok": true, "schedule": "<account>"} */
char *cv_address(CvClient *client, const char *request_json);

/* ------------------------------------------------------------------ preparing
 * Each answers {"ok": true, "transactions": [{"operation", "prepared": id, "estimate": {"network_fee", "reserve",
 * "payer", "private", "cycles", "headroom_blocks", "headroom_reserve"}, ...outcome}]}.
 */

/* {"from", "beneficiary", "total", "start"? (default now), "cliff"? (default start), "end"} or
 * {"from", "beneficiary", "milestones": [amounts]}, plus "token"?, "cancelable"? (true), "transferable"? (false),
 * "cancel_authority"?, "milestone_authority"?, "refund_to"?, "label"? | "id"?. Outcome: schedules, total,
 * creation_fee, treasury, schedule_id. */
char *cv_prepare_create(CvClient *client, const char *request_json);

/* {"from", "items": [{"beneficiary", "total", "label"?}], "start"?, "cliff"?, "end"} or {..., "milestone_bps": [..]},
 * plus the common terms of cv_prepare_create and "per_tx"?. One transaction per chunk that fits. */
char *cv_prepare_create_batch(CvClient *client, const char *request_json);

/* Public:  {"schedule", "as": "<beneficiary>", "to"?: "<public account>"}
 * Private: {"schedule", "as", "private": true, "private_to"?: "<owned private account>", "fee_payer"?: "<...>"}
 * Outcome: the pre-claim summary (amount, at, claimed_after, still_to_come, destination, fee_payer, fee_payer_holds,
 * fee_payer_choice, fee_payer_reason, reserve, headroom_*); private: privacy_disclosure, dev_mode, disclosure_ack. */
char *cv_prepare_claim(CvClient *client, const char *request_json);

/* {"schedule", "as": "<cancel authority>"}. Outcome: at, vested_at_cancel, refunded, refund_to, still_claimable. */
char *cv_prepare_cancel(CvClient *client, const char *request_json);

/* {"schedule", "as": "<creator or cancel authority>"}. */
char *cv_prepare_make_non_cancelable(CvClient *client, const char *request_json);

/* {"schedule", "as": "<milestone authority>", "index": n}. Outcome: index, unlocks. */
char *cv_prepare_signal(CvClient *client, const char *request_json);

/* {"schedule", "as": "<beneficiary>", "to": "<new beneficiary>"}. Outcome: from, to. */
char *cv_prepare_transfer(CvClient *client, const char *request_json);

/* ------------------------------------------------------------------ sending */

/* Sends prepared transaction `prepared` and waits for inclusion:
 * {"ok": true, "prepared", "sent": [{"hash", "block", "declared_fee"}], "private_balance_after"?}.
 * disclosure_ack: NULL, except for a claim to a private account, which is refused with DisclosureNotAcknowledged
 * (nothing sent, still prepared) unless it is that claim's "disclosure_ack". Any other outcome uses the id up. */
char *cv_send_prepared(CvClient *client, uint64_t prepared, const char *disclosure_ack);

/* Drops a prepared transaction unsent: {"ok": true, "prepared", "discarded": bool}. */
char *cv_discard_prepared(CvClient *client, uint64_t prepared);

/* In-memory chain only: sets the clock (ms). {"ok": true, "clock_ms", "clock"}. */
char *cv_memory_set_clock(CvClient *client, uint64_t now_ms);

#ifdef __cplusplus
}
#endif

#endif /* CLIFF_VEIL_H */
