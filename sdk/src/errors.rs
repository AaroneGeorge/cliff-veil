//! Errors of the SDK, with a message and an action for every program error code (SPEC section 5, RFP Usability 7).
//!
//! A refusal reaches the SDK in one of three ways, and all three end in the same [`Error`]:
//! - the SDK's own pre-flight, which runs the `vesting_core` rules on current state before anything is built
//!   ([`Error::from_core`]);
//! - the wallet's simulation of the built transaction against current state ([`Error::from_failure_text`] on the
//!   revert reason);
//! - the wallet's local proof run of a privacy-preserving transaction (`TransactionBuildError`, same parser).
//!
//! On LEZ rc3 the sequencer never returns a program's refusal text: a transaction sent anyway is included as
//! reverted, with no reason ([`Error::Reverted`]). So the SDK never sends a transaction whose local run reverts
//! (it never sets the wallet's `allow_revert`), and checks the one thing a local run does not: a `ClaimAsOf`'s
//! validity window ([`crate::Client::send`]).
//!
//! The program refuses with the text `cliff-veil error <code>: <message>`; [`vesting_core::parse_code`] reads the
//! code back. Failures raised by LEZ itself (not program codes) are matched by [`ChainFailure::classify`].

use lee_core::account::AccountId;
use vesting_core::{ERRORS, NextUnlock, VestingError};

use crate::backend::AccountRecord;
use crate::time::utc;

/// What a person can do about each program error code. Same codes, same order as [`vesting_core::ERRORS`]; a test
/// checks that no code is missing.
pub const ACTIONS: [(u32, &str); 29] = [
    (
        1,
        "sign with the account the operation is sent from; the wallet must hold its key",
    ),
    (
        2,
        "the message was not built by a matching client; rebuild it with this SDK",
    ),
    (3, "send from the schedule's current beneficiary (see `show`)"),
    (
        4,
        "send from the schedule's creator or its cancel authority (see `show`)",
    ),
    (5, "send from the schedule's cancel authority (see `show`)"),
    (6, "send from the schedule's milestone authority (see `show`)"),
    (7, "choose another schedule id for this creator"),
    (
        8,
        "check the schedule address: it is derived from the creator and the schedule id",
    ),
    (9, "derive the schedule address from the creator and the schedule id"),
    (10, "set a total greater than zero"),
    (11, "use times with start <= cliff <= end and start < end"),
    (12, "use non-zero milestone amounts that add up to the total"),
    (13, "use at most 64 milestones"),
    (14, "wait until the next unlock and claim again; nothing was consumed"),
    (
        15,
        "this schedule cannot be cancelled (it was created non-cancelable or made non-cancelable)",
    ),
    (
        16,
        "the schedule is cancelled: nothing more vests; the vested remainder stays claimable",
    ),
    (17, "use a milestone index the schedule has (see `show`)"),
    (18, "this milestone is already unlocked; signal another one"),
    (19, "this schedule was created non-transferable"),
    (20, "only milestone schedules take a milestone signal"),
    (21, "send one operation per schedule per transaction"),
    (22, "a guard of the program; report it with the transaction"),
    (
        23,
        "send operations to your own vesting actor (the SDK does), not to the schedule account",
    ),
    (24, "give at least one recipient"),
    (
        25,
        "split the batch into smaller transactions (`create_batch` does this)",
    ),
    (26, "pay out to an account other than the schedule account"),
    (
        27,
        "the fee rate or treasury changed since the quote: quote again and retry",
    ),
    (28, "the fee rate is capped at 1% (100 basis points)"),
    (29, "use amounts that fit in 128 bits"),
];

/// The message of program error `code`, as SPEC section 5 writes it.
#[must_use]
pub fn message_for_code(code: u32) -> Option<&'static str> {
    ERRORS.iter().find(|e| e.code == code).map(|e| e.message)
}

/// The name of program error `code` (`NotBeneficiary`, ...).
#[must_use]
pub fn name_for_code(code: u32) -> Option<&'static str> {
    ERRORS.iter().find(|e| e.code == code).map(|e| e.name)
}

/// What a person can do about program error `code`.
#[must_use]
pub fn action_for_code(code: u32) -> Option<&'static str> {
    ACTIONS.iter().find(|(c, _)| *c == code).map(|(_, a)| *a)
}

/// The text of a "nothing claimable" refusal, with the next unlock in UTC (RFP Usability 7).
#[must_use]
pub fn nothing_claimable_text(next: NextUnlock) -> String {
    match next {
        NextUnlock::At(t) => format!("nothing claimable yet; next unlock at {} ({t} ms)", utc(t)),
        NextUnlock::WhenMilestoneSignalled => {
            "nothing claimable yet; next unlock when the milestone authority signals a milestone".to_owned()
        }
        NextUnlock::Never => "nothing left to vest on this schedule".to_owned(),
    }
}

/// A failure raised by LEZ itself rather than by the vesting program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainFailure {
    /// An account cannot pay: a transfer or the network fee exceeds its balance.
    InsufficientBalance,
    /// The transaction's validity window has not opened yet (a `ClaimAsOf` whose `as_of` is ahead of the chain).
    ValidityWindow,
    /// The transaction ran past the execution budget.
    CycleBudget,
    /// A private account the wallet does not hold the keys of was asked to take part.
    ForeignPrivateAccount,
}

impl ChainFailure {
    /// Recognises a known LEZ failure in an error or revert text.
    #[must_use]
    pub fn classify(text: &str) -> Option<Self> {
        let t = text.to_ascii_lowercase();
        if t.contains("insufficientbalance")
            || t.contains("insufficient balance")
            || t.contains("can not pay for operation")
            || t.contains("insufficient funds")
        {
            Some(Self::InsufficientBalance)
        } else if t.contains("validity window") {
            Some(Self::ValidityWindow)
        } else if t.contains("cycle budget") {
            Some(Self::CycleBudget)
        } else if t.contains("foreign private account") {
            Some(Self::ForeignPrivateAccount)
        } else {
            None
        }
    }

    /// What it means.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::InsufficientBalance => "an account in the transaction cannot pay (amount or network fee)",
            Self::ValidityWindow => "the claim time is ahead of the chain",
            Self::CycleBudget => "the transaction exceeds the execution budget",
            Self::ForeignPrivateAccount => "the private account is not held by this wallet",
        }
    }

    /// What to do.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::InsufficientBalance => "fund it and retry; nothing was consumed",
            Self::ValidityWindow => "wait for the next block and retry",
            Self::CycleBudget => "split the batch into smaller ones",
            Self::ForeignPrivateAccount => "claim to a private account this wallet owns",
        }
    }

    /// What it means and what to do.
    #[must_use]
    pub const fn explain(self) -> &'static str {
        match self {
            Self::InsufficientBalance => {
                "an account in the transaction cannot pay (amount or network fee): fund it and retry; nothing was consumed"
            }
            Self::ValidityWindow => "the claim time is ahead of the chain: wait for the next block and retry",
            Self::CycleBudget => "the transaction exceeds the execution budget: split the batch into smaller ones",
            Self::ForeignPrivateAccount => {
                "the private account is not held by this wallet: claim to a private account this wallet owns"
            }
        }
    }
}

/// Every error of the SDK.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The vesting program refuses the operation (code 14 has its own variant, [`Error::NothingClaimable`]).
    #[error("cliff-veil error {code}: {message}. What to do: {action}")]
    Program {
        /// Stable code (SPEC section 5).
        code: u32,
        /// Its message.
        message: &'static str,
        /// What to do about it.
        action: &'static str,
        /// The full refusal text, when it came from a simulation or the sequencer.
        reason: Option<String>,
    },
    /// Nothing is claimable now (code 14); `next` is the next unlock, shown in UTC.
    #[error("cliff-veil error 14: {}", nothing_claimable_text(*next))]
    NothingClaimable {
        /// When more unlocks.
        next: NextUnlock,
    },
    /// No schedule at this account.
    #[error("cliff-veil error 8: no schedule at {0}. What to do: check the address (`show <schedule>`)")]
    NoSchedule(AccountId),
    /// Privacy 3: the target of a private claim is not a private account this wallet holds.
    #[error(
        "the claim target {account} is not a private account of this wallet ({found}); a private claim pays only \
         to a private (shielded) account. What to do: name a private account this wallet owns, or let the SDK create \
         a fresh one"
    )]
    NotPrivateAccount {
        /// The account named as target.
        account: AccountId,
        /// What the wallet's account record says it is.
        found: AccountRecord,
    },
    /// The account paying the network fee of a private claim cannot cover it (RFP Usability 4).
    #[error(
        "the private account {payer} cannot pay the network fee of this claim: it needs {need}, it will hold {have} \
         (short by {}). What to do: {hint}", need.saturating_sub(*have)
    )]
    PrivateFeeShortfall {
        /// The private account that pays the fee.
        payer: AccountId,
        /// Estimated fee.
        need: u128,
        /// What it holds at fee time (its balance, plus the claim when the claim is native and lands on it).
        have: u128,
        /// What to do.
        hint: &'static str,
    },
    /// A public account cannot pay what the operation needs (amount and estimated fee).
    #[error(
        "the account {account} cannot pay this operation: it needs {need} ({what}), it holds {have}. What to do: \
         fund it with at least {} more", need.saturating_sub(*have)
    )]
    InsufficientBalance {
        /// The paying account.
        account: AccountId,
        /// What it needs.
        need: u128,
        /// What it holds.
        have: u128,
        /// What the amount covers.
        what: &'static str,
    },
    /// A token destination has no holding of the definition yet (SPEC 3.1).
    #[error(
        "the account {0} has no holding of this token yet. What to do: initialise it with the token program's \
         InitializeHolding from that account, then retry"
    )]
    NoHolding(AccountId),
    /// A public destination never seen on chain and not held by this wallet cannot be admitted (SPEC 1.2).
    #[error(
        "the destination {0} does not exist on chain and this wallet holds no key for it. What to do: claim to an \
         account that exists, or to one of this wallet's accounts"
    )]
    UnknownDestination(AccountId),
    /// LEZ itself refuses the transaction.
    #[error("{}: {reason}", kind.explain())]
    Chain {
        /// The known failure.
        kind: ChainFailure,
        /// The full text.
        reason: String,
    },
    /// The transaction was included but reverted (it paid its fee and changed nothing else).
    #[error(
        "transaction {hash} was included in block {block} but reverted; nothing but its fee was consumed. The chain \
         publishes no reason for a revert: prepare the operation again to see the refusal from a local run"
    )]
    Reverted {
        /// Transaction hash.
        hash: String,
        /// Block.
        block: u64,
    },
    /// A wrong argument.
    #[error("{0}")]
    Input(String),
    /// The wallet, the RPC, or anything else outside the program.
    #[error(transparent)]
    Backend(#[from] anyhow::Error),
}

impl Error {
    /// The error of a `vesting_core` refusal (the SDK's own pre-flight).
    #[must_use]
    pub fn from_core(error: VestingError) -> Self {
        Self::from_code(error.code(), None, Some(error))
    }

    /// The error of a refusal text from a simulation, the sequencer or the wallet. `None` when the text holds no
    /// program code and no known LEZ failure.
    #[must_use]
    pub fn from_failure_text(text: &str) -> Option<Self> {
        if let Some(code) = vesting_core::parse_code(text) {
            return Some(Self::from_code(code, Some(text.to_owned()), None));
        }
        ChainFailure::classify(text).map(|kind| Self::Chain {
            kind,
            reason: text.to_owned(),
        })
    }

    /// As [`Self::from_failure_text`], falling back to [`Error::Backend`] with the text.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::from_failure_text(text).unwrap_or_else(|| Self::Backend(anyhow::anyhow!("{text}")))
    }

    fn from_code(code: u32, reason: Option<String>, core: Option<VestingError>) -> Self {
        if code == 14 {
            let next = match core {
                Some(VestingError::NothingClaimable { next }) => next,
                _ => next_unlock_from_text(reason.as_deref().unwrap_or_default()),
            };
            return Self::NothingClaimable { next };
        }
        Self::Program {
            code,
            message: message_for_code(code).unwrap_or("unknown error"),
            action: action_for_code(code).unwrap_or("report it with the transaction"),
            reason,
        }
    }

    /// A stable name for the kind of error: the program error's name for a program refusal (`NotBeneficiary`,
    /// `NothingClaimable`, ...), else the variant (`NotPrivateAccount`, `PrivateFeeShortfall`, `Chain`, ...).
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Program { code, .. } => name_for_code(*code).unwrap_or("Program"),
            Self::NothingClaimable { .. } => "NothingClaimable",
            Self::NoSchedule(_) => "NoSuchSchedule",
            Self::NotPrivateAccount { .. } => "NotPrivateAccount",
            Self::PrivateFeeShortfall { .. } => "PrivateFeeShortfall",
            Self::InsufficientBalance { .. } => "InsufficientBalance",
            Self::NoHolding(_) => "NoHolding",
            Self::UnknownDestination(_) => "UnknownDestination",
            Self::Chain { kind, .. } => match kind {
                ChainFailure::InsufficientBalance => "ChainInsufficientBalance",
                ChainFailure::ValidityWindow => "ValidityWindow",
                ChainFailure::CycleBudget => "CycleBudget",
                ChainFailure::ForeignPrivateAccount => "ForeignPrivateAccount",
            },
            Self::Reverted { .. } => "Reverted",
            Self::Input(_) => "Input",
            Self::Backend(_) => "Backend",
        }
    }

    /// What went wrong, without the action (the text before "What to do").
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Chain { kind, reason } => format!("{}: {reason}", kind.message()),
            Self::Reverted { hash, block } => {
                format!(
                    "transaction {hash} was included in block {block} but reverted; nothing but its fee was consumed"
                )
            }
            other => {
                let text = other.to_string();
                text.split_once(". What to do: ")
                    .map_or(text.clone(), |(m, _)| m.to_owned())
            }
        }
    }

    /// What to do about it, when there is something to do.
    #[must_use]
    pub fn action(&self) -> Option<String> {
        match self {
            Self::NothingClaimable { .. } => action_for_code(14).map(str::to_owned),
            Self::Chain { kind, .. } => Some(kind.action().to_owned()),
            Self::Reverted { .. } => Some(
                "the chain publishes no reason for a revert: prepare the operation again to see the refusal from a \
                 local run"
                    .to_owned(),
            ),
            Self::Input(_) | Self::Backend(_) => None,
            other => other
                .to_string()
                .split_once(". What to do: ")
                .map(|(_, a)| a.to_owned()),
        }
    }

    /// The program error code, if this is a program refusal.
    #[must_use]
    pub const fn code(&self) -> Option<u32> {
        match self {
            Self::Program { code, .. } => Some(*code),
            Self::NothingClaimable { .. } => Some(14),
            Self::NoSchedule(_) => Some(8),
            _ => None,
        }
    }
}

/// Reads the next unlock back out of a code-14 refusal text (`next unlock at <T>`, `... when a milestone is
/// signalled`, `nothing left to vest`).
fn next_unlock_from_text(text: &str) -> NextUnlock {
    if let Some(rest) = text.split("next unlock at ").nth(1) {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(t) = digits.parse() {
            return NextUnlock::At(t);
        }
    }
    if text.contains("milestone is signalled") {
        NextUnlock::WhenMilestoneSignalled
    } else {
        NextUnlock::Never
    }
}

/// `Result` with an SDK [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u7_every_program_code_has_a_message_and_an_action() {
        assert_eq!(ACTIONS.len(), ERRORS.len());
        for (row, (code, action)) in ERRORS.iter().zip(ACTIONS.iter()) {
            assert_eq!(row.code, *code, "ACTIONS is in the code order of vesting_core::ERRORS");
            assert!(!action.is_empty());
            assert_eq!(message_for_code(row.code), Some(row.message));
            assert_eq!(name_for_code(row.code), Some(row.name));
            let shown = Error::from_failure_text(&format!("cliff-veil error {}: {}", row.code, row.message))
                .expect("every code parses")
                .to_string();
            assert!(shown.contains(&format!("cliff-veil error {}", row.code)), "{shown}");
            if row.code != 14 {
                assert!(shown.contains(row.message) && shown.contains(action), "{shown}");
            }
        }
    }

    #[test]
    fn u7_no_code_outside_the_table() {
        assert!(message_for_code(0).is_none());
        assert!(message_for_code(30).is_none());
        assert!(action_for_code(30).is_none());
        assert!(Error::from_failure_text("cliff-veil error 30: x").is_none());
    }

    #[test]
    fn u7_nothing_claimable_shows_next_unlock_in_utc() {
        let e = Error::from_core(VestingError::NothingClaimable {
            next: NextUnlock::At(1_767_225_600_000),
        });
        assert_eq!(
            e.to_string(),
            "cliff-veil error 14: nothing claimable yet; next unlock at 2026-01-01T00:00:00.000Z (1767225600000 ms)"
        );
    }

    #[test]
    fn u7_nothing_claimable_from_a_sequencer_text_keeps_the_time() {
        let text = "Program execution failed: panicked: cliff-veil error 14: nothing claimable yet, next unlock at \
                    1767225600000";
        let e = Error::from_text(text);
        assert!(matches!(
            e,
            Error::NothingClaimable {
                next: NextUnlock::At(1_767_225_600_000)
            }
        ));
        assert!(e.to_string().contains("2026-01-01T00:00:00.000Z"));
        let m =
            Error::from_text("cliff-veil error 14: nothing claimable yet, next unlock when a milestone is signalled");
        assert!(matches!(
            m,
            Error::NothingClaimable {
                next: NextUnlock::WhenMilestoneSignalled
            }
        ));
        let n = Error::from_text("cliff-veil error 14: nothing left to vest");
        assert!(matches!(
            n,
            Error::NothingClaimable {
                next: NextUnlock::Never
            }
        ));
    }

    #[test]
    fn u7_codes_parse_out_of_the_wallet_texts_seen_on_rc3() {
        // Public send: the wallet's simulation refuses (S7 case A); the panic of an assert_eq! adds left/right lines.
        let public = "The transaction reverts when simulated and would still pay its fee, so it was not sent: Failed \
                      to execute program: Guest panicked: assertion `left == right` failed: cliff-veil error 3: only \
                      the beneficiary may do this\n  left: [1, 2]\n right: [3, 4]";
        assert_eq!(Error::from_text(public).code(), Some(3));
        // Private send: the refusal surfaces while the proof is built (S7 case D).
        let private = "Failed to build transaction: Failed to execute program: Guest panicked: cliff-veil error 14: \
                       nothing claimable yet, next unlock at 1767225600000\n  left: 0\n right: 1 \
                       (TransactionBuildError(ProgramExecutionFailed(\"...\")))";
        let e = Error::from_text(private);
        assert!(
            matches!(
                e,
                Error::NothingClaimable {
                    next: NextUnlock::At(1_767_225_600_000)
                }
            ),
            "{e}"
        );
    }

    #[test]
    fn u7_every_error_has_a_name_a_message_and_an_action_where_one_exists() {
        let e = Error::from_text("cliff-veil error 3: only the beneficiary may do this");
        assert_eq!(e.name(), "NotBeneficiary");
        assert_eq!(e.message(), "cliff-veil error 3: only the beneficiary may do this");
        assert_eq!(e.action().as_deref(), action_for_code(3));
        let n = Error::from_core(VestingError::NothingClaimable {
            next: NextUnlock::Never,
        });
        assert_eq!((n.name(), n.code()), ("NothingClaimable", Some(14)));
        assert!(n.action().is_some());
        let c = Error::from_text("Execution outside of the validity window");
        assert_eq!(c.name(), "ValidityWindow");
        assert_eq!(c.action().as_deref(), Some("wait for the next block and retry"));
        assert!(!c.message().contains("retry"));
        assert!(Error::Input("x".into()).action().is_none());
    }

    #[test]
    fn u7_error_names_are_stable() {
        // Clients (the mini-app) match on these names: pinned by hand, never renamed.
        let a = AccountId::new([1; 32]);
        let cases: Vec<(Error, &str)> = vec![
            (Error::from_core(VestingError::NotBeneficiary), "NotBeneficiary"),
            (
                Error::from_core(VestingError::NothingClaimable {
                    next: NextUnlock::Never,
                }),
                "NothingClaimable",
            ),
            (Error::NoSchedule(a), "NoSuchSchedule"),
            (
                Error::NotPrivateAccount {
                    account: a,
                    found: AccountRecord::PublicOwned,
                },
                "NotPrivateAccount",
            ),
            (
                Error::PrivateFeeShortfall {
                    payer: a,
                    need: 2,
                    have: 1,
                    hint: "",
                },
                "PrivateFeeShortfall",
            ),
            (
                Error::InsufficientBalance {
                    account: a,
                    need: 2,
                    have: 1,
                    what: "",
                },
                "InsufficientBalance",
            ),
            (Error::NoHolding(a), "NoHolding"),
            (Error::UnknownDestination(a), "UnknownDestination"),
            (
                Error::from_text("InsufficientBalance { account_id: x }"),
                "ChainInsufficientBalance",
            ),
            (
                Error::from_text("Execution outside of the validity window"),
                "ValidityWindow",
            ),
            (
                Error::from_text("Execution exceeded its cycle budget of 5 cycles"),
                "CycleBudget",
            ),
            (
                Error::from_text("is a foreign private account, which this wallet cannot witness"),
                "ForeignPrivateAccount",
            ),
            (
                Error::Reverted {
                    hash: String::new(),
                    block: 1,
                },
                "Reverted",
            ),
            (Error::Input(String::new()), "Input"),
            (Error::from_text("socket closed"), "Backend"),
        ];
        for (e, name) in cases {
            assert_eq!(e.name(), name, "{e}");
        }
        for row in ERRORS {
            if row.code != 14 {
                assert_eq!(
                    Error::from_text(&format!("cliff-veil error {}: x", row.code)).name(),
                    row.name
                );
            }
        }
    }

    #[test]
    fn u7_known_chain_failures_are_explained() {
        let e = Error::from_text("Failed to execute program: InsufficientBalance { account_id: x }");
        assert!(matches!(
            e,
            Error::Chain {
                kind: ChainFailure::InsufficientBalance,
                ..
            }
        ));
        assert!(e.to_string().contains("fund it and retry"));
        assert!(matches!(
            Error::from_text("Execution outside of the validity window"),
            Error::Chain {
                kind: ChainFailure::ValidityWindow,
                ..
            }
        ));
        assert!(matches!(Error::from_text("socket closed"), Error::Backend(_)));
    }
}
