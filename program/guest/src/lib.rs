//! The `vesting` program (SPEC.md): schedules, escrow and claims as a LEZ actor program.
//!
//! Every rule (validation, arithmetic, roles, error codes, events, fees) lives in `vesting_core`. This crate only
//! routes messages between actors, moves assets and emits events, so the program and the SDK can never disagree on
//! an amount.
//!
//! # Actors (SPEC 1.2)
//!
//! - **Caller actor** `(caller, vesting)`: always empty. A transaction sends every [`Message`] here; it requires
//!   `is_authorized` and forwards an [`Internal`] message to the schedule, or, for a creation, opens and funds new
//!   schedules.
//! - **Schedule actor** `(schedule, vesting)`, a PDA of `vesting` with the schedule seed: holds the
//!   [`ScheduleState`] and accepts [`Internal`] messages only from `(role holder, vesting)`.
//! - **Escrow**: the schedule account's native balance, or its holding under the token program. Only this program
//!   can debit it, by attaching the schedule seed.
//!
//! # Dispatch (SPEC 1.4)
//!
//! | `input.from` | Decoded as | Handler |
//! |---|---|---|
//! | the clock actor | `clock_core::Reply` | [`on_clock_reply`] |
//! | an actor of this program | [`Internal`] | [`on_internal`] |
//! | anything else | [`Message`] | [`on_message`] |
//!
//! Every refusal panics with `cliff-veil error <code>: <message>` ([`VestingError`]'s `Display`), which fails the
//! whole transaction.
//!
//! # Build inputs (SPEC 1.1)
//!
//! The ids of `vesting_fees`, the token program and the ATA program are fixed at build time from the environment
//! variables `CV_FEES_PROGRAM_ID`, `CV_TOKEN_PROGRAM_ID` and `CV_ATA_PROGRAM_ID` (base58, as LEZ prints account ids).
//! When a variable is unset the id is the matching `LOCAL_*` constant: the ids the executor tests deploy the programs
//! at. A deployment to a network builds with all three set to the ids recorded in `deploy/<network>.toml`.

use borsh::BorshDeserialize as _;
use clock_core::{Message as ClockMessage, Reply, clock_actor};
use lee_core::account::{AccountId, Actor};
use lee_core::native_token;
use lee_core::program::{PdaSeed, ProgramEvent, ReceiveInput, Response, SyncMessage};
use risc0_zkvm::sha::{Impl, Sha256 as _};
use token_core::Message as TokenMessage;
use vesting_core::{
    Asset, BeneficiaryTransferred, Cancelled, ClaimOutcome, Event, FeeMessage, Internal, MadeNonCancelable, Message,
    Pending, ScheduleCreated, ScheduleState, ScheduleV1, VestingError, ata_seed, check_destination, check_fee_cap,
    schedule_seed_preimage, validate_batch,
};

/// `vesting_fees` id used when `CV_FEES_PROGRAM_ID` is unset (local executor tests only).
pub const LOCAL_FEES_PROGRAM_ID: AccountId = AccountId::new(*b"cliff-veil/test/vesting_fees/000");
/// Token program id used when `CV_TOKEN_PROGRAM_ID` is unset (local executor tests only).
pub const LOCAL_TOKEN_PROGRAM_ID: AccountId = AccountId::new(*b"cliff-veil/test/token/0000000000");
/// ATA program id used when `CV_ATA_PROGRAM_ID` is unset (local executor tests only).
pub const LOCAL_ATA_PROGRAM_ID: AccountId = AccountId::new(*b"cliff-veil/test/ata/000000000000");

/// The `vesting_fees` program every creation asks for `CheckFee` (build input).
pub const FEES_PROGRAM_ID: AccountId = build_input(option_env!("CV_FEES_PROGRAM_ID"), LOCAL_FEES_PROGRAM_ID);
/// The one token program token schedules use (build input).
pub const TOKEN_PROGRAM_ID: AccountId = build_input(option_env!("CV_TOKEN_PROGRAM_ID"), LOCAL_TOKEN_PROGRAM_ID);
/// The ATA program that locates the treasury's holding for a token fee (build input).
pub const ATA_PROGRAM_ID: AccountId = build_input(option_env!("CV_ATA_PROGRAM_ID"), LOCAL_ATA_PROGRAM_ID);

/// Seed of the `vesting_fees` config account: the label `cliff-veil/fees/v1` zero-padded to 32 bytes, as
/// `lez_admin_authority::seed` builds it (SPEC 1.2).
pub const FEES_CONFIG_SEED: PdaSeed = PdaSeed::new(pad32(b"cliff-veil/fees/v1"));

/// Clock read granularity of a public claim and a cancel: 1 ms (SPEC 3.1).
pub const CLOCK_GRANULARITY_MS: u64 = 1;

/// Handles one delivery: `lee_core::actor_entrypoint!(raw vesting_program::handle)`.
pub fn handle(input: &ReceiveInput) -> Response {
    match input.from {
        Some(from) if from == clock_actor() => on_clock_reply(input),
        Some(from) if from.program_account_id == input.receiver.program_account_id => {
            on_internal(input, from.account_id)
        }
        _ => on_message(input),
    }
}

// ---------------------------------------------------------------------------------------------- caller actor

/// A [`Message`] at the caller's own actor (SPEC 3.1).
///
/// Refused with 23 at an actor that holds state (a schedule), 2 when it does not decode, 1 when the caller did not
/// authorise the transaction.
pub fn on_message(input: &ReceiveInput) -> Response {
    if !input.pre_state.is_empty() {
        refuse(VestingError::UnexpectedSender);
    }
    let message = Message::decode(&input.message).or_refuse();
    if !input.is_authorized {
        refuse(VestingError::NotAuthorised);
    }
    let program = input.receiver.program_account_id;
    let creator = ours(input.receiver.account_id);
    match message {
        Message::Create {
            schedule_id,
            terms,
            source,
            fee,
            treasury,
        } => {
            check_fee_cap(terms.total, fee).or_refuse();
            let (asset, total) = (terms.asset, terms.total);
            // The terms are checked here and again by `Open`, which runs the full order of SPEC 3.3.
            let state = terms.into_state(creator, schedule_id, source).or_refuse();
            let response = ask_fee_check(Response::keep_state(), total, fee, treasury, asset);
            let response = open_and_fund(response, program, state, source);
            pay_fee(response, asset, source, fee, treasury)
        }
        Message::CreateBatch {
            common,
            items,
            source,
            fee,
            treasury,
        } => {
            let (states, sum) = validate_batch(creator, source, &common, &items).or_refuse();
            check_fee_cap(sum, fee).or_refuse();
            let response = ask_fee_check(Response::keep_state(), sum, fee, treasury, common.asset);
            let response = states.into_iter().fold(response, |response, state| {
                open_and_fund(response, program, state, source)
            });
            pay_fee(response, common.asset, source, fee, treasury)
        }
        other => {
            // Every message but the two creations acts on one existing schedule.
            let (schedule, internal) = other.forward().unwrap_or_else(|| refuse(VestingError::InvalidMessage));
            Response::keep_state().send_sync(Actor::new(lez(schedule), program), &internal)
        }
    }
}

/// `CheckFee` to the `vesting_fees` config actor; it refuses (27) a fee or treasury that does not match its config.
fn ask_fee_check(
    response: Response,
    total: u128,
    fee: u128,
    treasury: vesting_core::AccountId,
    asset: Asset,
) -> Response {
    let check = FeeMessage::CheckFee {
        total,
        fee,
        treasury,
        asset,
    };
    response.send_sync(fees_config_actor(), &check)
}

/// `Open` at the derived schedule account with its seed attached (which admits the new account), then the escrow:
/// for a token schedule `InitializeHolding` on the schedule account with the seed, then `total` from `source`.
fn open_and_fund(
    response: Response,
    program: AccountId,
    state: ScheduleV1,
    source: vesting_core::AccountId,
) -> Response {
    let seed = schedule_seed(&state.creator, &state.schedule_id);
    let schedule = AccountId::for_public_pda(&program, &seed);
    let (asset, total) = (state.asset, state.total);
    let response = response
        .send(SyncMessage::new(Actor::new(schedule, program), &Internal::Open { state }).with_pda_seeds([seed]));
    match asset {
        Asset::Native => response.send_sync(
            Actor::native_balance(lez(source)),
            &native_token::Message::Transfer {
                to: schedule,
                amount: total,
            },
        ),
        Asset::Token { definition } => response
            .send(
                SyncMessage::new(
                    Actor::new(schedule, TOKEN_PROGRAM_ID),
                    &TokenMessage::InitializeHolding {
                        definition: lez(definition),
                    },
                )
                .with_pda_seeds([seed]),
            )
            .send_sync(
                Actor::new(lez(source), TOKEN_PROGRAM_ID),
                &TokenMessage::Transfer {
                    to: schedule,
                    amount: total,
                },
            ),
    }
}

/// The creation fee from `source` (SPEC 6): to `native(treasury)`, or to the treasury's associated token account
/// for the definition. Nothing when the fee is 0.
fn pay_fee(
    response: Response,
    asset: Asset,
    source: vesting_core::AccountId,
    fee: u128,
    treasury: vesting_core::AccountId,
) -> Response {
    if fee == 0 {
        return response;
    }
    match asset {
        Asset::Native => response.send_sync(
            Actor::native_balance(lez(source)),
            &native_token::Message::Transfer {
                to: lez(treasury),
                amount: fee,
            },
        ),
        Asset::Token { definition } => response.send_sync(
            Actor::new(lez(source), TOKEN_PROGRAM_ID),
            &TokenMessage::Transfer {
                to: treasury_ata(treasury, definition),
                amount: fee,
            },
        ),
    }
}

/// The treasury's associated token account for `definition` (SPEC 6).
#[must_use]
pub fn treasury_ata(treasury: vesting_core::AccountId, definition: vesting_core::AccountId) -> AccountId {
    let seed = ata_seed(&ours(TOKEN_PROGRAM_ID), &treasury, &definition);
    AccountId::for_public_pda(&ATA_PROGRAM_ID, &PdaSeed::new(seed))
}

/// The `vesting_fees` config actor.
#[must_use]
pub fn fees_config_actor() -> Actor {
    Actor::new(
        AccountId::for_public_pda(&FEES_PROGRAM_ID, &FEES_CONFIG_SEED),
        FEES_PROGRAM_ID,
    )
}

// ---------------------------------------------------------------------------------------------- schedule actor

/// An [`Internal`] message from `sender`'s actor of this program (SPEC 3.3).
pub fn on_internal(input: &ReceiveInput, sender: AccountId) -> Response {
    let message = Internal::decode(&input.message).or_refuse();
    if let Internal::Open { state } = message {
        return open(input, sender, state);
    }
    let here = input.receiver.account_id;
    let mut s = load(input);
    // Only `Open` has no role; it was handled above.
    let role = message
        .required_role()
        .unwrap_or_else(|| refuse(VestingError::InvalidMessage));
    s.authorise(role, &ours(sender)).or_refuse();
    match message {
        Internal::ClaimFor { to } => {
            check_destination(&ours(here), &to).or_refuse();
            s.begin(Pending::Claim { to }).or_refuse();
            ask_clock(save(&s))
        }
        Internal::ClaimForAsOf { to, as_of } => {
            check_destination(&ours(here), &to).or_refuse();
            s.ensure_idle().or_refuse();
            let outcome = s.apply_claim(as_of).or_refuse();
            pay_claim(here, &s, &outcome, to).timestamp_window(as_of..)
        }
        Internal::CancelFor => {
            s.check_cancel().or_refuse();
            s.begin(Pending::Cancel).or_refuse();
            ask_clock(save(&s))
        }
        Internal::MakeNonCancelableFor => {
            s.ensure_idle().or_refuse();
            s.apply_make_non_cancelable().or_refuse();
            let event = MadeNonCancelable {
                schedule: ours(here),
                by: ours(sender),
            };
            save(&s).event(emit(&event))
        }
        Internal::SignalFor { index } => {
            s.ensure_idle().or_refuse();
            let outcome = s.apply_signal_milestone(index).or_refuse();
            save(&s).event(emit(&outcome.event(ours(here))))
        }
        Internal::TransferFor { new_beneficiary } => {
            s.ensure_idle().or_refuse();
            let from = s.apply_transfer_beneficiary(new_beneficiary).or_refuse();
            let event = BeneficiaryTransferred {
                schedule: ours(here),
                from,
                to: new_beneficiary,
            };
            save(&s).event(emit(&event))
        }
        Internal::Open { .. } => refuse(VestingError::InvalidMessage),
    }
}

/// `Open { state }` from `sender` (SPEC 3.3). Refused, in this order: a sender other than `state.creator` or a
/// receiving account other than the derivation of `(state.creator, state.schedule_id)` (9); an actor that already
/// holds a schedule (7); invalid terms or a state that is not fresh (10, 11, 13, 12, 29, then 2, as
/// [`ScheduleV1::validate_new`]); `refund_to` equal to the schedule account (26).
fn open(input: &ReceiveInput, sender: AccountId, state: ScheduleV1) -> Response {
    let here = input.receiver.account_id;
    let derived = AccountId::for_public_pda(
        &input.receiver.program_account_id,
        &schedule_seed(&state.creator, &state.schedule_id),
    );
    if ours(sender) != state.creator || derived != here {
        refuse(VestingError::AddressMismatch);
    }
    if !input.pre_state.is_empty() {
        refuse(VestingError::ScheduleExists);
    }
    state.validate_new().or_refuse();
    check_destination(&ours(here), &state.refund_to).or_refuse();
    let event = ScheduleCreated::from_state(ours(here), &state);
    save(&state).event(emit(&event))
}

/// The clock's reply completes the pending claim or cancel (SPEC 3.3).
pub fn on_clock_reply(input: &ReceiveInput) -> Response {
    let mut s = load(input);
    let now = match Reply::try_from_slice(&input.message) {
        Ok(Reply::Timestamp(now)) => now,
        Ok(Reply::BlockId(_)) => refuse(VestingError::UnexpectedClockReply),
        Err(_) => refuse(VestingError::InvalidMessage),
    };
    let here = input.receiver.account_id;
    match s.finish().or_refuse() {
        Pending::Claim { to } => {
            let outcome = s.apply_claim(now).or_refuse();
            pay_claim(here, &s, &outcome, to)
        }
        Pending::Cancel => {
            let cancellation = s.apply_cancel(now).or_refuse();
            let event = Cancelled::new(ours(here), &cancellation, s.refund_to);
            let response = save(&s).event(emit(&event));
            if cancellation.refunded == 0 {
                // Nothing unvested: a valid cancel with no transfer.
                response
            } else {
                response.send(payout(here, &s, s.refund_to, cancellation.refunded))
            }
        }
    }
}

/// Writes the claimed state, emits `Claimed` (no destination in it) and pays `to` from the escrow.
fn pay_claim(here: AccountId, s: &ScheduleV1, outcome: &ClaimOutcome, to: vesting_core::AccountId) -> Response {
    let event = outcome.event(ours(here), s.beneficiary);
    save(s).event(emit(&event)).send(payout(here, s, to, outcome.amount))
}

/// A debit of the schedule's escrow to `to`, authorised by the schedule seed.
fn payout(schedule: AccountId, s: &ScheduleV1, to: vesting_core::AccountId, amount: u128) -> SyncMessage {
    let seed = schedule_seed(&s.creator, &s.schedule_id);
    match s.asset {
        Asset::Native => native_token::custody_transfer(schedule, seed, lez(to), amount),
        Asset::Token { .. } => SyncMessage::new(
            Actor::new(schedule, TOKEN_PROGRAM_ID),
            &TokenMessage::Transfer { to: lez(to), amount },
        )
        .with_pda_seeds([seed]),
    }
}

fn ask_clock(response: Response) -> Response {
    response.send_sync(
        clock_actor(),
        &ClockMessage::ReadTimestamp {
            granularity: CLOCK_GRANULARITY_MS,
        },
    )
}

/// The schedule in the receiving actor; 8 when it holds none.
fn load(input: &ReceiveInput) -> ScheduleV1 {
    ScheduleState::decode_v1(&input.pre_state).or_refuse()
}

fn save(s: &ScheduleV1) -> Response {
    Response::set_state(ScheduleState::V1(s.clone()).encode())
}

fn emit<E: Event>(event: &E) -> ProgramEvent {
    ProgramEvent {
        selector: E::SELECTOR,
        data: event.data(),
    }
}

// ---------------------------------------------------------------------------------------------- helpers

/// The schedule seed `SHA-256("cliff-veil/schedule/v1" ‖ creator ‖ schedule_id)` (SPEC 1.2), hashed with the zkVM's
/// accelerated SHA-256 over `vesting_core::schedule_seed_preimage`; equal to `vesting_core::schedule_seed`.
#[must_use]
pub fn schedule_seed(creator: &vesting_core::AccountId, schedule_id: &[u8; 32]) -> PdaSeed {
    let digest = Impl::hash_bytes(&schedule_seed_preimage(creator, schedule_id));
    let mut seed = [0u8; 32];
    seed.copy_from_slice(digest.as_bytes());
    PdaSeed::new(seed)
}

/// The schedule account of `(creator, schedule_id)` under the `vesting` program at `program`.
#[must_use]
pub fn schedule_account(program: &AccountId, creator: &vesting_core::AccountId, schedule_id: &[u8; 32]) -> AccountId {
    AccountId::for_public_pda(program, &schedule_seed(creator, schedule_id))
}

/// Fails the transaction with `cliff-veil error <code>: <message>`.
fn refuse(error: VestingError) -> ! {
    panic!("{error}")
}

trait OrRefuse<T> {
    fn or_refuse(self) -> T;
}

impl<T> OrRefuse<T> for Result<T, VestingError> {
    fn or_refuse(self) -> T {
        self.unwrap_or_else(|error| refuse(error))
    }
}

const fn lez(id: vesting_core::AccountId) -> AccountId {
    AccountId::new(id.into_value())
}

const fn ours(id: AccountId) -> vesting_core::AccountId {
    vesting_core::AccountId::new(id.into_value())
}

const fn pad32(label: &[u8]) -> [u8; 32] {
    assert!(label.len() <= 32, "label longer than 32 bytes");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < label.len() {
        out[i] = label[i];
        i += 1;
    }
    out
}

/// A build-input id: `value` decoded from base58, else `default`. A malformed value fails the build.
const fn build_input(value: Option<&str>, default: AccountId) -> AccountId {
    match value {
        Some(text) => AccountId::new(decode_base58_id(text)),
        None => default,
    }
}

/// Decodes the base58 text of a 32-byte account id (Bitcoin alphabet, as LEZ prints ids). Panics, at compile time
/// when used in a constant, on an empty text, a character outside the alphabet or a value above 32 bytes.
#[must_use]
pub const fn decode_base58_id(text: &str) -> [u8; 32] {
    const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let bytes = text.as_bytes();
    assert!(!bytes.is_empty(), "empty base58 account id");
    let mut out = [0u8; 32];
    let mut c = 0;
    while c < bytes.len() {
        let mut digit = 0;
        while digit < 58 && ALPHABET[digit] != bytes[c] {
            digit += 1;
        }
        assert!(digit < 58, "invalid base58 character in an account id");
        // out = out * 58 + digit, big-endian.
        let mut carry = digit as u32;
        let mut i = 32;
        while i > 0 {
            i -= 1;
            carry += out[i] as u32 * 58;
            out[i] = (carry & 0xff) as u8;
            carry >>= 8;
        }
        assert!(carry == 0, "base58 account id longer than 32 bytes");
        c += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_seed_matches_the_core_derivation() {
        let creator = vesting_core::AccountId::new([3; 32]);
        let id = [9; 32];
        assert_eq!(
            *schedule_seed(&creator, &id).as_bytes(),
            vesting_core::schedule_seed(&creator, &id)
        );
    }

    #[test]
    fn base58_ids_decode_as_lez_prints_them() {
        for bytes in [[0u8; 32], [1; 32], [0xff; 32], *b"cliff-veil/test/token/0000000000"] {
            let id = AccountId::new(bytes);
            assert_eq!(decode_base58_id(&id.to_string()), bytes, "{id}");
        }
    }

    #[test]
    fn fees_config_seed_is_the_padded_label() {
        let mut expected = [0u8; 32];
        expected[..18].copy_from_slice(b"cliff-veil/fees/v1");
        assert_eq!(*FEES_CONFIG_SEED.as_bytes(), expected);
    }

    #[test]
    fn unset_build_inputs_are_the_local_ids() {
        if option_env!("CV_FEES_PROGRAM_ID").is_none() {
            assert_eq!(FEES_PROGRAM_ID, LOCAL_FEES_PROGRAM_ID);
        }
        if option_env!("CV_TOKEN_PROGRAM_ID").is_none() {
            assert_eq!(TOKEN_PROGRAM_ID, LOCAL_TOKEN_PROGRAM_ID);
        }
        if option_env!("CV_ATA_PROGRAM_ID").is_none() {
            assert_eq!(ATA_PROGRAM_ID, LOCAL_ATA_PROGRAM_ID);
        }
    }
}
