//! `launchpad_escrow`: a minimal launch program that locks a project's collateral in a `vesting` schedule it
//! controls itself. It is a reference for launchpads (RFP-015 bonding curve, RFP-016 LBP) that use vesting as buyer
//! protection, and is not part of the `vesting` deployment. docs/COMPOSITION.md explains the integration.
//!
//! # The problem it shows a way out of
//!
//! When a project's collateral vests by milestones, whoever signals the milestones decides when the project gets
//! paid. If the project itself held that power, it could signal every milestone at once and drain the collateral.
//! Here the schedule is created by this program's own account (a PDA), and that account is the schedule's creator,
//! milestone authority and cancel authority. The PDA has no private key: only this program can act for it, and the
//! program acts only when its own rule is met.
//!
//! # The rule
//!
//! When a project opens a launch it names a fixed approver set (1 to 16 accounts, never the project itself) and a
//! threshold `M`. Then:
//! - a milestone is signalled to `vesting` in the same transaction as the `M`-th distinct approval of it;
//! - the schedule is cancelled, and the unvested collateral refunded to `refund_to`, in the same transaction as the
//!   `M`-th distinct cancel vote.
//!
//! The project is the schedule's beneficiary and claims from `vesting` directly, as any beneficiary does. It cannot
//! signal, cancel or make the schedule non-cancelable: `vesting` refuses its direct messages (codes 6, 5 and 4), and
//! this program refuses it as an approver (code 10 here).
//!
//! In this example the project names the approver set when it opens the launch; the set is then public in the launch
//! state and never changes, and buyers (or the launchpad's front end) check it before buying. A production
//! launchpad takes the set from its own governance configuration instead: only [`LaunchState::new`] changes.
//!
//! # Actors (the same seat routing as `vesting`, SPEC 1.3)
//!
//! - **Seat** `(caller, launchpad)`: always empty. A transaction sends every [`LaunchMessage`] here; it requires
//!   `is_authorized` and forwards an [`Internal`] message to the launch actor.
//! - **Launch** `(launch, launchpad)`, `launch = for_public_pda(launchpad, launch_seed(project, launch_id))`: holds
//!   the [`LaunchState`] and accepts [`Internal`] messages only from seats of this program.
//! - **The launch account under `vesting`**, `(launch, vesting)`: the vesting seat of the launch account. The launch
//!   actor sends `vesting` messages there with the launch seed attached, which authorises the launch account for that
//!   delivery. `vesting` therefore sees an authorised caller `launch` and treats it like any signed creator.
//!
//! # Build input
//!
//! The `vesting` program this launchpad talks to is fixed at build time: `CV_VESTING_PROGRAM_ID` (base58), else
//! [`LOCAL_VESTING_PROGRAM_ID`] (where the executor tests deploy `vesting`). A launch can therefore never be pointed
//! at another program that merely looks like `vesting`.

use std::fmt;

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::account::{AccountId, Actor};
use lee_core::program::{PdaSeed, ReceiveInput, Response, SyncMessage};
use sha2::{Digest as _, Sha256};
use vesting_core::{Asset, Kind, Message as VestingMessage, Terms};

/// `vesting` id used when `CV_VESTING_PROGRAM_ID` is unset (local executor tests only).
pub const LOCAL_VESTING_PROGRAM_ID: AccountId = AccountId::new(*b"cliff-veil/test/vesting/00000000");

/// The `vesting` program every launch creates its schedule in (build input).
pub const VESTING_PROGRAM_ID: AccountId = match option_env!("CV_VESTING_PROGRAM_ID") {
    Some(text) => AccountId::new(vesting_program::decode_base58_id(text)),
    None => LOCAL_VESTING_PROGRAM_ID,
};

/// Most approvers a launch may name (one bit each in a `u16`).
pub const MAX_APPROVERS: usize = 16;

/// Prefix of the launch seed preimage.
pub const LAUNCH_SEED_PREFIX: &[u8] = b"launchpad-escrow/launch/v1";

// ---------------------------------------------------------------------------------------------- wire types

/// A message a transaction sends to the caller's own seat `(caller, launchpad)`.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum LaunchMessage {
    /// Tag 0, by the project: open a launch and lock `sum(amounts)` native units of collateral from the project's
    /// own account in a milestone schedule of `vesting` whose creator and authorities are the launch account.
    Open {
        /// Chosen by the project; with the project it derives the launch account. Also the schedule id.
        launch_id: [u8; 32],
        /// Milestone amounts of the collateral schedule (as `vesting`'s `Kind::Milestones`).
        amounts: Vec<u128>,
        /// The approver set: 1 to 16 distinct accounts, not the project.
        approvers: Vec<AccountId>,
        /// Approvals (or cancel votes) needed: `1..=approvers.len()`.
        threshold: u8,
        /// Where a cancel refunds the unvested collateral (a buyer refund pool); not the project.
        refund_to: AccountId,
        /// The `vesting` creation fee the client quoted (0 while the fee switch is off), passed through.
        fee: u128,
        /// The `vesting` fee treasury the client read, passed through.
        treasury: AccountId,
    },
    /// Tag 1, by an approver: approve milestone `index` of `launch`.
    Approve {
        /// The launch account.
        launch: AccountId,
        /// Milestone index.
        index: u8,
    },
    /// Tag 2, by an approver: vote to cancel `launch` and refund the unvested collateral.
    VoteCancel {
        /// The launch account.
        launch: AccountId,
    },
}

/// A message from a seat of this program to a launch actor.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[allow(clippy::large_enum_variant, reason = "decoded once per delivery and never stored")]
pub enum Internal {
    /// Tag 0: write the launch and create its schedule. Accepted only from the project's seat, at the derived account.
    Init {
        /// The launch.
        state: LaunchState,
        /// Milestone amounts of the schedule.
        amounts: Vec<u128>,
        /// Passed to `vesting`'s `Create`.
        fee: u128,
        /// Passed to `vesting`'s `Create`.
        treasury: AccountId,
    },
    /// Tag 1: the sending seat approves milestone `index`.
    ApproveFor {
        /// Milestone index.
        index: u8,
    },
    /// Tag 2: the sending seat votes to cancel.
    VoteCancelFor,
}

/// The state of a launch actor (Borsh).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LaunchState {
    /// Chosen by the project; also the schedule id under `vesting`.
    pub launch_id: [u8; 32],
    /// The project: beneficiary of the collateral schedule, with no power over it.
    pub project: AccountId,
    /// The schedule account under `vesting` (creator: this launch account).
    pub schedule: AccountId,
    /// Refund account of the schedule.
    pub refund_to: AccountId,
    /// The approver set, fixed at `Open`.
    pub approvers: Vec<AccountId>,
    /// Approvals or cancel votes needed.
    pub threshold: u8,
    /// Per milestone: bit `i` set when `approvers[i]` approved it.
    pub approvals: Vec<u16>,
    /// Bit `i` set once milestone `i` was signalled to `vesting`.
    pub signalled: u64,
    /// Bit `i` set when `approvers[i]` voted to cancel.
    pub cancel_votes: u16,
    /// Set once the cancel was sent to `vesting`.
    pub cancelled: bool,
}

impl LaunchState {
    /// A new launch of `project`, validated: the approver set, the threshold and the refund account. The amounts are
    /// validated by `vesting` itself (its codes 10, 12, 13).
    ///
    /// # Errors
    /// [`LaunchError::BadApprovers`], [`LaunchError::BadThreshold`], [`LaunchError::RefundToProject`].
    pub fn new(
        project: AccountId,
        launch_id: [u8; 32],
        milestones: usize,
        approvers: Vec<AccountId>,
        threshold: u8,
        refund_to: AccountId,
        launchpad: &AccountId,
    ) -> Result<Self, LaunchError> {
        let distinct = approvers.iter().enumerate().all(|(i, a)| !approvers[..i].contains(a));
        if approvers.is_empty() || approvers.len() > MAX_APPROVERS || !distinct || approvers.contains(&project) {
            return Err(LaunchError::BadApprovers);
        }
        if threshold == 0 || usize::from(threshold) > approvers.len() {
            return Err(LaunchError::BadThreshold);
        }
        if refund_to == project {
            return Err(LaunchError::RefundToProject);
        }
        let launch = launch_account(launchpad, &project, &launch_id);
        Ok(Self {
            launch_id,
            project,
            schedule: schedule_account(&launch, &launch_id),
            refund_to,
            approvers,
            threshold,
            approvals: vec![0; milestones],
            signalled: 0,
            cancel_votes: 0,
            cancelled: false,
        })
    }

    /// The approver bit of `account`.
    ///
    /// # Errors
    /// [`LaunchError::NotApprover`].
    pub fn approver_bit(&self, account: &AccountId) -> Result<u16, LaunchError> {
        self.approvers
            .iter()
            .position(|a| a == account)
            .map(|i| 1u16 << i)
            .ok_or(LaunchError::NotApprover)
    }

    /// Records `approver`'s approval of milestone `index`. Returns true when this approval reaches the threshold:
    /// the caller then signals the milestone to `vesting`.
    ///
    /// # Errors
    /// Not an approver (10), cancelled (14), no such milestone (12), already signalled (13), already approved (11).
    pub fn approve(&mut self, approver: &AccountId, index: u8) -> Result<bool, LaunchError> {
        let bit = self.approver_bit(approver)?;
        if self.cancelled {
            return Err(LaunchError::LaunchCancelled);
        }
        let i = usize::from(index);
        let votes = self.approvals.get_mut(i).ok_or(LaunchError::MilestoneOutOfRange)?;
        if self.signalled & (1u64 << i) != 0 {
            return Err(LaunchError::MilestoneAlreadySignalled);
        }
        if *votes & bit != 0 {
            return Err(LaunchError::AlreadyVoted);
        }
        *votes |= bit;
        let reached = votes.count_ones() >= u32::from(self.threshold);
        if reached {
            self.signalled |= 1u64 << i;
        }
        Ok(reached)
    }

    /// Records `approver`'s cancel vote. Returns true when this vote reaches the threshold: the caller then cancels
    /// the schedule.
    ///
    /// # Errors
    /// Not an approver (10), cancelled (14), already voted (11).
    pub fn vote_cancel(&mut self, approver: &AccountId) -> Result<bool, LaunchError> {
        let bit = self.approver_bit(approver)?;
        if self.cancelled {
            return Err(LaunchError::LaunchCancelled);
        }
        if self.cancel_votes & bit != 0 {
            return Err(LaunchError::AlreadyVoted);
        }
        self.cancel_votes |= bit;
        self.cancelled = self.cancel_votes.count_ones() >= u32::from(self.threshold);
        Ok(self.cancelled)
    }
}

// ---------------------------------------------------------------------------------------------- errors

/// Refusals of this program. Each fails the whole transaction with `launchpad-escrow error <code>: <message>`.
/// `vesting` refusals keep their own `cliff-veil error` codes (SPEC 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum LaunchError {
    /// The caller did not authorise this transaction.
    NotAuthorised = 1,
    /// The message does not decode.
    InvalidMessage = 2,
    /// A launch accepts messages only from a seat of this program.
    UnexpectedSender = 3,
    /// 1 to 16 distinct approvers, none of them the project.
    BadApprovers = 4,
    /// The threshold must be between 1 and the number of approvers.
    BadThreshold = 5,
    /// The refund account cannot be the project.
    RefundToProject = 6,
    /// A launch with this id already exists for this project.
    LaunchExists = 7,
    /// No launch at this account.
    NoSuchLaunch = 8,
    /// The launch account does not match the project and launch id.
    AddressMismatch = 9,
    /// Only an approver of this launch may do this.
    NotApprover = 10,
    /// This approver already voted for this.
    AlreadyVoted = 11,
    /// No milestone with this index.
    MilestoneOutOfRange = 12,
    /// This milestone was already signalled.
    MilestoneAlreadySignalled = 13,
    /// The launch is cancelled.
    LaunchCancelled = 14,
}

/// Prefix of every refusal text of this program.
pub const ERROR_PREFIX: &str = "launchpad-escrow error ";

impl LaunchError {
    /// The stable code.
    #[must_use]
    pub const fn code(self) -> u32 {
        self as u32
    }

    /// The message.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotAuthorised => "the caller did not authorise this transaction",
            Self::InvalidMessage => "the message does not decode",
            Self::UnexpectedSender => "a launch accepts messages only from a seat of this program",
            Self::BadApprovers => "1 to 16 distinct approvers, none of them the project",
            Self::BadThreshold => "the threshold must be between 1 and the number of approvers",
            Self::RefundToProject => "the refund account cannot be the project",
            Self::LaunchExists => "a launch with this id already exists for this project",
            Self::NoSuchLaunch => "no launch at this account",
            Self::AddressMismatch => "the launch account does not match the project and launch id",
            Self::NotApprover => "only an approver of this launch may do this",
            Self::AlreadyVoted => "this approver already voted for this",
            Self::MilestoneOutOfRange => "no milestone with this index",
            Self::MilestoneAlreadySignalled => "this milestone was already signalled",
            Self::LaunchCancelled => "the launch is cancelled",
        }
    }
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{ERROR_PREFIX}{}: {}", self.code(), self.message())
    }
}

/// The code of a `launchpad-escrow error <code>:` refusal anywhere in `text`.
#[must_use]
pub fn parse_code(text: &str) -> Option<u32> {
    let rest = &text[text.find(ERROR_PREFIX)? + ERROR_PREFIX.len()..];
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[digits..].starts_with(':').then(|| rest[..digits].parse().ok())?
}

// ---------------------------------------------------------------------------------------------- derivations

/// The launch seed `SHA-256("launchpad-escrow/launch/v1" ‖ project ‖ launch_id)`.
#[must_use]
pub fn launch_seed(project: &AccountId, launch_id: &[u8; 32]) -> PdaSeed {
    let mut h = Sha256::new();
    h.update(LAUNCH_SEED_PREFIX);
    h.update(project.value());
    h.update(launch_id);
    PdaSeed::new(h.finalize().into())
}

/// The launch account of `(project, launch_id)` under the launchpad deployed at `launchpad`.
#[must_use]
pub fn launch_account(launchpad: &AccountId, project: &AccountId, launch_id: &[u8; 32]) -> AccountId {
    AccountId::for_public_pda(launchpad, &launch_seed(project, launch_id))
}

/// The `vesting` schedule a launch creates: creator `launch`, schedule id `launch_id` (SPEC 1.2).
#[must_use]
pub fn schedule_account(launch: &AccountId, launch_id: &[u8; 32]) -> AccountId {
    vesting_program::schedule_account(&VESTING_PROGRAM_ID, &core(*launch), launch_id)
}

// ---------------------------------------------------------------------------------------------- handler

/// Handles one delivery: `lee_core::actor_entrypoint!(raw launchpad_escrow::handle)`.
pub fn handle(input: &ReceiveInput) -> Response {
    match input.from {
        Some(from) if from.program_account_id == input.receiver.program_account_id => {
            on_internal(input, from.account_id)
        }
        _ => on_message(input),
    }
}

/// A [`LaunchMessage`] at the caller's seat: checks the caller signed, then forwards to the launch actor.
fn on_message(input: &ReceiveInput) -> Response {
    if !input.pre_state.is_empty() {
        refuse(LaunchError::UnexpectedSender);
    }
    let message = LaunchMessage::try_from_slice(&input.message).unwrap_or_else(|_| refuse(LaunchError::InvalidMessage));
    if !input.is_authorized {
        refuse(LaunchError::NotAuthorised);
    }
    let (caller, program) = (input.receiver.account_id, input.receiver.program_account_id);
    let (launch, internal) = match message {
        LaunchMessage::Open {
            launch_id,
            amounts,
            approvers,
            threshold,
            refund_to,
            fee,
            treasury,
        } => {
            let state = LaunchState::new(
                caller,
                launch_id,
                amounts.len(),
                approvers,
                threshold,
                refund_to,
                &program,
            )
            .unwrap_or_else(|e| refuse(e));
            let seed = launch_seed(&caller, &launch_id);
            let init = Internal::Init {
                state,
                amounts,
                fee,
                treasury,
            };
            // The seed admits the new launch account.
            let to = Actor::new(AccountId::for_public_pda(&program, &seed), program);
            return Response::keep_state().send(SyncMessage::new(to, &init).with_pda_seeds([seed]));
        }
        LaunchMessage::Approve { launch, index } => (launch, Internal::ApproveFor { index }),
        LaunchMessage::VoteCancel { launch } => (launch, Internal::VoteCancelFor),
    };
    Response::keep_state().send_sync(Actor::new(launch, program), &internal)
}

/// An [`Internal`] message from `sender`'s seat of this program, at a launch actor.
fn on_internal(input: &ReceiveInput, sender: AccountId) -> Response {
    let message = Internal::try_from_slice(&input.message).unwrap_or_else(|_| refuse(LaunchError::InvalidMessage));
    let here = input.receiver.account_id;
    if let Internal::Init {
        state,
        amounts,
        fee,
        treasury,
    } = message
    {
        return init(input, sender, state, amounts, fee, treasury);
    }
    if input.pre_state.is_empty() {
        refuse(LaunchError::NoSuchLaunch);
    }
    let mut s = LaunchState::try_from_slice(&input.pre_state).unwrap_or_else(|_| refuse(LaunchError::InvalidMessage));
    let seed = launch_seed(&s.project, &s.launch_id);
    let schedule = core(s.schedule);
    match message {
        Internal::ApproveFor { index } => {
            if !s.approve(&sender, index).unwrap_or_else(|e| refuse(e)) {
                return save(&s);
            }
            // The M-th approval: signal the milestone as the schedule's milestone authority.
            save(&s).send(to_vesting(
                here,
                seed,
                &VestingMessage::SignalMilestone { schedule, index },
            ))
        }
        Internal::VoteCancelFor => {
            if !s.vote_cancel(&sender).unwrap_or_else(|e| refuse(e)) {
                return save(&s);
            }
            // The M-th vote: cancel as the schedule's cancel authority; `vesting` refunds the unvested part.
            save(&s).send(to_vesting(here, seed, &VestingMessage::Cancel { schedule }))
        }
        Internal::Init { .. } => refuse(LaunchError::InvalidMessage),
    }
}

/// `Init` from the project's seat: writes the launch, then creates the collateral schedule under `vesting`, with
/// the launch account as creator, milestone authority and cancel authority, funded from the project's account.
fn init(
    input: &ReceiveInput,
    sender: AccountId,
    state: LaunchState,
    amounts: Vec<u128>,
    fee: u128,
    treasury: AccountId,
) -> Response {
    let here = input.receiver.account_id;
    let seed = launch_seed(&state.project, &state.launch_id);
    if sender != state.project || AccountId::for_public_pda(&input.receiver.program_account_id, &seed) != here {
        refuse(LaunchError::AddressMismatch);
    }
    if !input.pre_state.is_empty() {
        refuse(LaunchError::LaunchExists);
    }
    let launch = Some(core(here));
    let total = amounts
        .iter()
        .try_fold(0u128, |sum, a| sum.checked_add(*a))
        .unwrap_or(u128::MAX);
    let create = VestingMessage::Create {
        schedule_id: state.launch_id,
        terms: Terms {
            beneficiary: core(state.project),
            asset: Asset::Native,
            total,
            kind: Kind::Milestones { amounts, signalled: 0 },
            cancelable: true,
            transferable: false,
            cancel_authority: launch,
            milestone_authority: launch,
            refund_to: Some(core(state.refund_to)),
        },
        // The project signed this transaction, so `vesting` may debit its account.
        source: core(state.project),
        fee,
        treasury: core(treasury),
    };
    save(&state).send(to_vesting(here, seed, &create))
}

/// A message to the launch account's seat under `vesting`, with the launch seed attached: for this delivery the
/// launch account is authorised, so `vesting` accepts it as the caller (creator, milestone or cancel authority).
fn to_vesting(launch: AccountId, seed: PdaSeed, message: &VestingMessage) -> SyncMessage {
    SyncMessage::new(Actor::new(launch, VESTING_PROGRAM_ID), message).with_pda_seeds([seed])
}

fn save(s: &LaunchState) -> Response {
    Response::set_state(borsh::to_vec(s).expect("launch state encodes"))
}

fn refuse(error: LaunchError) -> ! {
    panic!("{error}")
}

const fn core(id: AccountId) -> vesting_core::AccountId {
    vesting_core::AccountId::new(id.into_value())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(b: u8) -> AccountId {
        AccountId::new([b; 32])
    }

    fn launch(approvers: &[u8], threshold: u8) -> Result<LaunchState, LaunchError> {
        let set = approvers.iter().map(|b| acct(*b)).collect();
        LaunchState::new(acct(1), [7; 32], 3, set, threshold, acct(9), &acct(100))
    }

    #[test]
    fn open_refuses_a_bad_approver_set_threshold_or_refund_account() {
        assert_eq!(launch(&[], 1), Err(LaunchError::BadApprovers));
        assert_eq!(launch(&[2, 2], 1), Err(LaunchError::BadApprovers));
        assert_eq!(
            launch(&[2, 1], 1),
            Err(LaunchError::BadApprovers),
            "the project is never an approver"
        );
        let seventeen: Vec<u8> = (10..27).collect();
        assert_eq!(launch(&seventeen, 1), Err(LaunchError::BadApprovers));
        assert_eq!(launch(&[2, 3], 0), Err(LaunchError::BadThreshold));
        assert_eq!(launch(&[2, 3], 3), Err(LaunchError::BadThreshold));
        let set = vec![acct(2)];
        assert_eq!(
            LaunchState::new(acct(1), [7; 32], 1, set, 1, acct(1), &acct(100)),
            Err(LaunchError::RefundToProject)
        );
    }

    #[test]
    fn the_mth_distinct_approval_signals_once() {
        let mut s = launch(&[2, 3, 4], 2).expect("valid");
        assert_eq!(s.approve(&acct(1), 0), Err(LaunchError::NotApprover));
        assert_eq!(s.approve(&acct(2), 0), Ok(false));
        assert_eq!(s.approve(&acct(2), 0), Err(LaunchError::AlreadyVoted));
        assert_eq!(s.approve(&acct(3), 3), Err(LaunchError::MilestoneOutOfRange));
        assert_eq!(s.approve(&acct(3), 0), Ok(true));
        assert_eq!(s.signalled, 1);
        assert_eq!(s.approve(&acct(4), 0), Err(LaunchError::MilestoneAlreadySignalled));
    }

    #[test]
    fn the_mth_cancel_vote_cancels_and_freezes_the_launch() {
        let mut s = launch(&[2, 3, 4], 2).expect("valid");
        assert_eq!(s.vote_cancel(&acct(2)), Ok(false));
        assert_eq!(s.vote_cancel(&acct(2)), Err(LaunchError::AlreadyVoted));
        assert_eq!(s.vote_cancel(&acct(4)), Ok(true));
        assert_eq!(s.vote_cancel(&acct(3)), Err(LaunchError::LaunchCancelled));
        assert_eq!(s.approve(&acct(3), 1), Err(LaunchError::LaunchCancelled));
    }

    #[test]
    fn error_texts_carry_their_codes() {
        let text = format!("guest panicked: {}", LaunchError::NotApprover);
        assert_eq!(parse_code(&text), Some(10));
        assert_eq!(parse_code("cliff-veil error 6: no"), None);
    }

    #[test]
    fn the_schedule_is_derived_from_the_launch_account() {
        let s = launch(&[2], 1).expect("valid");
        let la = launch_account(&acct(100), &acct(1), &[7; 32]);
        let seed = vesting_core::schedule_seed(&core(la), &[7; 32]);
        assert_eq!(
            s.schedule,
            AccountId::for_public_pda(&VESTING_PROGRAM_ID, &PdaSeed::new(seed))
        );
    }
}
