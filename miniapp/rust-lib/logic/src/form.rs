//! The create form (RFP Usability 2, creator view: "create a new schedule (all parameters)").
//!
//! Every parameter of `Create` (SPEC 3.1): beneficiary, asset, total, linear times or milestone amounts, cancelable,
//! transferable, cancel authority, milestone authority, refund account, and an optional label that names the
//! schedule id (`SHA-256(label)`, SPEC 1.2; a random id when empty). The form reports every field error at once, with
//! the program's code where the program has one (SPEC 5: 10 to 13). The SDK checks the terms again with
//! `vesting_core` and dry-runs the transaction before anything is signed.

use serde::{Deserialize, Serialize};

use crate::account;
use crate::amount::{self, display};
use crate::model::{Asset, Kind};
use crate::time::{parse_utc, utc};

/// At most this many milestones (SPEC 5, code 13).
pub const MAX_MILESTONES: usize = 64;

/// The form as the view sends it: every field as typed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CreateForm {
    /// The creator (signs and funds).
    pub creator: String,
    /// Who may claim.
    pub beneficiary: String,
    /// `native` or `token`.
    pub asset: String,
    /// The token definition, for a token schedule.
    pub token_definition: String,
    /// `linear` or `milestones`.
    pub kind: String,
    /// Total in base units (linear; for milestones, optional and then checked against the sum).
    pub total: String,
    /// Linear start, cliff, end: ISO-8601 UTC or ms.
    pub start: String,
    /// Cliff.
    pub cliff: String,
    /// End.
    pub end: String,
    /// Milestone amounts, separated by `;` or new lines.
    pub milestones: String,
    /// Cancelable (default on).
    pub cancelable: Option<bool>,
    /// Transferable (default off).
    pub transferable: Option<bool>,
    /// Cancel authority (empty: the creator).
    pub cancel_authority: String,
    /// Milestone authority (empty: the creator).
    pub milestone_authority: String,
    /// Refund account (empty: the creator, who funds the escrow).
    pub refund_to: String,
    /// Label naming the schedule id (empty: random id).
    pub label: String,
}

/// A checked create request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateRequest {
    /// Creator.
    pub creator: String,
    /// Beneficiary.
    pub beneficiary: String,
    /// Asset.
    pub asset: Asset,
    /// Total.
    #[serde(with = "amount::dec")]
    pub total: u128,
    /// Curve.
    pub kind: Kind,
    /// Cancelable.
    pub cancelable: bool,
    /// Transferable.
    pub transferable: bool,
    /// Cancel authority (`None`: the creator).
    pub cancel_authority: Option<String>,
    /// Milestone authority (`None`: the creator).
    pub milestone_authority: Option<String>,
    /// Refund account (`None`: the creator).
    pub refund_to: Option<String>,
    /// Label for the schedule id (`None`: random).
    pub label: Option<String>,
    /// The schedule id as 64 hex characters, chosen by the mini-app when there is no label, so the dry run and the
    /// send create the same schedule account.
    #[serde(default)]
    pub id: Option<String>,
}

/// A field the form refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    /// The form field.
    pub field: String,
    /// The program's error code for the same refusal, when it has one.
    pub program_code: Option<u32>,
    /// What is wrong and what to enter.
    pub message: String,
}

fn err(field: &str, program_code: Option<u32>, message: impl Into<String>) -> FieldError {
    FieldError {
        field: field.to_owned(),
        program_code,
        message: message.into(),
    }
}

/// Checks `form`; returns the request, or every field error.
///
/// # Errors
/// One [`FieldError`] per refused field.
pub fn validate(form: &CreateForm) -> Result<CreateRequest, Vec<FieldError>> {
    let mut errors = Vec::new();
    let mut acct = |field: &str, text: &str, optional: bool| -> Option<String> {
        if optional && text.trim().is_empty() {
            return None;
        }
        account::parse(text).map_err(|m| errors.push(err(field, None, m))).ok()
    };
    let creator = acct("creator", &form.creator, false);
    let beneficiary = acct("beneficiary", &form.beneficiary, false);
    let cancel_authority = acct("cancel_authority", &form.cancel_authority, true);
    let milestone_authority = acct("milestone_authority", &form.milestone_authority, true);
    let refund_to = acct("refund_to", &form.refund_to, true);
    let definition = (form.asset == "token").then(|| acct("token_definition", &form.token_definition, false));

    let asset = match (form.asset.as_str(), definition) {
        ("native" | "", _) => Some(Asset::Native),
        ("token", Some(Some(definition))) => Some(Asset::Token { definition }),
        ("token", _) => None,
        (other, _) => {
            errors.push(err(
                "asset",
                None,
                format!("unknown asset `{other}`: choose native or token"),
            ));
            None
        }
    };

    let typed_total = if form.total.trim().is_empty() && form.kind == "milestones" {
        None
    } else {
        match amount::parse(&form.total) {
            Ok(0) => {
                errors.push(err("total", Some(10), "the total must be greater than zero"));
                None
            }
            Ok(t) => Some(t),
            Err(e) => {
                errors.push(err("total", None, e.to_string()));
                None
            }
        }
    };

    let (kind, total) = match form.kind.as_str() {
        "linear" | "" => {
            let mut time = |field: &str, text: &str| parse_utc(text).map_err(|m| errors.push(err(field, None, m))).ok();
            let (s, c, e) = (
                time("start", &form.start),
                time("cliff", &form.cliff),
                time("end", &form.end),
            );
            match (s, c, e) {
                (Some(start), Some(cliff), Some(end)) => {
                    if start <= cliff && cliff <= end && start < end {
                        (Some(Kind::Linear { start, cliff, end }), typed_total)
                    } else {
                        errors.push(err(
                            "cliff",
                            Some(11),
                            "times must satisfy start <= cliff <= end and start < end",
                        ));
                        (None, typed_total)
                    }
                }
                _ => (None, typed_total),
            }
        }
        "milestones" => {
            let parts: Vec<&str> = form
                .milestones
                .split([';', '\n'])
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect();
            let mut amounts = Vec::new();
            for (i, p) in parts.iter().enumerate() {
                match amount::parse(p) {
                    Ok(0) => errors.push(err("milestones", Some(12), format!("milestone #{i} is zero"))),
                    Ok(a) => amounts.push(a),
                    Err(e) => errors.push(err("milestones", None, format!("milestone #{i}: {e}"))),
                }
            }
            if parts.is_empty() {
                errors.push(err(
                    "milestones",
                    Some(12),
                    "enter at least one milestone amount (separate with `;`)",
                ));
                (None, typed_total)
            } else if parts.len() > MAX_MILESTONES {
                errors.push(err(
                    "milestones",
                    Some(13),
                    format!("at most {MAX_MILESTONES} milestones"),
                ));
                (None, typed_total)
            } else if amounts.len() == parts.len() {
                match amounts.iter().try_fold(0u128, |s, a| s.checked_add(*a)) {
                    None => {
                        errors.push(err("milestones", Some(29), "the milestone amounts overflow"));
                        (None, typed_total)
                    }
                    Some(sum) if typed_total.is_some_and(|t| t != sum) => {
                        errors.push(err(
                            "total",
                            Some(12),
                            format!(
                                "the milestones sum to {}; leave the total empty or enter that sum",
                                display(sum)
                            ),
                        ));
                        (None, typed_total)
                    }
                    Some(sum) => (Some(Kind::Milestones { amounts, signalled: 0 }), Some(sum)),
                }
            } else {
                (None, typed_total)
            }
        }
        other => {
            errors.push(err(
                "kind",
                None,
                format!("unknown schedule kind `{other}`: choose linear or milestones"),
            ));
            (None, typed_total)
        }
    };

    let label = form.label.trim();
    match (creator, beneficiary, asset, kind, total) {
        (Some(creator), Some(beneficiary), Some(asset), Some(kind), Some(total)) if errors.is_empty() => {
            Ok(CreateRequest {
                creator,
                beneficiary,
                asset,
                total,
                kind,
                cancelable: form.cancelable.unwrap_or(true),
                transferable: form.transferable.unwrap_or(false),
                cancel_authority,
                milestone_authority,
                refund_to,
                label: (!label.is_empty()).then(|| label.to_owned()),
                id: None,
            })
        }
        _ => Err(errors),
    }
}

impl CreateRequest {
    /// The request as review lines (shown before the dry run is asked for).
    #[must_use]
    pub fn lines(&self) -> Vec<(String, String)> {
        let or_creator = |a: &Option<String>| a.clone().unwrap_or_else(|| format!("{} (the creator)", self.creator));
        let mut lines = vec![
            ("Creator".to_owned(), self.creator.clone()),
            ("Beneficiary".to_owned(), self.beneficiary.clone()),
            ("Asset".to_owned(), crate::views::asset_label(&self.asset)),
            ("Total".to_owned(), display(self.total)),
        ];
        match &self.kind {
            Kind::Linear { start, cliff, end } => {
                lines.push(("Start".to_owned(), utc(*start)));
                lines.push(("Cliff".to_owned(), utc(*cliff)));
                lines.push(("End".to_owned(), utc(*end)));
            }
            Kind::Milestones { amounts, .. } => lines.push((
                "Milestones".to_owned(),
                amounts.iter().map(|a| display(*a)).collect::<Vec<_>>().join("; "),
            )),
        }
        lines.push((
            "Cancelable".to_owned(),
            if self.cancelable { "yes" } else { "no" }.to_owned(),
        ));
        lines.push((
            "Transferable".to_owned(),
            if self.transferable { "yes" } else { "no" }.to_owned(),
        ));
        lines.push(("Cancel authority".to_owned(), or_creator(&self.cancel_authority)));
        lines.push(("Milestone authority".to_owned(), or_creator(&self.milestone_authority)));
        lines.push(("Refund account".to_owned(), or_creator(&self.refund_to)));
        lines.push((
            "Schedule id".to_owned(),
            match (&self.label, &self.id) {
                (Some(l), _) => format!("SHA-256 of label `{l}`"),
                (None, Some(id)) => format!("random: {id}"),
                (None, None) => "random".to_owned(),
            },
        ));
        lines
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::account::tests::id;

    pub(crate) fn linear_form() -> CreateForm {
        CreateForm {
            creator: id(2),
            beneficiary: id(3),
            asset: "native".into(),
            kind: "linear".into(),
            total: "1_000".into(),
            start: "2026-01-01".into(),
            cliff: "2026-04-01T00:00:00Z".into(),
            end: "2027-01-01T00:00:00.000Z".into(),
            ..CreateForm::default()
        }
    }

    #[test]
    fn u2_create_form_takes_every_parameter() {
        let form = CreateForm {
            asset: "token".into(),
            token_definition: id(6),
            cancelable: Some(false),
            transferable: Some(true),
            cancel_authority: id(7),
            milestone_authority: format!("Public/{}", id(8)),
            refund_to: id(9),
            label: " team-2026 ".into(),
            ..linear_form()
        };
        let r = validate(&form).unwrap();
        assert_eq!(r.asset, Asset::Token { definition: id(6) });
        assert_eq!(r.total, 1000);
        assert_eq!(
            r.kind,
            Kind::Linear {
                start: 1_767_225_600_000,
                cliff: 1_775_001_600_000,
                end: 1_798_761_600_000
            }
        );
        assert!(!r.cancelable && r.transferable);
        assert_eq!(r.cancel_authority, Some(id(7)));
        assert_eq!(r.milestone_authority, Some(id(8)));
        assert_eq!(r.refund_to, Some(id(9)));
        assert_eq!(r.label.as_deref(), Some("team-2026"));
        let lines = r.lines();
        assert!(lines.contains(&("Cliff".to_owned(), "2026-04-01T00:00:00.000Z".to_owned())));
        assert!(lines.contains(&("Schedule id".to_owned(), "SHA-256 of label `team-2026`".to_owned())));
    }

    #[test]
    fn u2_create_form_defaults_match_the_rfp() {
        let r = validate(&linear_form()).unwrap();
        assert!(r.cancelable && !r.transferable);
        assert_eq!(
            (r.cancel_authority, r.milestone_authority, r.refund_to, r.label),
            (None, None, None, None)
        );
        assert!(validate(&linear_form())
            .unwrap()
            .lines()
            .iter()
            .any(|(k, v)| k == "Refund account" && v.ends_with("(the creator)")));
    }

    #[test]
    fn u2_create_form_milestones_total_is_the_sum() {
        let form = CreateForm {
            kind: "milestones".into(),
            total: String::new(),
            milestones: "100; 200\n300".into(),
            ..linear_form()
        };
        let r = validate(&form).unwrap();
        assert_eq!(r.total, 600);
        assert_eq!(
            r.kind,
            Kind::Milestones {
                amounts: vec![100, 200, 300],
                signalled: 0
            }
        );
        let wrong = CreateForm {
            total: "500".into(),
            ..form
        };
        let e = validate(&wrong).unwrap_err();
        assert_eq!((e[0].field.as_str(), e[0].program_code), ("total", Some(12)));
    }

    #[test]
    fn u2_create_form_refuses_with_the_program_codes() {
        let code = |f: CreateForm| {
            validate(&f)
                .unwrap_err()
                .iter()
                .filter_map(|e| e.program_code)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            code(CreateForm {
                total: "0".into(),
                ..linear_form()
            }),
            [10]
        );
        assert_eq!(
            code(CreateForm {
                cliff: "2028-01-01".into(),
                ..linear_form()
            }),
            [11]
        );
        assert_eq!(
            code(CreateForm {
                start: "2027-01-01".into(),
                cliff: "2027-01-01".into(),
                ..linear_form()
            }),
            [11]
        );
        let ms = |m: &str| CreateForm {
            kind: "milestones".into(),
            total: String::new(),
            milestones: m.into(),
            ..linear_form()
        };
        assert_eq!(code(ms("")), [12]);
        assert_eq!(code(ms("5; 0")), [12]);
        assert_eq!(code(ms(&vec!["1"; 65].join(";"))), [13]);
    }

    #[test]
    fn u2_create_form_reports_every_bad_field_at_once() {
        let form = CreateForm {
            beneficiary: "not an id".into(),
            asset: "token".into(),
            token_definition: String::new(),
            end: "never".into(),
            refund_to: "x".into(),
            ..linear_form()
        };
        let fields: Vec<String> = validate(&form).unwrap_err().into_iter().map(|e| e.field).collect();
        assert_eq!(fields, ["beneficiary", "refund_to", "token_definition", "end"]);
    }
}
