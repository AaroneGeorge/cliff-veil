//! Error codes (SPEC section 5).
//!
//! The program refuses an operation by panicking with the [`Display`](core::fmt::Display) text of a
//! [`VestingError`]: `cliff-veil error <code>: <message>`. LEZ returns that text in the failed transaction's error,
//! and [`parse_code`] reads the code back out of it. Codes are stable: a code is never reused or renumbered.

use core::fmt;

/// When the next amount unlocks; carried by [`VestingError::NothingClaimable`] and returned by
/// [`ScheduleV1::next_unlock`](crate::ScheduleV1::next_unlock).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextUnlock {
    /// More vests at this clock time (ms since the Unix epoch).
    At(u64),
    /// More vests only when the milestone authority signals a milestone.
    WhenMilestoneSignalled,
    /// Nothing more will ever vest: the schedule is fully vested or cancelled.
    Never,
}

/// Every refusal of the vesting rules, with the stable code of SPEC section 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VestingError {
    /// 1: the caller did not authorise this transaction.
    NotAuthorised,
    /// 2: the message does not decode.
    InvalidMessage,
    /// 3: only the beneficiary may do this.
    NotBeneficiary,
    /// 4: only the creator or the cancel authority may do this.
    NotCreator,
    /// 5: only the cancel authority may cancel.
    NotCancelAuthority,
    /// 6: only the milestone authority may signal a milestone.
    NotMilestoneAuthority,
    /// 7: a schedule with this id already exists for this creator.
    ScheduleExists,
    /// 8: no schedule at this account.
    NoSuchSchedule,
    /// 9: the schedule account does not match the creator and schedule id.
    AddressMismatch,
    /// 10: the total must be greater than zero.
    ZeroTotal,
    /// 11: times must satisfy start <= cliff <= end and start < end.
    BadTimes,
    /// 12: milestone amounts must be non-zero and sum to the total.
    BadMilestones,
    /// 13: at most 64 milestones.
    TooManyMilestones,
    /// 14: nothing claimable yet; `next` says when more unlocks.
    NothingClaimable {
        /// When the next amount unlocks.
        next: NextUnlock,
    },
    /// 15: the schedule is not cancelable.
    NotCancelable,
    /// 16: the schedule is cancelled.
    AlreadyCancelled,
    /// 17: no milestone with this index.
    MilestoneOutOfRange,
    /// 18: this milestone was already signalled.
    MilestoneAlreadySignalled,
    /// 19: this schedule is not transferable.
    NotTransferable,
    /// 20: this schedule has no milestones.
    NotMilestoneSchedule,
    /// 21: another operation on this schedule is in progress in this transaction.
    OperationPending,
    /// 22: clock reply with no operation pending.
    UnexpectedClockReply,
    /// 23: a schedule accepts messages only from a vesting actor (a transaction or another program sent a message
    /// straight to a schedule account).
    UnexpectedSender,
    /// 24: a batch needs at least one item.
    EmptyBatch,
    /// 25: too many schedules in one batch (above [`MAX_BATCH_NATIVE`](crate::MAX_BATCH_NATIVE) or
    /// [`MAX_BATCH_TOKEN`](crate::MAX_BATCH_TOKEN)).
    BatchTooLarge,
    /// 26: a claim destination or refund account is the schedule account itself.
    PayoutToEscrow,
    /// 27: the creation fee does not match the current rate or treasury.
    FeeMismatch,
    /// 28: the fee rate, or a quoted fee, exceeds the hard cap of [`MAX_FEE_BPS`](crate::MAX_FEE_BPS).
    FeeAboveCap,
    /// 29: amount overflow.
    Overflow,
}

/// One row of the error table: code, name and message (for code 14, the message with a placeholder `<T>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorInfo {
    /// The stable numeric code.
    pub code: u32,
    /// The variant name, as in SPEC section 5.
    pub name: &'static str,
    /// The message, as in SPEC section 5.
    pub message: &'static str,
}

/// The whole error table of SPEC section 5, in code order. The SDK maps codes to messages with it, and a test
/// checks that every [`VestingError`] variant has exactly its row.
pub const ERRORS: [ErrorInfo; 29] = [
    row(1, "NotAuthorised", "the caller did not authorise this transaction"),
    row(2, "InvalidMessage", "the message does not decode"),
    row(3, "NotBeneficiary", "only the beneficiary may do this"),
    row(4, "NotCreator", "only the creator or the cancel authority may do this"),
    row(5, "NotCancelAuthority", "only the cancel authority may cancel"),
    row(
        6,
        "NotMilestoneAuthority",
        "only the milestone authority may signal a milestone",
    ),
    row(
        7,
        "ScheduleExists",
        "a schedule with this id already exists for this creator",
    ),
    row(8, "NoSuchSchedule", "no schedule at this account"),
    row(
        9,
        "AddressMismatch",
        "the schedule account does not match the creator and schedule id",
    ),
    row(10, "ZeroTotal", "the total must be greater than zero"),
    row(
        11,
        "BadTimes",
        "times must satisfy start <= cliff <= end and start < end",
    ),
    row(
        12,
        "BadMilestones",
        "milestone amounts must be non-zero and sum to the total",
    ),
    row(13, "TooManyMilestones", "at most 64 milestones"),
    row(14, "NothingClaimable", "nothing claimable yet, next unlock at <T>"),
    row(15, "NotCancelable", "the schedule is not cancelable"),
    row(16, "AlreadyCancelled", "the schedule is cancelled"),
    row(17, "MilestoneOutOfRange", "no milestone with this index"),
    row(18, "MilestoneAlreadySignalled", "this milestone was already signalled"),
    row(19, "NotTransferable", "this schedule is not transferable"),
    row(20, "NotMilestoneSchedule", "this schedule has no milestones"),
    row(
        21,
        "OperationPending",
        "another operation on this schedule is in progress in this transaction",
    ),
    row(22, "UnexpectedClockReply", "clock reply with no operation pending"),
    row(
        23,
        "UnexpectedSender",
        "a schedule accepts messages only from a vesting actor",
    ),
    row(24, "EmptyBatch", "a batch needs at least one item"),
    row(25, "BatchTooLarge", "too many schedules in one batch"),
    row(
        26,
        "PayoutToEscrow",
        "a claim destination or refund account cannot be the schedule account",
    ),
    row(
        27,
        "FeeMismatch",
        "the creation fee does not match the current rate or treasury",
    ),
    row(28, "FeeAboveCap", "the fee rate exceeds the hard cap"),
    row(29, "Overflow", "amount overflow"),
];

const fn row(code: u32, name: &'static str, message: &'static str) -> ErrorInfo {
    ErrorInfo { code, name, message }
}

/// Prefix of every refusal text.
pub const ERROR_PREFIX: &str = "cliff-veil error ";

impl VestingError {
    /// The stable numeric code.
    #[must_use]
    pub const fn code(&self) -> u32 {
        match self {
            Self::NotAuthorised => 1,
            Self::InvalidMessage => 2,
            Self::NotBeneficiary => 3,
            Self::NotCreator => 4,
            Self::NotCancelAuthority => 5,
            Self::NotMilestoneAuthority => 6,
            Self::ScheduleExists => 7,
            Self::NoSuchSchedule => 8,
            Self::AddressMismatch => 9,
            Self::ZeroTotal => 10,
            Self::BadTimes => 11,
            Self::BadMilestones => 12,
            Self::TooManyMilestones => 13,
            Self::NothingClaimable { .. } => 14,
            Self::NotCancelable => 15,
            Self::AlreadyCancelled => 16,
            Self::MilestoneOutOfRange => 17,
            Self::MilestoneAlreadySignalled => 18,
            Self::NotTransferable => 19,
            Self::NotMilestoneSchedule => 20,
            Self::OperationPending => 21,
            Self::UnexpectedClockReply => 22,
            Self::UnexpectedSender => 23,
            Self::EmptyBatch => 24,
            Self::BatchTooLarge => 25,
            Self::PayoutToEscrow => 26,
            Self::FeeMismatch => 27,
            Self::FeeAboveCap => 28,
            Self::Overflow => 29,
        }
    }

    /// The row of [`ERRORS`] for this error.
    #[must_use]
    pub const fn info(&self) -> &'static ErrorInfo {
        &ERRORS[self.code() as usize - 1]
    }

    /// The variant name, as in SPEC section 5.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.info().name
    }

    /// Writes the message alone (no `cliff-veil error <code>: ` prefix). For code 14 the message names the next
    /// unlock: `nothing claimable yet, next unlock at <T>` with `<T>` in ms since the Unix epoch, or
    /// `nothing claimable yet, next unlock when a milestone is signalled`, or `nothing left to vest`.
    ///
    /// # Errors
    /// Only those of the formatter.
    pub fn write_message(&self, f: &mut impl fmt::Write) -> fmt::Result {
        match self {
            Self::NothingClaimable {
                next: NextUnlock::At(t),
            } => write!(f, "nothing claimable yet, next unlock at {t}"),
            Self::NothingClaimable {
                next: NextUnlock::WhenMilestoneSignalled,
            } => f.write_str("nothing claimable yet, next unlock when a milestone is signalled"),
            Self::NothingClaimable {
                next: NextUnlock::Never,
            } => f.write_str("nothing left to vest"),
            other => f.write_str(other.info().message),
        }
    }
}

impl fmt::Display for VestingError {
    /// The refusal text: `cliff-veil error <code>: <message>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{ERROR_PREFIX}{}: ", self.code())?;
        self.write_message(f)
    }
}

impl core::error::Error for VestingError {}

/// Reads the code out of a refusal text, wherever it appears in `text` (LEZ wraps the panic message in its own
/// error text). Returns `None` when `text` holds no `cliff-veil error <code>:` with a known code.
#[must_use]
pub fn parse_code(text: &str) -> Option<u32> {
    let start = text.find(ERROR_PREFIX)? + ERROR_PREFIX.len();
    let rest = &text[start..];
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if digits == 0 || !rest[digits..].starts_with(':') {
        return None;
    }
    let code: u32 = rest[..digits].parse().ok()?;
    ERRORS.iter().any(|e| e.code == code).then_some(code)
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use super::*;

    fn every_variant() -> Vec<VestingError> {
        use VestingError::*;
        alloc::vec![
            NotAuthorised,
            InvalidMessage,
            NotBeneficiary,
            NotCreator,
            NotCancelAuthority,
            NotMilestoneAuthority,
            ScheduleExists,
            NoSuchSchedule,
            AddressMismatch,
            ZeroTotal,
            BadTimes,
            BadMilestones,
            TooManyMilestones,
            NothingClaimable {
                next: NextUnlock::At(5)
            },
            NotCancelable,
            AlreadyCancelled,
            MilestoneOutOfRange,
            MilestoneAlreadySignalled,
            NotTransferable,
            NotMilestoneSchedule,
            OperationPending,
            UnexpectedClockReply,
            UnexpectedSender,
            EmptyBatch,
            BatchTooLarge,
            PayoutToEscrow,
            FeeMismatch,
            FeeAboveCap,
            Overflow,
        ]
    }

    #[test]
    fn u7_every_code_has_one_variant_and_one_message() {
        let variants = every_variant();
        assert_eq!(variants.len(), ERRORS.len());
        for (i, (variant, row)) in variants.iter().zip(ERRORS.iter()).enumerate() {
            assert_eq!(row.code as usize, i + 1, "the table is in code order with no gap");
            assert_eq!(variant.code(), row.code);
            assert_eq!(variant.name(), row.name);
            assert!(!row.message.is_empty());
        }
    }

    #[test]
    fn u7_codes_are_stable() {
        // Pinned by hand: renumbering a code breaks every client that maps it.
        assert_eq!(VestingError::NotAuthorised.code(), 1);
        assert_eq!(
            VestingError::NothingClaimable {
                next: NextUnlock::Never
            }
            .code(),
            14
        );
        assert_eq!(VestingError::MilestoneAlreadySignalled.code(), 18);
        assert_eq!(VestingError::Overflow.code(), 29);
    }

    #[test]
    fn u7_display_is_the_refusal_text() {
        assert_eq!(
            VestingError::NotBeneficiary.to_string(),
            "cliff-veil error 3: only the beneficiary may do this"
        );
        assert_eq!(
            VestingError::BatchTooLarge.to_string(),
            "cliff-veil error 25: too many schedules in one batch"
        );
    }

    #[test]
    fn u7_nothing_claimable_reports_next_unlock_time() {
        let e = VestingError::NothingClaimable {
            next: NextUnlock::At(1_767_225_600_000),
        };
        assert_eq!(
            e.to_string(),
            "cliff-veil error 14: nothing claimable yet, next unlock at 1767225600000"
        );
    }

    #[test]
    fn u7_nothing_claimable_milestone_and_never_texts() {
        let m = VestingError::NothingClaimable {
            next: NextUnlock::WhenMilestoneSignalled,
        };
        assert_eq!(
            m.to_string(),
            "cliff-veil error 14: nothing claimable yet, next unlock when a milestone is signalled"
        );
        let n = VestingError::NothingClaimable {
            next: NextUnlock::Never,
        };
        assert_eq!(n.to_string(), "cliff-veil error 14: nothing left to vest");
    }

    #[test]
    fn u7_every_display_parses_back_to_its_code() {
        for variant in every_variant() {
            let wrapped = alloc::format!("ProgramExecutionFailed(\"panicked at src/main.rs: {variant}\")");
            assert_eq!(parse_code(&wrapped), Some(variant.code()), "{wrapped}");
        }
    }

    #[test]
    fn parse_code_refuses_unknown_or_malformed_codes() {
        assert_eq!(parse_code("cliff-veil error 0: x"), None);
        assert_eq!(parse_code("cliff-veil error 30: x"), None);
        assert_eq!(parse_code("cliff-veil error : x"), None);
        assert_eq!(parse_code("cliff-veil error 12x: x"), None);
        assert_eq!(parse_code("cliff-veil error 99999999999999999999: x"), None);
        assert_eq!(parse_code("some other failure"), None);
        assert_eq!(parse_code(""), None);
    }

    #[test]
    fn write_message_has_no_prefix() {
        let mut s = String::new();
        VestingError::ZeroTotal.write_message(&mut s).unwrap();
        assert_eq!(s, "the total must be greater than zero");
    }
}
