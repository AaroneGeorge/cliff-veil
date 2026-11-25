//! What the SDK answers, as the mini-app holds it: the SDK's `ScheduleView`, `ClaimSummary`, `Outcome` and `Receipt`
//! (`sdk/src/client.rs`, `sdk/src/backend.rs`) with every account id as base58. [`crate::cli`] reads them from the
//! `cliff-veil --json` output; as JSON (to the view) every `u128` is a decimal string.

use serde::{Deserialize, Serialize};

use crate::amount::dec;

/// The asset a schedule escrows (SPEC 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Asset {
    /// The native token.
    Native,
    /// A token of the LEZ token program.
    Token {
        /// The token definition account.
        definition: String,
    },
}

/// How a schedule vests (SPEC 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Cliff and linear; `cliff == start` is fully linear, `cliff == end` a single cliff.
    Linear {
        /// Start (ms).
        start: u64,
        /// Cliff (ms).
        cliff: u64,
        /// End (ms).
        end: u64,
    },
    /// Milestones signalled by the milestone authority.
    Milestones {
        /// Amount of each milestone.
        #[serde(with = "dec::vec")]
        amounts: Vec<u128>,
        /// Bit `i` set: milestone `i` is signalled.
        signalled: u64,
    },
}

/// When more becomes claimable (`vesting_core::NextUnlock`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextUnlock {
    /// At this clock time (ms).
    At(u64),
    /// When the milestone authority signals a milestone.
    WhenMilestoneSignalled,
    /// Nothing more vests.
    Never,
}

/// Active, cancelled or settled (`sdk::Standing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    /// Vesting, or vested and not fully claimed.
    Active,
    /// Cancelled; the vested remainder may still be claimable.
    Cancelled,
    /// Everything that will vest is paid out.
    Settled,
}

/// A cancellation (SPEC 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancellation {
    /// Effective time (ms).
    pub at: u64,
    /// Vested amount frozen by the cancel.
    #[serde(with = "dec")]
    pub vested_at_cancel: u128,
    /// Returned to the refund account.
    #[serde(with = "dec")]
    pub refunded: u128,
}

/// A schedule as of the last block (`sdk::ScheduleView` with its `ScheduleV1` flattened).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleStatus {
    /// The schedule account.
    pub account: String,
    /// Creator.
    pub creator: String,
    /// Current beneficiary.
    pub beneficiary: String,
    /// Asset.
    pub asset: Asset,
    /// Escrowed at creation.
    #[serde(with = "dec")]
    pub total: u128,
    /// Paid out so far.
    #[serde(with = "dec")]
    pub claimed: u128,
    /// Vested at `now` (frozen at the cancel when cancelled).
    #[serde(with = "dec")]
    pub vested: u128,
    /// Claimable now.
    #[serde(with = "dec")]
    pub claimable: u128,
    /// Not vested yet (0 once cancelled).
    #[serde(with = "dec")]
    pub locked: u128,
    /// The clock time the numbers are computed at (ms; the last block's timestamp).
    pub now: u64,
    /// When more becomes claimable.
    pub next_unlock: NextUnlock,
    /// Active, cancelled or settled.
    pub standing: Standing,
    /// Linear or milestones.
    pub kind: Kind,
    /// Can still be cancelled.
    pub cancelable: bool,
    /// The beneficiary may transfer it.
    pub transferable: bool,
    /// Who may cancel.
    pub cancel_authority: String,
    /// Who may signal milestones.
    pub milestone_authority: String,
    /// Where a cancel refunds to.
    pub refund_to: String,
    /// The cancellation, when cancelled.
    #[serde(default)]
    pub cancelled: Option<Cancellation>,
}

/// Where a prepared claim pays to (`sdk::Destination`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    /// A public account.
    Public {
        /// The account.
        account: String,
    },
    /// An owned private account, reached at a one-time blinded address.
    Private {
        /// Known to this wallet only; never in the transaction.
        account: String,
        /// The one-time address the transaction names.
        blinded: String,
        /// Created for this claim.
        fresh: bool,
    },
}

/// A checked, priced claim: `sdk::ClaimSummary`, plus the disclosure text and dev-mode flag the SDK attaches to a
/// private claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedClaim {
    /// The schedule account.
    pub schedule: String,
    /// The beneficiary that signs.
    pub beneficiary: String,
    /// What the claim pays (public: at least this).
    #[serde(with = "dec")]
    pub amount: u128,
    /// The time it is computed at (ms).
    pub at: u64,
    /// `claimed` after the claim.
    #[serde(with = "dec")]
    pub claimed_total: u128,
    /// Still to be paid out after this claim.
    #[serde(with = "dec")]
    pub remaining: u128,
    /// Where it goes.
    pub destination: Destination,
    /// Estimated network fee.
    #[serde(with = "dec")]
    pub fee: u128,
    /// Who pays the fee.
    pub fee_payer: String,
    /// What the fee payer holds when the fee is charged.
    #[serde(with = "dec")]
    pub fee_payer_holds: u128,
    /// The fee reserve (`sdk::ClaimSummary::reserve`, at least `fee`): on the private path the amount fixed inside
    /// the proof that the fee is paid from, the most the fee payer is charged; on the public path what admission
    /// holds from the signer, which is then charged the actual fee.
    #[serde(with = "dec", default)]
    pub reserve: u128,
    /// Blocks of steepest base-fee rise the reserve is priced for.
    #[serde(default)]
    pub headroom_blocks: u64,
    /// The reserve that covers `headroom_blocks` of rise.
    #[serde(with = "dec", default)]
    pub headroom_reserve: u128,
    /// Why this account pays the fee, in the SDK's words (`fee_payer_reason`).
    #[serde(default)]
    pub fee_payer_reason: Option<String>,
    /// `sdk::privacy_disclosure()`; present for a private claim.
    #[serde(default)]
    pub disclosure: Option<String>,
    /// `sdk::dev_mode()`: no real proof is generated.
    #[serde(default)]
    pub dev_mode: bool,
}

/// The predicted result of a creator or authority operation (`sdk::Outcome` without `Claim`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Schedules are created.
    Created {
        /// The schedule accounts.
        schedules: Vec<String>,
        /// Escrowed.
        #[serde(with = "dec")]
        total: u128,
        /// The protocol creation fee.
        #[serde(with = "dec")]
        protocol_fee: u128,
        /// Where the creation fee goes.
        treasury: String,
    },
    /// A cancel.
    Cancelled {
        /// Effective time (at least this).
        at: u64,
        /// Vested amount frozen by the cancel.
        #[serde(with = "dec")]
        vested_at_cancel: u128,
        /// Refunded.
        #[serde(with = "dec")]
        refunded: u128,
        /// To.
        refund_to: String,
        /// Stays claimable by the beneficiary.
        #[serde(with = "dec")]
        still_claimable: u128,
    },
    /// A milestone signal.
    Signalled {
        /// Index.
        index: u8,
        /// Amount it unlocks.
        #[serde(with = "dec")]
        amount: u128,
    },
}

/// A checked, priced creator or authority operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedOp {
    /// What it will do.
    pub outcome: Outcome,
    /// Estimated network fee.
    #[serde(with = "dec")]
    pub fee: u128,
    /// Who pays it.
    pub fee_payer: String,
}

/// An included transaction (`sdk::Receipt`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// Transaction hash.
    pub hash: String,
    /// Block.
    pub block: u64,
}

/// A refusal, from the SDK (stable `code`; `program_code` for the program's codes of SPEC 5) or from the mini-app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    /// Stable code, e.g. `nothing_claimable`, `private_fee_shortfall`, `disclosure_not_acknowledged`.
    pub code: String,
    /// The program's error code (SPEC 5), when the refusal is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_code: Option<u32>,
    /// What went wrong.
    pub message: String,
    /// What to do about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl ErrorInfo {
    /// A mini-app error.
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>, action: Option<&str>) -> Self {
        Self {
            code: code.to_owned(),
            program_code: None,
            message: message.into(),
            action: action.map(str::to_owned),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::account::tests::id;

    /// 1,000 native units, linear over 1,000 s from t = 1,000,000 ms with a cliff at 250 s, seen at 500 s.
    pub(crate) fn linear_status() -> ScheduleStatus {
        ScheduleStatus {
            account: id(1),
            creator: id(2),
            beneficiary: id(3),
            asset: Asset::Native,
            total: 1000,
            claimed: 100,
            vested: 500,
            claimable: 400,
            locked: 500,
            now: 1_500_000,
            next_unlock: NextUnlock::At(1_501_000),
            standing: Standing::Active,
            kind: Kind::Linear {
                start: 1_000_000,
                cliff: 1_250_000,
                end: 2_000_000,
            },
            cancelable: true,
            transferable: false,
            cancel_authority: id(2),
            milestone_authority: id(2),
            refund_to: id(2),
            cancelled: None,
        }
    }

    /// Three milestones (100, 200, 300), the first signalled and claimed.
    pub(crate) fn milestone_status() -> ScheduleStatus {
        ScheduleStatus {
            account: id(4),
            total: 600,
            claimed: 100,
            vested: 100,
            claimable: 0,
            locked: 500,
            next_unlock: NextUnlock::WhenMilestoneSignalled,
            kind: Kind::Milestones {
                amounts: vec![100, 200, 300],
                signalled: 0b001,
            },
            milestone_authority: id(5),
            ..linear_status()
        }
    }

    #[test]
    fn reads_the_json_the_sdk_prints() {
        let json = serde_json::to_value(linear_status()).unwrap();
        assert_eq!(json["total"], "1000");
        assert_eq!(json["asset"], "native");
        assert_eq!(json["next_unlock"]["at"], 1_501_000);
        assert_eq!(json["kind"]["linear"]["cliff"], 1_250_000);
        assert_eq!(serde_json::from_value::<ScheduleStatus>(json).unwrap(), linear_status());
        let m = serde_json::to_value(milestone_status()).unwrap();
        assert_eq!(m["next_unlock"], "when_milestone_signalled");
        assert_eq!(m["kind"]["milestones"]["amounts"][2], "300");
        let token: Asset = serde_json::from_str(r#"{"token":{"definition":"abc"}}"#).unwrap();
        assert_eq!(
            token,
            Asset::Token {
                definition: "abc".into()
            }
        );
    }
}
