//! The checked-in IDLs against the program (SPEC section 10).
//!
//! Every message, internal message, state and event is encoded with `vesting_core` (the encoding the program and
//! the SDK use), then decoded with SPEL's IDL-driven decoder through the checked-in IDL, and compared with the values
//! it was built from. Every field holds a distinct value, so a missing, extra, retyped or reordered field fails.

use std::path::Path;

use serde_json::{Value, json};
use spel_framework_core::decode::{decode_named, decode_state, decode_value};
use spel_framework_core::idl::{IdlEnumVariant, IdlMessage, IdlType, IdlTypeDef, SpelIdl};
use vesting_core::{
    AccountId, Asset, BatchItem, BatchShape, BatchTerms, BeneficiaryTransferred, Cancellation, Cancelled, Claimed,
    ERRORS, Event, FeeChanged, FeeMessage, Internal, Kind, MadeNonCancelable, Message, MilestoneSignalled, Pending,
    ScheduleCreated, ScheduleState, ScheduleV1, Terms,
};
use vesting_idl::{FEES_IDL, VESTING_IDL};

const A: AccountId = AccountId::new([1; 32]);
const B: AccountId = AccountId::new([2; 32]);
const C: AccountId = AccountId::new([3; 32]);
const D: AccountId = AccountId::new([4; 32]);

fn vesting() -> SpelIdl {
    SpelIdl::from_json(VESTING_IDL).expect("vesting.json loads in SPEL")
}

fn fees() -> SpelIdl {
    SpelIdl::from_json(FEES_IDL).expect("vesting_fees.json loads in SPEL")
}

fn raw(json: &str) -> Value {
    serde_json::from_str(json).expect("valid JSON")
}

/// How SPEL renders the account `[b; 32]`.
fn id(b: u8) -> Value {
    decode_value(&[b; 32], &IdlType::Primitive("account_id".to_owned()), &vesting()).unwrap()
}

/// How SPEL renders the bytes `[b; 32]` (lowercase hex).
fn bytes32(b: u8) -> Value {
    json!(format!("{b:02x}").repeat(32))
}

/// Decodes `bytes` as one of `messages`: the leading byte is the tag, then the variant's fields, all bytes consumed.
fn decode_message(idl: &SpelIdl, messages: &[IdlMessage], bytes: &[u8]) -> Value {
    for (index, message) in messages.iter().enumerate() {
        assert_eq!(
            usize::from(message.tag),
            index,
            "{} has the tag of its position",
            message.name
        );
    }
    let mut with_enum = idl.clone();
    with_enum.types.push(IdlTypeDef {
        name: "Message".to_owned(),
        kind: "enum".to_owned(),
        fields: Vec::new(),
        variants: messages
            .iter()
            .map(|m| IdlEnumVariant {
                name: m.name.clone(),
                fields: m.fields.clone(),
            })
            .collect(),
    });
    decode_named(bytes, "Message", &with_enum).unwrap_or_else(|e| panic!("decode {bytes:02x?}: {e}"))
}

/// The variant name; the match is exhaustive, so a new message cannot be added without an example below.
const fn message_name(message: &Message) -> &'static str {
    match message {
        Message::Create { .. } => "Create",
        Message::CreateBatch { .. } => "CreateBatch",
        Message::Claim { .. } => "Claim",
        Message::ClaimAsOf { .. } => "ClaimAsOf",
        Message::Cancel { .. } => "Cancel",
        Message::MakeNonCancelable { .. } => "MakeNonCancelable",
        Message::SignalMilestone { .. } => "SignalMilestone",
        Message::TransferBeneficiary { .. } => "TransferBeneficiary",
    }
}

/// As [`message_name`], for [`Internal`].
const fn internal_name(message: &Internal) -> &'static str {
    match message {
        Internal::Open { .. } => "Open",
        Internal::ClaimFor { .. } => "ClaimFor",
        Internal::ClaimForAsOf { .. } => "ClaimForAsOf",
        Internal::CancelFor => "CancelFor",
        Internal::MakeNonCancelableFor => "MakeNonCancelableFor",
        Internal::SignalFor { .. } => "SignalFor",
        Internal::TransferFor { .. } => "TransferFor",
    }
}

/// One example of every [`Message`], in tag order, with the JSON SPEL must decode it to.
fn every_message() -> Vec<(Message, Value)> {
    vec![
        (
            Message::Create {
                schedule_id: [7; 32],
                terms: Terms {
                    beneficiary: B,
                    asset: Asset::Token { definition: C },
                    total: 1_000,
                    kind: Kind::Linear {
                        start: 11,
                        cliff: 12,
                        end: 13,
                    },
                    cancelable: true,
                    transferable: false,
                    cancel_authority: Some(D),
                    milestone_authority: None,
                    refund_to: Some(A),
                },
                source: A,
                fee: 5,
                treasury: D,
            },
            json!({"Create": {
                "schedule_id": bytes32(7),
                "terms": {
                    "beneficiary": id(2),
                    "asset": {"Token": {"definition": id(3)}},
                    "total": "1000",
                    "kind": {"Linear": {"start": "11", "cliff": "12", "end": "13"}},
                    "cancelable": true,
                    "transferable": false,
                    "cancel_authority": id(4),
                    "milestone_authority": null,
                    "refund_to": id(1),
                },
                "source": id(1),
                "fee": "5",
                "treasury": id(4),
            }}),
        ),
        (
            Message::CreateBatch {
                common: BatchTerms {
                    asset: Asset::Native,
                    shape: BatchShape::Milestones {
                        bps: vec![2_500, 7_500],
                    },
                    cancelable: false,
                    transferable: true,
                    cancel_authority: None,
                    milestone_authority: Some(C),
                    refund_to: None,
                },
                items: vec![
                    BatchItem {
                        schedule_id: [8; 32],
                        beneficiary: B,
                        total: 40,
                    },
                    BatchItem {
                        schedule_id: [9; 32],
                        beneficiary: C,
                        total: 60,
                    },
                ],
                source: A,
                fee: 1,
                treasury: D,
            },
            json!({"CreateBatch": {
                "common": {
                    "asset": "Native",
                    "shape": {"Milestones": {"bps": [2500, 7500]}},
                    "cancelable": false,
                    "transferable": true,
                    "cancel_authority": null,
                    "milestone_authority": id(3),
                    "refund_to": null,
                },
                "items": [
                    {"schedule_id": bytes32(8), "beneficiary": id(2), "total": "40"},
                    {"schedule_id": bytes32(9), "beneficiary": id(3), "total": "60"},
                ],
                "source": id(1),
                "fee": "1",
                "treasury": id(4),
            }}),
        ),
        (
            Message::Claim { schedule: A, to: B },
            json!({"Claim": {"schedule": id(1), "to": id(2)}}),
        ),
        (
            Message::ClaimAsOf {
                schedule: A,
                to: B,
                as_of: 77,
            },
            json!({"ClaimAsOf": {"schedule": id(1), "to": id(2), "as_of": "77"}}),
        ),
        (Message::Cancel { schedule: C }, json!({"Cancel": {"schedule": id(3)}})),
        (
            Message::MakeNonCancelable { schedule: D },
            json!({"MakeNonCancelable": {"schedule": id(4)}}),
        ),
        (
            Message::SignalMilestone { schedule: A, index: 3 },
            json!({"SignalMilestone": {"schedule": id(1), "index": 3}}),
        ),
        (
            Message::TransferBeneficiary {
                schedule: A,
                new_beneficiary: C,
            },
            json!({"TransferBeneficiary": {"schedule": id(1), "new_beneficiary": id(3)}}),
        ),
    ]
}

/// A schedule with every optional part set, and its JSON.
fn full_schedule() -> (ScheduleV1, Value) {
    let schedule = ScheduleV1 {
        schedule_id: [9; 32],
        creator: A,
        beneficiary: B,
        asset: Asset::Token { definition: C },
        total: 1_000,
        claimed: 10,
        last_time: 77,
        kind: Kind::Milestones {
            amounts: vec![400, 600],
            signalled: 1,
        },
        cancelable: true,
        transferable: false,
        cancel_authority: D,
        milestone_authority: C,
        refund_to: A,
        cancelled: Some(Cancellation {
            at: 80,
            vested_at_cancel: 401,
            refunded: 599,
        }),
        pending: Some(Pending::Claim { to: D }),
    };
    let value = json!({
        "schedule_id": bytes32(9),
        "creator": id(1),
        "beneficiary": id(2),
        "asset": {"Token": {"definition": id(3)}},
        "total": "1000",
        "claimed": "10",
        "last_time": "77",
        "kind": {"Milestones": {"amounts": ["400", "600"], "signalled": "1"}},
        "cancelable": true,
        "transferable": false,
        "cancel_authority": id(4),
        "milestone_authority": id(3),
        "refund_to": id(1),
        "cancelled": {"at": "80", "vested_at_cancel": "401", "refunded": "599"},
        "pending": {"Claim": {"to": id(4)}},
    });
    (schedule, value)
}

#[test]
fn u6_idl_files_are_generated_from_core_and_up_to_date() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (name, fresh) in vesting_idl::generate_all(&vesting_idl::core_dir()).unwrap() {
        let checked_in = std::fs::read_to_string(dir.join(name)).unwrap();
        assert!(
            checked_in == fresh,
            "idl/{name} differs from a fresh generation; run `cargo run -p vesting_idl` and commit the result"
        );
    }
}

#[test]
fn u6_idl_loads_in_spel_and_names_the_program() {
    let idl = vesting();
    assert_eq!((idl.version.as_str(), idl.name.as_str()), ("2", "vesting"));
    assert_eq!(idl.state_types, ["ScheduleState"]);
    // SPEC 10: the types the messages and the state use are all described.
    for name in [
        "Terms",
        "BatchTerms",
        "BatchShape",
        "BatchItem",
        "Asset",
        "Kind",
        "ScheduleState",
        "ScheduleV1",
        "Cancellation",
        "Pending",
    ] {
        assert!(idl.type_def(name).is_some(), "{name} is described");
    }
    // The CLI names messages in kebab case.
    assert_eq!(idl.message("claim-as-of").map(|m| m.tag), Some(3));
    let fees = fees();
    assert_eq!((fees.version.as_str(), fees.name.as_str()), ("2", "vesting_fees"));
    assert_eq!(fees.state_types, ["FeeConfig"]);
}

#[test]
fn u6_idl_matches_program_messages() {
    let idl = vesting();
    let examples = every_message();
    let names: Vec<&str> = examples.iter().map(|(m, _)| message_name(m)).collect();
    let described: Vec<&str> = idl.messages.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(described, names, "one IDL message per Message variant, in tag order");
    for (tag, (message, expected)) in examples.into_iter().enumerate() {
        let bytes = message.encode();
        assert_eq!(usize::from(bytes[0]), tag, "{} is tag {tag}", message_name(&message));
        assert_eq!(decode_message(&idl, &idl.messages, &bytes), expected);
    }
}

#[test]
fn u6_idl_matches_program_internal_messages() {
    let idl = vesting();
    let internal: Vec<IdlMessage> =
        serde_json::from_value(raw(VESTING_IDL)["internal_messages"].clone()).expect("internal_messages");
    let (state, state_json) = full_schedule();
    let examples = vec![
        (Internal::Open { state }, json!({"Open": {"state": state_json}})),
        (Internal::ClaimFor { to: B }, json!({"ClaimFor": {"to": id(2)}})),
        (
            Internal::ClaimForAsOf { to: C, as_of: 9 },
            json!({"ClaimForAsOf": {"to": id(3), "as_of": "9"}}),
        ),
        (Internal::CancelFor, json!("CancelFor")),
        (Internal::MakeNonCancelableFor, json!("MakeNonCancelableFor")),
        (Internal::SignalFor { index: 5 }, json!({"SignalFor": {"index": 5}})),
        (
            Internal::TransferFor { new_beneficiary: D },
            json!({"TransferFor": {"new_beneficiary": id(4)}}),
        ),
    ];
    let names: Vec<&str> = examples.iter().map(|(m, _)| internal_name(m)).collect();
    let described: Vec<&str> = internal.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        described, names,
        "one IDL internal message per Internal variant, in tag order"
    );
    for (message, expected) in examples {
        assert_eq!(decode_message(&idl, &internal, &message.encode()), expected);
    }
}

#[test]
fn u6_idl_matches_program_state() {
    let (schedule, value) = full_schedule();
    let bytes = ScheduleState::V1(schedule).encode();
    assert_eq!(decode_state(&bytes, &vesting()).unwrap(), json!({ "V1": value }));
}

/// Checks an event's selector against the program's constant, and its payload through the IDL.
fn check_event<E: Event>(idl: &SpelIdl, event: &E, expected: &Value) {
    let described = idl
        .events
        .iter()
        .find(|e| e.name == E::NAME)
        .unwrap_or_else(|| panic!("{} is in the IDL", E::NAME));
    let selector: String = E::SELECTOR.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(described.selector, selector, "selector of {}", E::NAME);
    assert_eq!(
        described.type_,
        IdlType::Defined {
            defined: E::NAME.to_owned()
        }
    );
    assert_eq!(
        &decode_named(&event.data(), E::NAME, idl).unwrap(),
        expected,
        "{}",
        E::NAME
    );
}

#[test]
fn u6_idl_matches_program_events() {
    let idl = vesting();
    let names: Vec<&str> = idl.events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "ScheduleCreated",
            "Claimed",
            "Cancelled",
            "MilestoneSignalled",
            "BeneficiaryTransferred",
            "MadeNonCancelable",
            "FeeChanged"
        ],
        "every event of SPEC 7"
    );
    let (state, _) = full_schedule();
    check_event(
        &idl,
        &ScheduleCreated::from_state(D, &state),
        &json!({
            "schedule": id(4),
            "schedule_id": bytes32(9),
            "creator": id(1),
            "beneficiary": id(2),
            "asset": {"Token": {"definition": id(3)}},
            "total": "1000",
            "kind": {"Milestones": {"amounts": ["400", "600"], "signalled": "1"}},
            "cancelable": true,
            "transferable": false,
            "cancel_authority": id(4),
            "milestone_authority": id(3),
            "refund_to": id(1),
        }),
    );
    check_event(
        &idl,
        &Claimed {
            schedule: A,
            beneficiary: B,
            amount: 3,
            at: 4,
            claimed_total: 5,
        },
        &json!({"schedule": id(1), "beneficiary": id(2), "amount": "3", "at": "4", "claimed_total": "5"}),
    );
    check_event(
        &idl,
        &Cancelled {
            schedule: A,
            at: 6,
            vested_at_cancel: 7,
            refunded: 8,
            refund_to: C,
        },
        &json!({"schedule": id(1), "at": "6", "vested_at_cancel": "7", "refunded": "8", "refund_to": id(3)}),
    );
    check_event(
        &idl,
        &MilestoneSignalled {
            schedule: B,
            index: 2,
            amount: 9,
            signalled: 5,
        },
        &json!({"schedule": id(2), "index": 2, "amount": "9", "signalled": "5"}),
    );
    check_event(
        &idl,
        &BeneficiaryTransferred {
            schedule: A,
            from: B,
            to: C,
        },
        &json!({"schedule": id(1), "from": id(2), "to": id(3)}),
    );
    check_event(
        &idl,
        &MadeNonCancelable { schedule: C, by: D },
        &json!({"schedule": id(3), "by": id(4)}),
    );
    check_event(&idl, &fee_changed(), &fee_changed_json());
}

fn fee_changed() -> FeeChanged {
    FeeChanged {
        old_rate_bps: 10,
        new_rate_bps: 50,
        treasury: C,
        by: D,
    }
}

fn fee_changed_json() -> Value {
    json!({"old_rate_bps": 10, "new_rate_bps": 50, "treasury": id(3), "by": id(4)})
}

#[test]
fn u6_idl_matches_program_errors() {
    let errors = raw(VESTING_IDL)["errors"].clone();
    let rows = errors.as_array().expect("errors is a list");
    assert_eq!(rows.len(), ERRORS.len(), "every code of SPEC 5");
    for (row, info) in rows.iter().zip(ERRORS.iter()) {
        assert_eq!(row, &json!({"code": info.code, "name": info.name, "msg": info.message}));
    }
    let codes: Vec<u64> = rows.iter().map(|r| r["code"].as_u64().unwrap()).collect();
    assert_eq!(codes, (1..=29).collect::<Vec<u64>>(), "codes 1 to 29, no gap, no reuse");
    // The fee switch lists the two codes it raises itself.
    let fee_codes: Vec<u64> = raw(FEES_IDL)["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["code"].as_u64().unwrap())
        .collect();
    assert_eq!(fee_codes, [27, 28]);
}

#[test]
fn u6_fees_idl_matches_program() {
    use lez_admin_authority::{AuthoritySlot, Holder};
    use vesting_fees::{FeeConfig, wire};

    let idl = fees();
    let names: Vec<&str> = idl.messages.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["CheckFee", "SetFee"]);
    // Encoded as the program's own message enum encodes them on the wire.
    let check = FeeMessage::CheckFee {
        total: 1_000,
        fee: 5,
        treasury: C,
        asset: Asset::Token { definition: D },
    };
    let set = FeeMessage::SetFee {
        rate_bps: 25,
        treasury: B,
    };
    let on_wire = |m: FeeMessage| borsh::to_vec(&wire(m.into())).unwrap();
    assert_eq!(
        decode_message(&idl, &idl.messages, &on_wire(check)),
        json!({"CheckFee": {"total": "1000", "fee": "5", "treasury": id(3), "asset": {"Token": {"definition": id(4)}}}})
    );
    assert_eq!(
        decode_message(&idl, &idl.messages, &on_wire(set)),
        json!({"SetFee": {"rate_bps": 25, "treasury": id(2)}})
    );

    // The config state, as the program encodes it, in each admin slot state.
    let lee = |b: u8| lee_core::account::AccountId::new([b; 32]);
    let config = |admin| FeeConfig {
        rate_bps: 25,
        treasury: lee(3),
        admin,
    };
    let held = AuthoritySlot::Held {
        holder: Holder {
            account: lee(1),
            via: Some(lee(4)),
        },
        nominee: Some(lee(2)),
    };
    let decode = |c: FeeConfig| decode_state(&borsh::to_vec(&c).unwrap(), &idl).unwrap();
    assert_eq!(
        decode(config(held)),
        json!({"rate_bps": 25, "treasury": id(3), "admin": {"Held": {
            "holder": {"account": id(1), "via": id(4)}, "nominee": id(2)}}})
    );
    assert_eq!(
        decode(config(AuthoritySlot::Unset)),
        json!({"rate_bps": 25, "treasury": id(3), "admin": "Unset"})
    );
    assert_eq!(
        decode(config(AuthoritySlot::Renounced)),
        json!({"rate_bps": 25, "treasury": id(3), "admin": "Renounced"})
    );

    // `FeeChanged` keeps the `vesting::` selector the program emits (SPEC 7).
    check_event(&idl, &fee_changed(), &fee_changed_json());
}
