//! SPEL IDL (version 2) of the `vesting` and `vesting_fees` programs (SPEC section 10).
//!
//! The IDLs are generated from `vesting_core` with SPEL's own generator
//! (`spel_framework_core::idl_gen::generate_idl`, the code behind `spel generate-idl`), at the SPEL commit pinned in
//! this crate's `Cargo.toml`, and checked in as `idl/vesting.json` and `idl/vesting_fees.json`. The tests in
//! `tests/` regenerate both and fail when a checked-in file differs, and decode bytes encoded by `vesting_core` with
//! SPEL's IDL-driven decoder, so the IDL cannot drift from the program. `docs/IDL.md` says how to regenerate.
//!
//! `vesting.json` is what `spel generate-idl core --message Message --state ScheduleState --events events` produces,
//! with two additions:
//! - The event structs and selectors. `vesting_core` declares its event impls through a small macro
//!   (`event!(Name, selector)`), which a source reader does not expand, so the event names are read from those
//!   invocations; the selectors come from SPEL's `event_selector` and the payload types from SPEL's generator.
//! - Two keys outside SPEL's format, which `spel` ignores when it loads the file: `internal_messages` (the
//!   `Internal` enum of SPEC 3.3, generated the same way; only `vesting`'s own actors send it) and `errors` (the
//!   code table of SPEC 5, in the shape of SPEL's version 1 `errors`).
//!
//! `vesting_fees.json` is generated from `vesting_core::FeeMessage`, which `vesting_fees` encodes byte for byte. Its
//! config state names a type of `lez-admin-authority` (a git dependency the generator does not read), so the
//! state's three types are written out here and a test decodes a real config with them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use spel_framework_core::idl::{
    IdlEnumVariant, IdlEvent, IdlField, IdlMessage, IdlType, IdlTypeDef, SpelIdl, event_selector,
};
use spel_framework_core::idl_gen::{GenOptions, IdlGenError, generate_idl};
use vesting_core::ERRORS;

/// The checked-in IDL of `vesting`.
pub const VESTING_IDL: &str = include_str!("../vesting.json");
/// The checked-in IDL of `vesting_fees`.
pub const FEES_IDL: &str = include_str!("../vesting_fees.json");

/// File names of the IDLs, in `idl/`.
pub const VESTING_FILE: &str = "vesting.json";
/// See [`VESTING_FILE`].
pub const FEES_FILE: &str = "vesting_fees.json";

/// Event namespace of every selector (SPEC 7), `vesting_fees`' `FeeChanged` included.
pub const EVENT_NAMESPACE: &str = "vesting";
/// Program name in the IDL of the fee switch.
pub const FEES_NAME: &str = "vesting_fees";
/// Codes of [`ERRORS`] that `vesting_fees` raises itself (SPEC 5).
pub const FEES_ERROR_CODES: [u32; 2] = [27, 28];

/// A SPEL version 2 IDL with the two additions this repository makes (see the crate docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProgramIdl {
    /// Everything SPEL reads.
    #[serde(flatten)]
    pub spel: SpelIdl,
    /// Messages only the program's own actors send (SPEC 3.3); clients never send them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub internal_messages: Vec<IdlMessage>,
    /// The program's refusal codes (SPEC 5).
    pub errors: Vec<IdlError>,
}

/// One row of the error table, as SPEL version 1 wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdlError {
    /// Stable numeric code.
    pub code: u32,
    /// Variant name of `vesting_core::VestingError`.
    pub name: String,
    /// The message (for code 14, with the placeholder `<T>`).
    pub msg: String,
}

impl ProgramIdl {
    /// Pretty-printed JSON with a final newline, as checked in.
    ///
    /// # Errors
    /// Never in practice: every field serializes.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map(|json| json + "\n")
            .map_err(|e| e.to_string())
    }
}

/// The `core/` directory next to this crate.
#[must_use]
pub fn core_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../core")
}

/// The IDL of `vesting`, generated from the core crate at `core`.
///
/// # Errors
/// A generator error (a renamed or undescribable type), or two passes describing one type differently.
pub fn vesting_idl(core: &Path) -> Result<ProgramIdl, String> {
    let mut spel = generate(core, "Message", &["ScheduleState"], Some("events"), None)?;
    if spel.events.is_empty() {
        let names = macro_event_names(core)?;
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let payloads = generate(core, "Message", &names, None, None)?;
        merge_types(&mut spel.types, payloads.types)?;
        spel.events = names.iter().map(|name| event(name)).collect();
    }
    let internal = generate(core, "Internal", &[], None, None)?;
    merge_types(&mut spel.types, internal.types)?;
    Ok(ProgramIdl {
        spel,
        internal_messages: internal.messages,
        errors: ERRORS.iter().map(error_row).collect(),
    })
}

/// The IDL of `vesting_fees`, generated from `vesting_core::FeeMessage` in the core crate at `core`.
///
/// # Errors
/// As [`vesting_idl`].
pub fn fees_idl(core: &Path) -> Result<ProgramIdl, String> {
    // The `FeeChanged` payload is described by naming it as a state type, then the real state type replaces it.
    let mut spel = generate(core, "FeeMessage", &["FeeChanged"], None, Some(FEES_NAME))?;
    merge_types(&mut spel.types, fee_config_types())?;
    spel.state_types = vec!["FeeConfig".to_owned()];
    spel.events = vec![event("FeeChanged")];
    Ok(ProgramIdl {
        spel,
        internal_messages: Vec::new(),
        errors: ERRORS
            .iter()
            .filter(|row| FEES_ERROR_CODES.contains(&row.code))
            .map(error_row)
            .collect(),
    })
}

/// Both IDLs as `(file name, JSON)`.
///
/// # Errors
/// As [`vesting_idl`].
pub fn generate_all(core: &Path) -> Result<[(&'static str, String); 2], String> {
    Ok([
        (VESTING_FILE, vesting_idl(core)?.to_json()?),
        (FEES_FILE, fees_idl(core)?.to_json()?),
    ])
}

/// One run of SPEL's generator. A missing events module is not an error here: the caller falls back to the macro.
fn generate(
    core: &Path,
    message: &str,
    states: &[&str],
    events: Option<&str>,
    namespace: Option<&str>,
) -> Result<SpelIdl, String> {
    let options = GenOptions {
        crate_dir: core.to_path_buf(),
        message: Some(message.to_owned()),
        states: states.iter().map(|s| (*s).to_owned()).collect(),
        events: events.map(str::to_owned),
        namespace: namespace.map(str::to_owned),
    };
    let mut warnings = Vec::new();
    let result = generate_idl(&options, |w| warnings.push(w));
    if !warnings.is_empty() {
        return Err(format!("SPEL generator warnings: {}", warnings.join("; ")));
    }
    match result {
        Ok(idl) => Ok(idl),
        Err(IdlGenError::NoEvents { .. }) => generate(core, message, states, None, namespace),
        Err(e) => Err(e.to_string()),
    }
}

/// The event names of the `event!(Name, selector)` invocations in the core crate's `src/events.rs`, in order.
fn macro_event_names(core: &Path) -> Result<Vec<String>, String> {
    let path = core.join("src/events.rs");
    let source = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let names: Vec<String> = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("event!("))
        .filter_map(|rest| rest.split(',').next())
        .map(|name| name.trim().to_owned())
        .collect();
    if names.is_empty() {
        return Err(format!("no events in {}", path.display()));
    }
    Ok(names)
}

fn event(name: &str) -> IdlEvent {
    IdlEvent {
        name: name.to_owned(),
        selector: event_selector(EVENT_NAMESPACE, name),
        type_: IdlType::Defined {
            defined: name.to_owned(),
        },
    }
}

fn error_row(row: &vesting_core::ErrorInfo) -> IdlError {
    IdlError {
        code: row.code,
        name: row.name.to_owned(),
        msg: row.message.to_owned(),
    }
}

/// Adds `more` to `types`, refusing a name described two ways; keeps `types` sorted by name, as SPEL writes it.
fn merge_types(types: &mut Vec<IdlTypeDef>, more: Vec<IdlTypeDef>) -> Result<(), String> {
    let mut by_name: BTreeMap<String, IdlTypeDef> = types.drain(..).map(|t| (t.name.clone(), t)).collect();
    for def in more {
        match by_name.get(&def.name) {
            Some(existing) if *existing != def => {
                return Err(format!("type `{}` is described two ways", def.name));
            }
            Some(_) => {}
            None => {
                by_name.insert(def.name.clone(), def);
            }
        }
    }
    types.extend(by_name.into_values());
    Ok(())
}

/// `vesting_fees::FeeConfig` and the admin slot of `lez-admin-authority` v0.2.0 (its `lez-authority` v0.2.0 types).
fn fee_config_types() -> Vec<IdlTypeDef> {
    let account = || IdlType::Primitive("account_id".to_owned());
    let optional_account = || IdlType::Option {
        option: Box::new(account()),
    };
    let field = |name: &str, type_: IdlType, docs: &str| IdlField {
        name: name.to_owned(),
        type_,
        docs: Some(docs.to_owned()),
    };
    let defined = |name: &str| IdlType::Defined {
        defined: name.to_owned(),
    };
    let variant = |name: &str, fields: Vec<IdlField>| IdlEnumVariant {
        name: name.to_owned(),
        fields,
    };
    vec![
        IdlTypeDef {
            name: "FeeConfig".to_owned(),
            kind: "struct".to_owned(),
            fields: vec![
                field(
                    "rate_bps",
                    IdlType::Primitive("u16".to_owned()),
                    "Fee rate in basis points, `0..=MAX_FEE_BPS`.",
                ),
                field(
                    "treasury",
                    account(),
                    "The treasury: the owner of the fee destination (SPEC 6).",
                ),
                field(
                    "admin",
                    defined("AuthoritySlot"),
                    "The admin slot of the RFP-001 admin authority.",
                ),
            ],
            variants: Vec::new(),
        },
        IdlTypeDef {
            name: "AuthoritySlot".to_owned(),
            kind: "enum".to_owned(),
            fields: Vec::new(),
            variants: vec![
                variant("Unset", Vec::new()),
                variant(
                    "Held",
                    vec![
                        field("holder", defined("Holder"), "The holder."),
                        field(
                            "nominee",
                            optional_account(),
                            "The account the holder nominated, until it accepts or the offer changes.",
                        ),
                    ],
                ),
                variant("Renounced", Vec::new()),
            ],
        },
        IdlTypeDef {
            name: "Holder".to_owned(),
            kind: "struct".to_owned(),
            fields: vec![
                field("account", account(), "The holder's account."),
                field(
                    "via",
                    optional_account(),
                    "The program the holder acts through, or `None` for top level.",
                ),
            ],
            variants: Vec::new(),
        },
    ]
}
