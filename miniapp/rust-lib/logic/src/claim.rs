//! The claim flow: request, pre-claim summary, gas check, disclosure, confirmation (RFP Usability 4 and 5,
//! Privacy 2).
//!
//! ```text
//! request --prepare (SDK dry run)--> summary --(private: acknowledge the disclosure shown)--> confirm --> send
//! ```
//!
//! Every claim starts from a fresh prepare. A [`ClaimFlow`] confirms once; the next claim prepares again, gets the
//! disclosure again, and needs a new acknowledgement. [`ClaimFlow::confirm`] refuses while anything blocks the claim:
//! nothing to claim, a fee payer that cannot pay, or, on the private path, a disclosure that is missing, incomplete or
//! not acknowledged. The QML view mirrors these rules by disabling the button; this type is what enforces them.

use serde::{Deserialize, Serialize};

use crate::amount::display;
use crate::disclosure;
use crate::model::{Destination, ErrorInfo, PreparedClaim};
use crate::time::utc;

/// Where the claim should pay to, as chosen on screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "path")]
pub enum ClaimPath {
    /// To a public account; `to: None` is the beneficiary itself.
    Public {
        /// Destination account.
        #[serde(default)]
        to: Option<String>,
    },
    /// To a private account of this wallet; `account: None` creates a fresh one for this claim.
    Private {
        /// Destination private account.
        #[serde(default)]
        account: Option<String>,
        /// Private account that pays the network fee (`None`: the destination pays it out of a native claim).
        #[serde(default)]
        fee_payer: Option<String>,
    },
}

/// A claim the beneficiary asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRequest {
    /// The schedule account.
    pub schedule: String,
    /// The beneficiary (signer).
    pub beneficiary: String,
    /// Where to.
    #[serde(flatten)]
    pub path: ClaimPath,
}

impl ClaimRequest {
    /// `true` on the private path.
    #[must_use]
    pub const fn is_private(&self) -> bool {
        matches!(self.path, ClaimPath::Private { .. })
    }
}

/// Can the fee payer pay the network fee (RFP Usability 4)? It must hold the fee reserve, not only the estimated
/// fee: admission holds the reserve (the SDK checks the same), and a private claim's reserve is fixed in its proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum GasCheck {
    /// Yes.
    Sufficient {
        /// Who pays.
        payer: String,
        /// What it holds when the fee is charged.
        holds: String,
        /// The estimated fee.
        fee: String,
        /// The fee reserve it must hold (at least the fee).
        reserve: String,
    },
    /// No: the claim is blocked, with what to do.
    Insufficient {
        /// Who pays.
        payer: String,
        /// The fee reserve it must hold.
        need: String,
        /// What it holds.
        have: String,
        /// What to do.
        action: String,
    },
}

/// The gas check of a prepared claim. On the private path the payer is a private account of this wallet: what it
/// holds when the fee is charged is its synced balance, plus the claimed amount when it is the destination of a native
/// claim (the fee is charged after the claim, SPEC 9); the SDK computes that figure (`fee_payer_holds`).
#[must_use]
pub fn gas_check(p: &PreparedClaim, native: bool) -> GasCheck {
    let need = p.reserve.max(p.fee);
    if p.fee_payer_holds >= need {
        return GasCheck::Sufficient {
            payer: p.fee_payer.clone(),
            holds: display(p.fee_payer_holds),
            fee: display(p.fee),
            reserve: display(need),
        };
    }
    let action = match (&p.destination, native) {
        (Destination::Public { .. }, _) => format!(
            "send at least {} native units to the beneficiary account {} and review the claim again",
            display(need - p.fee_payer_holds),
            p.fee_payer
        ),
        (Destination::Private { .. }, true) => {
            "claim later, when more has vested, or choose a funded private account of this wallet as fee payer"
                .to_owned()
        }
        (Destination::Private { .. }, false) => format!(
            "a token claim brings no native balance: fund a private account of this wallet with at least {} native \
             units and choose it as fee payer",
            display(need)
        ),
    };
    GasCheck::Insufficient {
        payer: p.fee_payer.clone(),
        need: display(need),
        have: display(p.fee_payer_holds),
        action,
    }
}

/// One labelled line of the pre-claim summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    /// Label.
    pub label: String,
    /// Value.
    pub value: String,
}

/// The pre-claim confirmation summary (RFP Usability 4): what is claimed, where it goes, the estimated fee and who
/// pays it, and the gas check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// `"public"` or `"private"`.
    pub path: String,
    /// Claimable amount (display text).
    pub claimable: String,
    /// Estimated network fee (display text).
    pub fee: String,
    /// The lines, in order.
    pub lines: Vec<Line>,
    /// The gas check.
    pub gas: GasCheck,
    /// Shown next to a private claim when proofs are not generated.
    pub dev_mode_note: Option<String>,
}

/// Shown when the SDK reports dev mode (the same words as the SDK's `DEV_MODE_NOTE`).
pub const DEV_MODE_NOTE: &str = "dev mode: RISC0_DEV_MODE=1, the privacy-preserving transaction carries no real \
proof; on a network that verifies proofs the claim is proved locally first, which takes minutes";

impl Summary {
    /// The summary of `p`.
    #[must_use]
    pub fn new(p: &PreparedClaim, native: bool) -> Self {
        let l = |label: &str, value: String| Line {
            label: label.to_owned(),
            value,
        };
        let (path, claimable, to) = match &p.destination {
            Destination::Public { account } => (
                "public",
                format!(
                    "at least {} (computed at the clock time of the including block)",
                    display(p.amount)
                ),
                format!("{account} (public account)"),
            ),
            Destination::Private {
                account,
                blinded,
                fresh,
            } => (
                "private",
                format!("{} (computed at {})", display(p.amount), utc(p.at)),
                format!(
                    "{account} ({}private account of this wallet; the transaction names only the one-time address \
                     {blinded})",
                    if *fresh { "new " } else { "" }
                ),
            ),
        };
        let mut lines = vec![
            l("Schedule", p.schedule.clone()),
            l("Beneficiary", p.beneficiary.clone()),
            l("Claimable now", claimable.clone()),
            l("Destination", to),
            l("Claimed after this claim", display(p.claimed_total)),
            l("Still to come", display(p.remaining)),
            l(
                "Estimated network fee",
                format!("~{} (dry run at current state)", display(p.fee)),
            ),
        ];
        if let Some(reserve) = reserve_line(p) {
            lines.push(l("Fee reserve (max charged)", reserve));
        }
        lines.push(l(
            "Fee paid by",
            match &p.fee_payer_reason {
                Some(why) => format!(
                    "{}, which will hold {} ({why})",
                    p.fee_payer,
                    display(p.fee_payer_holds)
                ),
                None => format!("{}, which will hold {}", p.fee_payer, display(p.fee_payer_holds)),
            },
        ));
        Self {
            path: path.to_owned(),
            claimable,
            fee: display(p.fee),
            lines,
            gas: gas_check(p, native),
            dev_mode_note: (p.dev_mode && path == "private").then(|| DEV_MODE_NOTE.to_owned()),
        }
    }
}

/// The fee reserve line: always on the private path (the reserve is fixed in the proof and is what the fee payer
/// is charged at most, typically the whole of it); on the public path only when it differs from the fee (the
/// beneficiary must hold it; the actual fee is charged).
fn reserve_line(p: &PreparedClaim) -> Option<String> {
    let reserve = p.reserve.max(p.fee);
    let private = matches!(p.destination, Destination::Private { .. });
    if !private && reserve == p.fee {
        return None;
    }
    let mut v = if private {
        format!(
            "{}, fixed in the proof; estimated fee ~{}",
            display(reserve),
            display(p.fee)
        )
    } else {
        format!(
            "{} held from the beneficiary at admission; charged: the actual fee, ~{}",
            display(reserve),
            display(p.fee)
        )
    };
    if p.headroom_blocks > 0 {
        if reserve >= p.headroom_reserve {
            v.push_str(&format!(
                "; covers {} blocks of base-fee rise before inclusion",
                p.headroom_blocks
            ));
        } else {
            v.push_str(&format!(
                "; below the {} that covers {} blocks of base-fee rise: if blocks are busy the claim can be dropped \
                 (nothing is consumed; review it again)",
                display(p.headroom_reserve),
                p.headroom_blocks
            ));
        }
    }
    Some(v)
}

/// Where a flow is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Summary shown; a private claim waits for the disclosure to be acknowledged.
    Review,
    /// Everything needed is in place.
    Ready,
    /// Confirmed once; this flow is spent.
    Confirmed,
}

/// One claim, from its summary to its confirmation.
#[derive(Debug, Clone)]
pub struct ClaimFlow {
    request: ClaimRequest,
    prepared: PreparedClaim,
    summary: Summary,
    acknowledged: bool,
    confirmed: bool,
}

impl ClaimFlow {
    /// A flow for `request`, from the SDK's prepared claim. `native`: the schedule escrows the native token.
    ///
    /// # Errors
    /// `destination_mismatch` when the SDK prepared a claim on the other path; on the private path
    /// `disclosure_missing` (the SDK sent no disclosure) or `disclosure_incomplete` ([`disclosure::check`]).
    pub fn new(request: ClaimRequest, prepared: PreparedClaim, native: bool) -> Result<Self, ErrorInfo> {
        let private = matches!(prepared.destination, Destination::Private { .. });
        if private != request.is_private() {
            return Err(ErrorInfo::new(
                "destination_mismatch",
                "the prepared claim does not go where you asked; nothing was sent",
                Some("review the claim again"),
            ));
        }
        if private {
            let text = prepared.disclosure.as_deref().ok_or_else(|| {
                ErrorInfo::new(
                    "disclosure_missing",
                    "no privacy disclosure came with this private claim; the claim is blocked",
                    Some("update the cliff-veil command line to the version this mini-app was built with"),
                )
            })?;
            disclosure::check(text)?;
        }
        let summary = Summary::new(&prepared, native);
        Ok(Self {
            request,
            prepared,
            summary,
            acknowledged: false,
            confirmed: false,
        })
    }

    /// The request.
    #[must_use]
    pub const fn request(&self) -> &ClaimRequest {
        &self.request
    }

    /// The pre-claim summary.
    #[must_use]
    pub const fn summary(&self) -> &Summary {
        &self.summary
    }

    /// The disclosure to show (private path only).
    #[must_use]
    pub fn disclosure(&self) -> Option<&str> {
        if self.request.is_private() {
            self.prepared.disclosure.as_deref()
        } else {
            None
        }
    }

    /// Records that the person acknowledged the disclosure shown; `digest` is [`disclosure::digest`] of the text on
    /// screen.
    ///
    /// # Errors
    /// `no_disclosure` on the public path; `disclosure_changed` when `digest` is not that of this claim's disclosure;
    /// `claim_already_confirmed` once confirmed.
    pub fn acknowledge(&mut self, digest: &str) -> Result<(), ErrorInfo> {
        if self.confirmed {
            return Err(spent());
        }
        let text = self.disclosure().ok_or_else(|| {
            ErrorInfo::new(
                "no_disclosure",
                "a claim to a public account has no privacy disclosure",
                None,
            )
        })?;
        if disclosure::digest(text) != digest {
            return Err(ErrorInfo::new(
                "disclosure_changed",
                "the acknowledgement is not for the disclosure of this claim",
                Some("read the disclosure shown for this claim and acknowledge it"),
            ));
        }
        self.acknowledged = true;
        Ok(())
    }

    /// Why the claim cannot be confirmed now, if anything blocks it (first reason).
    #[must_use]
    pub fn blocked(&self) -> Option<ErrorInfo> {
        if self.confirmed {
            return Some(spent());
        }
        if self.prepared.amount == 0 {
            return Some(ErrorInfo::new("nothing_claimable", "nothing is claimable now", None));
        }
        if let GasCheck::Insufficient {
            payer,
            need,
            have,
            action,
        } = &self.summary.gas
        {
            let code = if self.request.is_private() {
                "private_fee_shortfall"
            } else {
                "insufficient_balance"
            };
            return Some(ErrorInfo::new(
                code,
                format!("the fee payer {payer} holds {have}, and the network fee is about {need}"),
                Some(action),
            ));
        }
        if self.request.is_private() && !self.acknowledged {
            return Some(ErrorInfo::new(
                "disclosure_not_acknowledged",
                "a claim to a private account is sent only after its privacy disclosure is acknowledged",
                Some("read the privacy disclosure for this claim and acknowledge it"),
            ));
        }
        None
    }

    /// Where the flow is.
    #[must_use]
    pub fn stage(&self) -> Stage {
        if self.confirmed {
            Stage::Confirmed
        } else if self.blocked().is_none() {
            Stage::Ready
        } else {
            Stage::Review
        }
    }

    /// Confirms the claim: returns the request to send, and spends the flow.
    ///
    /// The command line checks and builds the claim again when it sends it. So that it pays where the summary said,
    /// a private claim that was reviewed with a fresh destination is sent to that same private account (created by
    /// the dry run in this wallet), not to another fresh one.
    ///
    /// # Errors
    /// [`Self::blocked`]'s reason.
    pub fn confirm(&mut self) -> Result<ClaimRequest, ErrorInfo> {
        if let Some(e) = self.blocked() {
            return Err(e);
        }
        self.confirmed = true;
        let mut request = self.request.clone();
        if let (ClaimPath::Private { account, .. }, Destination::Private { account: reviewed, .. }) =
            (&mut request.path, &self.prepared.destination)
        {
            *account = Some(reviewed.clone());
        }
        Ok(request)
    }
}

fn spent() -> ErrorInfo {
    ErrorInfo::new(
        "claim_already_confirmed",
        "this claim was already confirmed",
        Some("review a new claim; each claim shows its own summary and disclosure"),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::account::tests::id;
    use crate::disclosure::tests::sdk_text;

    pub(crate) fn private_request() -> ClaimRequest {
        ClaimRequest {
            schedule: id(1),
            beneficiary: id(3),
            path: ClaimPath::Private {
                account: None,
                fee_payer: None,
            },
        }
    }

    pub(crate) fn public_request() -> ClaimRequest {
        ClaimRequest {
            path: ClaimPath::Public { to: None },
            ..private_request()
        }
    }

    pub(crate) fn prepared_private() -> PreparedClaim {
        PreparedClaim {
            schedule: id(1),
            beneficiary: id(3),
            amount: 400,
            at: 1_500_000,
            claimed_total: 500,
            remaining: 500,
            destination: Destination::Private {
                account: id(8),
                blinded: id(9),
                fresh: true,
            },
            fee: 30,
            fee_payer: id(8),
            fee_payer_holds: 400,
            reserve: 30,
            headroom_blocks: 0,
            headroom_reserve: 30,
            fee_payer_reason: None,
            disclosure: Some(sdk_text()),
            dev_mode: true,
        }
    }

    pub(crate) fn prepared_public() -> PreparedClaim {
        PreparedClaim {
            destination: Destination::Public { account: id(3) },
            fee_payer: id(3),
            fee_payer_holds: 50,
            disclosure: None,
            dev_mode: false,
            ..prepared_private()
        }
    }

    fn value<'a>(s: &'a Summary, label: &str) -> &'a str {
        &s.lines
            .iter()
            .find(|l| l.label == label)
            .unwrap_or_else(|| panic!("no line {label}"))
            .value
    }

    #[test]
    fn u4_summary_shows_claimable_amount_and_estimated_fee() {
        let s = Summary::new(&prepared_public(), true);
        assert_eq!(s.path, "public");
        assert_eq!(s.fee, "30");
        assert!(value(&s, "Claimable now").starts_with("at least 400"));
        assert_eq!(value(&s, "Estimated network fee"), "~30 (dry run at current state)");
        assert!(value(&s, "Fee paid by").contains(&id(3)));
        assert_eq!(s.dev_mode_note, None);
        assert!(matches!(s.gas, GasCheck::Sufficient { .. }));
    }

    #[test]
    fn u4_summary_of_a_private_claim_names_the_time_and_one_time_address() {
        let s = Summary::new(&prepared_private(), true);
        assert_eq!(s.path, "private");
        assert_eq!(value(&s, "Claimable now"), "400 (computed at 1970-01-01T00:25:00.000Z)");
        assert!(value(&s, "Destination").contains("new private account"));
        assert!(value(&s, "Destination").contains(&id(9)));
        assert_eq!(s.dev_mode_note.as_deref(), Some(DEV_MODE_NOTE));
    }

    #[test]
    fn u4_private_gas_check_passes_when_the_payer_covers_the_fee() {
        let p = prepared_private();
        assert_eq!(
            gas_check(&p, true),
            GasCheck::Sufficient {
                payer: id(8),
                holds: "400".into(),
                fee: "30".into(),
                reserve: "30".into()
            }
        );
    }

    #[test]
    fn u4_private_gas_shortfall_blocks_with_an_actionable_error() {
        let p = PreparedClaim {
            fee_payer_holds: 10,
            ..prepared_private()
        };
        let GasCheck::Insufficient { need, have, action, .. } = gas_check(&p, true) else {
            panic!("must be short")
        };
        assert_eq!((need.as_str(), have.as_str()), ("30", "10"));
        assert!(action.contains("funded private account"), "{action}");
        // Token claim: the action says to fund a fee payer with native balance.
        let GasCheck::Insufficient { action, .. } = gas_check(&p, false) else {
            panic!()
        };
        assert!(
            action.contains("token claim brings no native balance") && action.contains("30"),
            "{action}"
        );
        // The flow refuses to confirm, even with the disclosure acknowledged.
        let mut f = ClaimFlow::new(private_request(), p, true).unwrap();
        f.acknowledge(&disclosure::digest(&sdk_text())).unwrap();
        let e = f.confirm().unwrap_err();
        assert_eq!(e.code, "private_fee_shortfall");
        assert!(e.message.contains(&id(8)) && e.action.is_some());
    }

    /// A private claim as the SDK prices it on local-dev: the fee payer is another private account of the wallet,
    /// chosen by the SDK, and the reserve fixed in the proof (what is charged) is about three times the fee.
    pub(crate) fn prepared_private_reserve() -> PreparedClaim {
        PreparedClaim {
            amount: 300_000_000,
            destination: Destination::Private {
                account: id(8),
                blinded: id(9),
                fresh: true,
            },
            fee: 5_529_344,
            fee_payer: id(7),
            fee_payer_holds: 42_762_634,
            reserve: 16_588_032,
            headroom_blocks: 12,
            headroom_reserve: 16_588_032,
            fee_payer_reason: Some(
                "chosen by the SDK: the private account of this wallet with the smallest balance (42762634) that covers \
                 the reserve for 12 blocks of base-fee rise (16588032)"
                    .into(),
            ),
            ..prepared_private()
        }
    }

    #[test]
    fn u4_private_summary_shows_the_fee_reserve_next_to_the_estimated_fee() {
        let s = Summary::new(&prepared_private_reserve(), true);
        assert_eq!(
            value(&s, "Estimated network fee"),
            "~5,529,344 (dry run at current state)"
        );
        assert_eq!(
            value(&s, "Fee reserve (max charged)"),
            "16,588,032, fixed in the proof; estimated fee ~5,529,344; covers 12 blocks of base-fee rise before \
             inclusion"
        );
        // The reserve line comes right after the estimated fee, before who pays.
        let labels: Vec<&str> = s.lines.iter().map(|l| l.label.as_str()).collect();
        let at = |x: &str| labels.iter().position(|l| *l == x).unwrap();
        assert_eq!(at("Fee reserve (max charged)"), at("Estimated network fee") + 1);
        assert_eq!(at("Fee paid by"), at("Fee reserve (max charged)") + 1);
        assert!(
            value(&s, "Fee paid by").contains("chosen by the SDK"),
            "{}",
            value(&s, "Fee paid by")
        );
        assert_eq!(
            s.gas,
            GasCheck::Sufficient {
                payer: id(7),
                holds: "42,762,634".into(),
                fee: "5,529,344".into(),
                reserve: "16,588,032".into()
            }
        );
    }

    #[test]
    fn u4_private_reserve_below_the_headroom_is_flagged() {
        let p = PreparedClaim {
            reserve: 9_000_000,
            ..prepared_private_reserve()
        };
        let v = Summary::new(&p, true)
            .lines
            .into_iter()
            .find(|l| l.label == "Fee reserve (max charged)")
            .unwrap();
        assert!(
            v.value.contains("below the 16,588,032 that covers 12 blocks"),
            "{}",
            v.value
        );
    }

    #[test]
    fn u4_gas_check_needs_the_reserve_not_only_the_fee() {
        // Holds more than the estimated fee but less than the reserve fixed in the proof: blocked.
        let p = PreparedClaim {
            fee_payer_holds: 10_000_000,
            ..prepared_private_reserve()
        };
        let GasCheck::Insufficient { need, have, .. } = gas_check(&p, true) else {
            panic!("must be short")
        };
        assert_eq!((need.as_str(), have.as_str()), ("16,588,032", "10,000,000"));
        // Public: the beneficiary must hold the reserve admission holds; the summary says what is charged.
        let p = PreparedClaim {
            reserve: 40,
            ..prepared_public()
        };
        let s = Summary::new(&p, true);
        assert!(matches!(s.gas, GasCheck::Sufficient { ref reserve, .. } if reserve == "40"));
        assert!(value(&s, "Fee reserve (max charged)").contains("charged: the actual fee, ~30"));
        let short = PreparedClaim {
            fee_payer_holds: 35,
            ..p
        };
        let mut f = ClaimFlow::new(public_request(), short, true).unwrap();
        assert!(f.confirm().unwrap_err().action.unwrap().contains("send at least 5"));
        // Public with reserve equal to the fee: no reserve line.
        assert!(!Summary::new(&prepared_public(), true)
            .lines
            .iter()
            .any(|l| l.label.starts_with("Fee reserve")));
    }

    #[test]
    fn u4_public_fee_shortfall_names_the_amount_to_send() {
        let p = PreparedClaim {
            fee_payer_holds: 5,
            ..prepared_public()
        };
        let mut f = ClaimFlow::new(public_request(), p, true).unwrap();
        let e = f.confirm().unwrap_err();
        assert_eq!(e.code, "insufficient_balance");
        assert!(e.action.unwrap().contains("send at least 25"));
    }

    #[test]
    fn u5_blocks_until_ack() {
        let mut f = ClaimFlow::new(private_request(), prepared_private(), true).unwrap();
        assert_eq!(f.stage(), Stage::Review);
        assert_eq!(f.confirm().unwrap_err().code, "disclosure_not_acknowledged");
        f.acknowledge(&disclosure::digest(f.disclosure().unwrap())).unwrap();
        assert_eq!(f.stage(), Stage::Ready);
        let sent = f.confirm().unwrap();
        assert_eq!(f.stage(), Stage::Confirmed);
        // Sent to the private account the summary named, not to another fresh one.
        assert_eq!(
            sent.path,
            ClaimPath::Private {
                account: Some(id(8)),
                fee_payer: None
            }
        );
    }

    #[test]
    fn u5_ack_must_match_the_text_shown() {
        let mut f = ClaimFlow::new(private_request(), prepared_private(), true).unwrap();
        assert_eq!(
            f.acknowledge(&disclosure::digest("some other text")).unwrap_err().code,
            "disclosure_changed"
        );
        assert_eq!(f.confirm().unwrap_err().code, "disclosure_not_acknowledged");
    }

    #[test]
    fn u5_each_claim_needs_its_own_ack() {
        let mut first = ClaimFlow::new(private_request(), prepared_private(), true).unwrap();
        first.acknowledge(&disclosure::digest(&sdk_text())).unwrap();
        first.confirm().unwrap();
        // The spent flow cannot be confirmed or acknowledged again.
        assert_eq!(first.confirm().unwrap_err().code, "claim_already_confirmed");
        assert_eq!(
            first.acknowledge(&disclosure::digest(&sdk_text())).unwrap_err().code,
            "claim_already_confirmed"
        );
        // The next claim starts unacknowledged.
        let second = ClaimFlow::new(private_request(), prepared_private(), true).unwrap();
        assert_eq!(second.blocked().unwrap().code, "disclosure_not_acknowledged");
    }

    #[test]
    fn u5_private_claim_without_a_complete_disclosure_is_refused() {
        let none = PreparedClaim {
            disclosure: None,
            ..prepared_private()
        };
        assert_eq!(
            ClaimFlow::new(private_request(), none, true).unwrap_err().code,
            "disclosure_missing"
        );
        let cut = PreparedClaim {
            disclosure: Some(sdk_text().replace("the claim amount", "")),
            ..prepared_private()
        };
        assert_eq!(
            ClaimFlow::new(private_request(), cut, true).unwrap_err().code,
            "disclosure_incomplete"
        );
    }

    #[test]
    fn u5_public_claim_shows_no_disclosure_and_needs_no_ack() {
        let mut f = ClaimFlow::new(public_request(), prepared_public(), true).unwrap();
        assert_eq!(f.disclosure(), None);
        assert_eq!(f.acknowledge("x").unwrap_err().code, "no_disclosure");
        assert!(f.confirm().is_ok());
    }

    #[test]
    fn u5_prepared_path_must_match_the_request() {
        assert_eq!(
            ClaimFlow::new(private_request(), prepared_public(), true)
                .unwrap_err()
                .code,
            "destination_mismatch"
        );
    }

    #[test]
    fn u4_nothing_claimable_blocks() {
        let p = PreparedClaim {
            amount: 0,
            ..prepared_public()
        };
        assert_eq!(
            ClaimFlow::new(public_request(), p, true)
                .unwrap()
                .confirm()
                .unwrap_err()
                .code,
            "nothing_claimable"
        );
    }
}
