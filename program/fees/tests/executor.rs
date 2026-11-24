//! `vesting_fees` at executor level: the real guest, run by LEZ's public execution on a `V03State` exactly as the
//! sequencer runs a block, metered under the chain's 10,000,000-cycle cap.
//!
//! The guest comes from `vesting_methods`. Under `RISC0_SKIP_BUILD=1` (`make test`) it is not built and these tests
//! say so and return. With the guests built they run in full:
//!
//! ```text
//! source scripts/env.sh && cargo test -p vesting_fees --test executor -- --nocapture --test-threads=1
//! ```
//!
//! Each test prints the cycles of every transaction it sends (`cycles <operation> <n>`), the source of
//! `evidence/fees/cycles.txt`.

use std::borrow::Cow;

use borsh::BorshSerialize;
use lee::{
    PrivateKey, PublicAccountEvidence, PublicKey, PublicTransaction, V03State, ValidatedStateDiff,
    error::LeeError,
    program::Program,
    public_transaction::{Message, WitnessSet},
};
use lee_core::account::{AccountId, Actor};
use lee_core::program::TransactionEvent;
use lez_admin_authority::{AdminOp, AuthoritySlot, Holder, Wire, admin_op};
use vesting_core::{Asset, Event, FeeChanged};
use vesting_fees::{CONFIG_SEED, FeeConfig, FeeMessage, config_account, config_actor, wire};

/// The chain's per-block execution cap (fee market `MAX_GAS_EXEC`, rc3).
const CYCLE_CAP: u64 = 10_000_000;
const TOTAL: u128 = 1_000_000;

fn key(byte: u8) -> PrivateKey {
    PrivateKey::try_new([byte; 32]).expect("valid test key")
}

fn account(key: &PrivateKey) -> AccountId {
    AccountId::from(&PublicKey::new_from_private_key(key))
}

fn core_id(id: AccountId) -> vesting_core::AccountId {
    vesting_core::AccountId::new(id.into_value())
}

/// One deployment of `vesting_fees`. Its program id is the account of `deployer`, as on LEZ 0.4, where the program id
/// is the loader header account and the deployer holds its key.
struct Fees {
    state: V03State,
    program: AccountId,
    deployer: PrivateKey,
    /// Admission evidence for the config account, which does not exist before `Init`.
    evidence: Vec<PublicAccountEvidence>,
}

struct Outcome {
    events: Vec<TransactionEvent>,
    cycles: u64,
}

impl Fees {
    /// `None` when the guest was not built (`RISC0_SKIP_BUILD=1`).
    fn deploy() -> Option<Self> {
        if vesting_methods::VESTING_FEES_ELF.is_empty() {
            assert_eq!(
                std::env::var("RISC0_SKIP_BUILD").as_deref(),
                Ok("1"),
                "the vesting_fees guest is empty although RISC0_SKIP_BUILD is not set"
            );
            eprintln!("vesting_fees guest not built (RISC0_SKIP_BUILD=1): executor test skipped");
            return None;
        }
        let deployer = key(0x11);
        let program = account(&deployer);
        let guest = Program::new(Cow::Borrowed(vesting_methods::VESTING_FEES_ELF)).expect("the guest loads");
        let state = V03State::new().with_named_programs(vec![(program, guest)]);
        let evidence = vec![PublicAccountEvidence::Pda {
            program,
            seed: CONFIG_SEED,
        }];
        Some(Self {
            state,
            program,
            deployer,
            evidence,
        })
    }

    /// A deployment whose deployer has sent `Init`.
    fn initialised() -> Option<Self> {
        let mut fees = Self::deploy()?;
        let deployer = fees.deployer.clone();
        fees.admin(&deployer, &admin_op(AdminOp::Init), "init")
            .expect("the deployer's init");
        Some(fees)
    }

    fn seat(&self, id: AccountId) -> Actor {
        Actor::new(id, self.program)
    }

    fn config(&self) -> Option<FeeConfig> {
        let account = self.state.get_account_by_id(config_account(self.program));
        let bytes = account.data.actor_state(self.program);
        (!bytes.is_empty()).then(|| borsh::from_slice(bytes).expect("the config decodes"))
    }

    /// A message from `signer`'s own actor (seat) to the config: the admin's operations and admin-only messages.
    fn admin(&mut self, signer: &PrivateKey, message: &Wire<FeeMessage>, what: &str) -> Result<Outcome, LeeError> {
        let seat = self.seat(account(signer));
        let declared = [seat, config_actor(self.program)];
        self.submit(seat, &declared, &[signer], message, what)
    }

    /// `SetFee` from `signer`'s seat.
    fn set_fee(
        &mut self,
        signer: &PrivateKey,
        rate_bps: u16,
        treasury: AccountId,
        what: &str,
    ) -> Result<Outcome, LeeError> {
        let message = wire(FeeMessage::SetFee {
            rate_bps,
            treasury: core_id(treasury),
        });
        self.admin(signer, &message, what)
    }

    /// `CheckFee` sent by a transaction straight to the config actor, signed by `payer` only.
    fn check_fee(&mut self, payer: &PrivateKey, message: FeeMessage, what: &str) -> Result<Outcome, LeeError> {
        let config = config_actor(self.program);
        self.submit(config, &[config], &[payer], &wire(message), what)
    }

    fn submit(
        &mut self,
        root: Actor,
        declared: &[Actor],
        signers: &[&PrivateKey],
        message: &impl BorshSerialize,
        what: &str,
    ) -> Result<Outcome, LeeError> {
        let nonces = signers
            .iter()
            .map(|key| {
                let id = account(key);
                (id, self.state.get_account_by_id(id).nonce)
            })
            .collect();
        let mut msg = Message::try_new(root, declared.iter().copied(), nonces, message).expect("the message encodes");
        msg.admission_evidence.clone_from(&self.evidence);
        let witness_set = WitnessSet::for_message(&msg, signers);
        let tx = PublicTransaction::new(msg, witness_set);
        let (diff, charge) =
            ValidatedStateDiff::from_public_transaction_with_cycle_budget(&tx, &self.state, 1, 1_000, CYCLE_CAP)?;
        let events = self.state.apply_state_diff(diff)?;
        println!("cycles {what} {}", charge.cycles);
        Ok(Outcome {
            events,
            cycles: charge.cycles,
        })
    }
}

fn check(asset: Asset, total: u128, fee: u128, treasury: AccountId) -> FeeMessage {
    FeeMessage::CheckFee {
        total,
        fee,
        treasury: core_id(treasury),
        asset,
    }
}

fn native_check(total: u128, fee: u128, treasury: AccountId) -> FeeMessage {
    check(Asset::Native, total, fee, treasury)
}

/// The refusal text of a failed transaction.
fn refusal<T>(result: Result<T, LeeError>) -> String {
    match result {
        Err(LeeError::ProgramExecutionFailed(message)) => message,
        Err(other) => panic!("expected a guest refusal, got {other:?}"),
        Ok(_) => panic!("expected a guest refusal, the transaction passed"),
    }
}

fn assert_refused<T>(result: Result<T, LeeError>, needle: &str) {
    let text = refusal(result);
    assert!(text.contains(needle), "expected {needle:?} in {text:?}");
}

fn held_by(id: AccountId) -> AuthoritySlot {
    AuthoritySlot::held(Holder { account: id, via: None })
}

fn the_fee_changed(outcome: &Outcome, program: AccountId) -> FeeChanged {
    let [event] = outcome.events.as_slice() else {
        panic!("expected one event, got {:?}", outcome.events);
    };
    assert_eq!(event.account_id, program, "emitted by vesting_fees");
    assert_eq!(event.event.selector, FeeChanged::SELECTOR);
    assert_eq!(FeeChanged::SELECTOR, vesting_core::event_selector("FeeChanged"));
    borsh::from_slice(&event.event.data).expect("FeeChanged decodes")
}

#[test]
fn fee_empty_config_reads_as_rate_zero() {
    let Some(mut fees) = Fees::deploy() else { return };
    let payer = key(0x22);
    assert_eq!(fees.config(), None);
    // Before Init a creation with fee 0 passes, with any treasury.
    fees.check_fee(
        &payer,
        native_check(TOTAL, 0, account(&key(0x33))),
        "check_fee_empty_config",
    )
    .expect("fee 0 passes on an empty config");
    // Any other fee is refused: nothing is charged before a rate is set.
    assert_refused(
        fees.check_fee(
            &payer,
            native_check(TOTAL, 1, AccountId::default()),
            "check_fee_empty_config_wrong",
        ),
        "cliff-veil error 27:",
    );
    // The admin's messages still need Init: SetFee on the empty config is refused by the guard.
    assert_refused(
        fees.set_fee(&payer, 1, AccountId::default(), "set_fee_empty_config"),
        "lez-authority/admin E101",
    );
    assert_eq!(fees.config(), None, "nothing written");
}

#[test]
fn fee_empty_config_needs_admission_evidence() {
    // The config account does not exist before Init, so a transaction that reaches it must admit it (the SDK adds
    // the PDA evidence); without it the engine refuses before the program runs.
    let Some(mut fees) = Fees::deploy() else { return };
    fees.evidence.clear();
    let result = fees.check_fee(
        &key(0x22),
        native_check(TOTAL, 0, AccountId::default()),
        "check_fee_unadmitted",
    );
    let error = result.err().expect("refused without admission evidence");
    assert!(format!("{error:?}").contains("Unadmitted"), "{error:?}");
}

#[test]
fn fee_init_installs_the_deployer_at_rate_zero() {
    let Some(mut fees) = Fees::deploy() else { return };
    // Only the deployer (the program's own account) installs the first admin.
    assert_refused(
        fees.admin(&key(0x33), &admin_op(AdminOp::Init), "init_stranger"),
        "lez-authority/admin E117",
    );
    assert_eq!(fees.config(), None);
    let deployer = fees.deployer.clone();
    let out = fees.admin(&deployer, &admin_op(AdminOp::Init), "init").expect("init");
    assert!(out.events.is_empty());
    assert_eq!(
        fees.config(),
        Some(FeeConfig {
            admin: held_by(fees.program),
            ..FeeConfig::default()
        })
    );
    assert_refused(
        fees.admin(&deployer, &admin_op(AdminOp::Init), "init_again"),
        "lez-authority/admin E118",
    );
}

#[test]
fn fee_check_passes_at_zero_after_init() {
    let Some(mut fees) = Fees::initialised() else { return };
    let payer = key(0x22);
    let out = fees
        .check_fee(&payer, native_check(TOTAL, 0, AccountId::default()), "check_fee_rate_0")
        .expect("fee 0 at rate 0");
    assert!(out.events.is_empty());
    let token = Asset::Token {
        definition: vesting_core::AccountId::new([0xde; 32]),
    };
    fees.check_fee(
        &payer,
        check(token, TOTAL, 0, AccountId::default()),
        "check_fee_rate_0_token",
    )
    .expect("token fee 0 at rate 0");
    assert_refused(
        fees.check_fee(
            &payer,
            native_check(TOTAL, 10, AccountId::default()),
            "check_fee_rate_0_wrong",
        ),
        "cliff-veil error 27:",
    );
}

#[test]
fn fee_set_fee_by_a_non_admin_is_refused() {
    let Some(mut fees) = Fees::initialised() else { return };
    let before = fees.config();
    let stranger = key(0x33);
    assert_refused(
        fees.set_fee(&stranger, 50, account(&stranger), "set_fee_stranger"),
        "lez-authority/admin E103",
    );
    // Straight to the config, even signed by the admin, an admin-only message is refused.
    let deployer = fees.deployer.clone();
    let config = config_actor(fees.program);
    let direct = wire(FeeMessage::SetFee {
        rate_bps: 50,
        treasury: core_id(account(&deployer)),
    });
    assert_refused(
        fees.submit(config, &[config], &[&deployer], &direct, "set_fee_direct"),
        "lez-authority/admin E122",
    );
    assert_eq!(fees.config(), before);
}

#[test]
fn fee_set_fee_above_the_cap_is_refused() {
    let Some(mut fees) = Fees::initialised() else { return };
    let deployer = fees.deployer.clone();
    let treasury = account(&key(0x44));
    assert_refused(
        fees.set_fee(&deployer, 101, treasury, "set_fee_above_cap"),
        "cliff-veil error 28:",
    );
    assert_refused(
        fees.set_fee(&deployer, u16::MAX, treasury, "set_fee_max_u16"),
        "cliff-veil error 28:",
    );
    assert_eq!(fees.config().unwrap().rate_bps, 0);
    // The cap itself is allowed, and the change is an event.
    let out = fees.set_fee(&deployer, 100, treasury, "set_fee").expect("1%");
    assert_eq!(
        the_fee_changed(&out, fees.program),
        FeeChanged {
            old_rate_bps: 0,
            new_rate_bps: 100,
            treasury: core_id(treasury),
            by: core_id(fees.program),
        }
    );
    let config = fees.config().unwrap();
    assert_eq!((config.rate_bps, config.treasury), (100, treasury));
    assert!(out.cycles < CYCLE_CAP);
}

#[test]
fn fee_wrong_fee_or_treasury_is_refused() {
    let Some(mut fees) = Fees::initialised() else { return };
    let deployer = fees.deployer.clone();
    let payer = key(0x22);
    let treasury = account(&key(0x44));
    let out = fees.set_fee(&deployer, 50, treasury, "set_fee_50").expect("0.5%");
    assert_eq!(the_fee_changed(&out, fees.program).treasury, core_id(treasury));
    // 0.5% of 1,000,000 is 5,000.
    fees.check_fee(&payer, native_check(TOTAL, 5_000, treasury), "check_fee_rate_50")
        .expect("the right fee to the treasury");
    for (fee, to, what) in [
        (4_999, treasury, "check_fee_short"),
        (5_001, treasury, "check_fee_over"),
        (0, treasury, "check_fee_zero_at_rate_50"),
        (5_000, account(&key(0x33)), "check_fee_wrong_treasury"),
    ] {
        assert_refused(
            fees.check_fee(&payer, native_check(TOTAL, fee, to), what),
            "cliff-veil error 27:",
        );
    }
    // One rate for every asset; `vesting` derives the token fee's holding from the checked treasury.
    let token = Asset::Token {
        definition: vesting_core::AccountId::new([0xde; 32]),
    };
    fees.check_fee(&payer, check(token, TOTAL, 5_000, treasury), "check_fee_rate_50_token")
        .expect("the same rate on a token");
    // A fee that rounds down to zero needs no treasury.
    fees.check_fee(
        &payer,
        native_check(199, 0, AccountId::default()),
        "check_fee_rounds_to_zero",
    )
    .expect("floor(199 * 50 / 10000) = 0");
}

#[test]
fn fee_check_is_answered_only_at_the_config_account() {
    let Some(mut fees) = Fees::initialised() else { return };
    let payer = key(0x22);
    // Any other actor of the program has an empty state; reading it as rate 0 would wave any fee through.
    let elsewhere = fees.seat(account(&payer));
    let message = wire(native_check(TOTAL, 0, AccountId::default()));
    assert_refused(
        fees.submit(elsewhere, &[elsewhere], &[&payer], &message, "check_fee_elsewhere"),
        "cliff-veil error 27:",
    );
}

#[test]
fn fee_after_renounce_set_fee_is_refused_and_creation_goes_on() {
    let Some(mut fees) = Fees::initialised() else { return };
    let deployer = fees.deployer.clone();
    let payer = key(0x22);
    fees.admin(&deployer, &admin_op(AdminOp::Renounce), "renounce")
        .expect("renounce");
    assert_eq!(fees.config().unwrap().admin, AuthoritySlot::Renounced);
    assert_refused(
        fees.set_fee(&deployer, 1, AccountId::default(), "set_fee_after_renounce"),
        "lez-authority/admin E102",
    );
    assert_refused(
        fees.admin(&deployer, &admin_op(AdminOp::Init), "init_after_renounce"),
        "lez-authority/admin E102",
    );
    fees.check_fee(
        &payer,
        native_check(TOTAL, 0, AccountId::default()),
        "check_fee_after_renounce",
    )
    .expect("the rate stays 0 for good, creation goes on");
    assert_refused(
        fees.check_fee(
            &payer,
            native_check(TOTAL, 1, AccountId::default()),
            "check_fee_after_renounce_wrong",
        ),
        "cliff-veil error 27:",
    );
}

#[test]
fn fee_admin_hands_over_in_two_steps() {
    let Some(mut fees) = Fees::initialised() else { return };
    let deployer = fees.deployer.clone();
    let next = key(0x55);
    let treasury = account(&key(0x44));
    fees.admin(
        &deployer,
        &admin_op(AdminOp::Nominate { next: account(&next) }),
        "nominate",
    )
    .expect("nominate");
    // Nothing moves until the nominee accepts.
    assert_refused(
        fees.set_fee(&next, 1, treasury, "set_fee_nominee_early"),
        "lez-authority/admin E103",
    );
    fees.admin(&next, &admin_op(AdminOp::Accept { pda_seed: None }), "accept")
        .expect("accept");
    assert_eq!(fees.config().unwrap().admin, held_by(account(&next)));
    let out = fees
        .set_fee(&next, 1, treasury, "set_fee_new_admin")
        .expect("new admin");
    assert_eq!(the_fee_changed(&out, fees.program).by, core_id(account(&next)));
    assert_refused(
        fees.set_fee(&deployer, 2, treasury, "set_fee_old_admin"),
        "lez-authority/admin E103",
    );
}
