//! Composition at executor level: the `launchpad_escrow` example (examples/launchpad_escrow) calling `vesting` as a
//! program-owned account, run by LEZ's public execution exactly as the sequencer runs a block.
//!
//! A launch account (a PDA of the launchpad) creates the project's collateral schedule and is its milestone and
//! cancel authority. The project is only the beneficiary: `vesting` refuses its direct signal, cancel and
//! make-non-cancelable, and the launchpad signals or cancels only when `M` of its `N` approvers agree.
//! docs/COMPOSITION.md describes the transactions.

mod common;

use clock_core::clock_actor;
use common::{Chain, DAY, Outcome, Parties, Sent, T0, cid, code, core_id, id, parties};
use launchpad_escrow::{Internal, LaunchError, LaunchMessage, LaunchState, launch_account};
use lee::PrivateKey;
use lee::error::LeeError;
use lee_core::account::{AccountId, Actor};
use vesting_core::{Cancelled, Kind, MilestoneSignalled, ScheduleCreated};
use vesting_program::fees_config_actor;

/// Where the tests deploy the launchpad example.
const LAUNCHPAD: AccountId = AccountId::new(*b"cliff-veil/test/launchpad/000000");
const AMOUNTS: [u128; 3] = [300_000, 300_000, 400_000];
const TOTAL: u128 = 1_000_000;
const LAUNCH_ID: [u8; 32] = [0x1a; 32];

// vesting's codes (SPEC 5).
const NOT_CREATOR: u32 = 4;
const NOT_CANCEL_AUTHORITY: u32 = 5;
const NOT_MILESTONE_AUTHORITY: u32 = 6;
const NOTHING_CLAIMABLE: u32 = 14;

/// The chain with `vesting` and the launchpad deployed; the project (the `creator` key) holds the collateral.
fn chain() -> Option<Chain> {
    assert_eq!(
        launchpad_escrow::VESTING_PROGRAM_ID,
        common::VESTING,
        "the example is built against the test deployment of vesting (CV_VESTING_PROGRAM_ID unset)"
    );
    let mut chain = Chain::new(T0)?.with_program(LAUNCHPAD, launchpad_escrow_methods::LAUNCHPAD_ESCROW_ELF);
    chain.fund_native(id(&parties().creator), 10 * TOTAL);
    Some(chain)
}

/// The approvers: three accounts other than the project.
fn approvers(p: &Parties) -> [&PrivateKey; 3] {
    [&p.canceller, &p.signaller, &p.heir]
}

/// The refusal code of a `launchpad_escrow` refusal.
fn launch_code(sent: &Sent) -> u32 {
    match sent {
        Err(LeeError::ProgramExecutionFailed(text)) => {
            launchpad_escrow::parse_code(text).unwrap_or_else(|| panic!("no launchpad-escrow code in: {text}"))
        }
        other => panic!("expected a launchpad refusal, got {other:?}"),
    }
}

/// One launch: its accounts and the transactions of docs/COMPOSITION.md.
struct Launch {
    launch: AccountId,
    schedule: AccountId,
    refund_to: AccountId,
}

impl Launch {
    fn of(project: &PrivateKey, launch_id: [u8; 32], refund_to: AccountId) -> Self {
        let launch = launch_account(&LAUNCHPAD, &id(project), &launch_id);
        Self {
            launch,
            schedule: launchpad_escrow::schedule_account(&launch, &launch_id),
            refund_to,
        }
    }

    /// Actors every launch transaction reaches: the launch, its seat under `vesting`, the schedule.
    fn core_actors(&self, chain: &Chain) -> [Actor; 3] {
        [
            Actor::new(self.launch, LAUNCHPAD),
            chain.seat(self.launch),
            Actor::new(self.schedule, chain.vesting),
        ]
    }

    /// `Open` by `project`, 2 of 3 approvers, `AMOUNTS`, fee 0.
    fn open(chain: &mut Chain, project: &PrivateKey, approvers: Vec<AccountId>, threshold: u8) -> (Self, Sent) {
        let refund_to = id(&parties().refund);
        Self::open_with(chain, project, LAUNCH_ID, approvers, threshold, refund_to)
    }

    fn open_with(
        chain: &mut Chain,
        project: &PrivateKey,
        launch_id: [u8; 32],
        approvers: Vec<AccountId>,
        threshold: u8,
        refund_to: AccountId,
    ) -> (Self, Sent) {
        let l = Self::of(project, launch_id, refund_to);
        let root = Actor::new(id(project), LAUNCHPAD);
        let mut declared = vec![root];
        declared.extend(l.core_actors(chain));
        declared.extend([
            Actor::native_balance(id(project)),
            Actor::native_balance(l.schedule),
            fees_config_actor(),
        ]);
        let message = LaunchMessage::Open {
            launch_id,
            amounts: AMOUNTS.to_vec(),
            approvers,
            threshold,
            refund_to,
            fee: 0,
            treasury: id(project),
        };
        let sent = chain.submit(root, &declared, &[project], &message);
        (l, sent)
    }

    fn approve(&self, chain: &mut Chain, approver: &PrivateKey, index: u8) -> Sent {
        let root = Actor::new(id(approver), LAUNCHPAD);
        let mut declared = vec![root];
        declared.extend(self.core_actors(chain));
        let message = LaunchMessage::Approve {
            launch: self.launch,
            index,
        };
        chain.submit(root, &declared, &[approver], &message)
    }

    fn vote_cancel(&self, chain: &mut Chain, approver: &PrivateKey) -> Sent {
        let root = Actor::new(id(approver), LAUNCHPAD);
        let mut declared = vec![root];
        declared.extend(self.core_actors(chain));
        declared.extend([
            clock_actor(),
            Actor::native_balance(self.schedule),
            Actor::native_balance(self.refund_to),
        ]);
        let message = LaunchMessage::VoteCancel { launch: self.launch };
        chain.submit(root, &declared, &[approver], &message)
    }

    fn state(&self, chain: &Chain) -> LaunchState {
        let bytes = chain
            .state
            .get_account_by_id(self.launch)
            .data
            .actor_state(LAUNCHPAD)
            .clone();
        borsh::from_slice(&bytes).expect("launch state decodes")
    }
}

fn ids(keys: &[&PrivateKey]) -> Vec<AccountId> {
    keys.iter().map(|k| id(k)).collect()
}

fn opened(chain: &mut Chain, p: &Parties) -> (Launch, Outcome) {
    let (l, sent) = Launch::open(chain, &p.creator, ids(&approvers(p)), 2);
    (l, sent.expect("the launch opens"))
}

#[test]
fn soft2_compose_a_launch_program_pda_creates_the_schedule_and_holds_both_authorities() {
    let Some(mut chain) = chain() else { return };
    let p = parties();
    let project_before = chain.native(id(&p.creator));
    let (l, out) = opened(&mut chain, &p);

    let s = chain.schedule(l.schedule).expect("the schedule exists");
    let launch = core_id(l.launch);
    assert_eq!(
        s.creator, launch,
        "the schedule's creator is the launch program's account"
    );
    assert_eq!(s.milestone_authority, launch);
    assert_eq!(s.cancel_authority, launch);
    assert_eq!(s.beneficiary, cid(&p.creator), "the project is only the beneficiary");
    assert_eq!(s.refund_to, core_id(l.refund_to));
    assert!(s.cancelable && !s.transferable);
    assert_eq!(s.total, TOTAL);
    assert_eq!(chain.native(l.schedule), TOTAL, "the escrow holds the collateral");
    assert_eq!(
        project_before - chain.native(id(&p.creator)),
        TOTAL,
        "funded from the project's account"
    );

    let created: ScheduleCreated = out.one(chain.vesting);
    assert_eq!((created.creator, created.milestone_authority), (launch, launch));
    let state = l.state(&chain);
    assert_eq!(state.schedule, l.schedule);
    assert_eq!(state.approvers, ids(&approvers(&p)));
    assert_eq!((state.threshold, state.signalled, state.cancelled), (2, 0, false));
}

#[test]
fn soft2_compose_the_project_cannot_signal_cancel_or_freeze_its_own_collateral() {
    let Some(mut chain) = chain() else { return };
    let p = parties();
    let (l, _) = opened(&mut chain, &p);

    // Straight to `vesting`, as the creator wallet of a plain schedule would.
    let sent = chain.signal(&p.creator, l.schedule, 0);
    assert_eq!(code(&sent), NOT_MILESTONE_AUTHORITY);
    let sent = chain.cancel(&p.creator, l.schedule);
    assert_eq!(code(&sent), NOT_CANCEL_AUTHORITY);
    let sent = chain.make_non_cancelable(&p.creator, l.schedule);
    assert_eq!(code(&sent), NOT_CREATOR);
    // Through the launchpad: the project is not an approver.
    assert_eq!(
        launch_code(&l.approve(&mut chain, &p.creator, 0)),
        LaunchError::NotApprover.code()
    );
    assert_eq!(
        launch_code(&l.vote_cancel(&mut chain, &p.creator)),
        LaunchError::NotApprover.code()
    );
    // Nor is anyone else outside the set.
    assert_eq!(
        launch_code(&l.approve(&mut chain, &p.stranger, 0)),
        LaunchError::NotApprover.code()
    );

    let s = chain.schedule(l.schedule).expect("schedule");
    assert_eq!(
        s.kind,
        Kind::Milestones {
            amounts: AMOUNTS.to_vec(),
            signalled: 0
        }
    );
    assert!(s.cancelable && s.cancelled.is_none());
    assert_eq!(chain.native(l.schedule), TOTAL);
}

#[test]
fn soft2_compose_m_of_n_approvals_signal_then_the_project_claims() {
    let Some(mut chain) = chain() else { return };
    let p = parties();
    let (l, _) = opened(&mut chain, &p);
    let [a, b, c] = approvers(&p);
    let to = id(&p.recipient);

    // One approval of 2: nothing signalled, nothing claimable.
    let out = l.approve(&mut chain, a, 0).expect("first approval");
    assert!(
        out.vesting_events(chain.vesting).is_empty(),
        "no signal below the threshold"
    );
    assert_eq!(code(&chain.claim(&p.creator, l.schedule, to)), NOTHING_CLAIMABLE);
    assert_eq!(
        launch_code(&l.approve(&mut chain, a, 0)),
        LaunchError::AlreadyVoted.code()
    );

    // The second approval signals milestone 0 in the same transaction.
    let out = l.approve(&mut chain, c, 0).expect("second approval");
    let signalled: MilestoneSignalled = out.one(chain.vesting);
    assert_eq!(
        (signalled.index, signalled.amount, signalled.signalled),
        (0, AMOUNTS[0], 1)
    );
    assert_eq!(l.state(&chain).signalled, 1);
    assert_eq!(
        launch_code(&l.approve(&mut chain, b, 0)),
        LaunchError::MilestoneAlreadySignalled.code()
    );
    assert_eq!(
        launch_code(&l.approve(&mut chain, b, 3)),
        LaunchError::MilestoneOutOfRange.code()
    );

    // The project claims exactly the approved milestone.
    chain.tick(T0 + DAY);
    chain.claim(&p.creator, l.schedule, to).expect("the project claims");
    assert_eq!(chain.native(to), AMOUNTS[0]);
    assert_eq!(chain.native(l.schedule), TOTAL - AMOUNTS[0]);
}

#[test]
fn soft1_compose_cancel_only_by_the_launch_rule_refunds_the_unvested_collateral() {
    let Some(mut chain) = chain() else { return };
    let p = parties();
    let (l, _) = opened(&mut chain, &p);
    let [a, b, c] = approvers(&p);
    l.approve(&mut chain, a, 1).expect("approval");
    l.approve(&mut chain, b, 1).expect("milestone 1 signalled");

    // The project cannot cancel (vesting refuses it) and one vote of 2 cancels nothing.
    assert_eq!(code(&chain.cancel(&p.creator, l.schedule)), NOT_CANCEL_AUTHORITY);
    let out = l.vote_cancel(&mut chain, c).expect("first vote");
    assert!(out.vesting_events(chain.vesting).is_empty());
    assert!(chain.schedule(l.schedule).expect("schedule").cancelled.is_none());

    // The second vote cancels: the unvested part goes to the refund account, the signalled one stays claimable.
    let refund_before = chain.native(l.refund_to);
    chain.tick(T0 + DAY);
    let out = l.vote_cancel(&mut chain, a).expect("second vote cancels");
    let cancelled: Cancelled = out.one(chain.vesting);
    assert_eq!(cancelled.vested_at_cancel, AMOUNTS[1]);
    assert_eq!(cancelled.refunded, TOTAL - AMOUNTS[1]);
    assert_eq!(chain.native(l.refund_to) - refund_before, TOTAL - AMOUNTS[1]);
    assert!(l.state(&chain).cancelled);

    let to = id(&p.recipient);
    chain
        .claim(&p.creator, l.schedule, to)
        .expect("the vested part stays claimable");
    assert_eq!(chain.native(to), AMOUNTS[1]);
    assert_eq!(chain.native(l.schedule), 0);
    // After the cancel the launch takes no more votes.
    assert_eq!(
        launch_code(&l.approve(&mut chain, c, 2)),
        LaunchError::LaunchCancelled.code()
    );
    assert_eq!(
        launch_code(&l.vote_cancel(&mut chain, b)),
        LaunchError::LaunchCancelled.code()
    );
}

#[test]
fn compose_open_refusals_carry_their_codes() {
    let Some(mut chain) = chain() else { return };
    let p = parties();
    let set = ids(&approvers(&p));
    let refund = id(&p.refund);
    let with_project = vec![id(&p.creator), set[0]];
    let cases: [(Vec<AccountId>, u8, AccountId, LaunchError); 5] = [
        (vec![], 1, refund, LaunchError::BadApprovers),
        (with_project, 1, refund, LaunchError::BadApprovers),
        (vec![set[0], set[0]], 1, refund, LaunchError::BadApprovers),
        (set.clone(), 4, refund, LaunchError::BadThreshold),
        (set.clone(), 2, id(&p.creator), LaunchError::RefundToProject),
    ];
    for (approvers, threshold, refund_to, expected) in cases {
        let (_, sent) = Launch::open_with(&mut chain, &p.creator, LAUNCH_ID, approvers, threshold, refund_to);
        assert_eq!(launch_code(&sent), expected.code(), "{expected:?}");
    }
    let (l, sent) = Launch::open_with(&mut chain, &p.creator, LAUNCH_ID, set.clone(), 2, refund);
    sent.expect("a valid launch opens");
    let (_, sent) = Launch::open_with(&mut chain, &p.creator, LAUNCH_ID, set, 2, refund);
    assert_eq!(launch_code(&sent), LaunchError::LaunchExists.code());
    assert_eq!(chain.native(l.schedule), TOTAL, "nothing moved twice");
}

#[test]
fn compose_a_launch_accepts_messages_only_from_seats() {
    let Some(mut chain) = chain() else { return };
    let p = parties();
    let (l, _) = opened(&mut chain, &p);
    // A transaction sending `ApproveFor` straight to the launch actor is decoded as a user message at an actor that
    // holds state, and refused.
    let root = Actor::new(l.launch, LAUNCHPAD);
    let mut declared = vec![root];
    declared.extend(l.core_actors(&chain)[1..].iter().copied());
    let sent = chain.submit(root, &declared, &[], &Internal::ApproveFor { index: 0 });
    assert_eq!(launch_code(&sent), LaunchError::UnexpectedSender.code());
    assert_eq!(l.state(&chain).signalled, 0);
}
