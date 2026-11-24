//! A local chain for executor-level tests: the real `vesting`, `vesting_fees` and token guests, run by LEZ's public
//! execution on a `V03State` exactly as the sequencer runs a block, metered under the chain's 10,000,000-cycle cap.
//!
//! Transactions are built with the shapes of SPEC 3.2 (root actor, declared actors, signers, admission evidence).

#![allow(dead_code, reason = "each test binary uses a different part of the harness")]

use std::borrow::Cow;

use borsh::BorshSerialize;
use clock_core::{CLOCK_PROGRAM_ACCOUNT_ID, ClockAccountData, clock_account_id, clock_actor};
use lee::{
    PrivateKey, PublicAccountEvidence, PublicKey, PublicTransaction, V03State, ValidatedStateDiff,
    error::LeeError,
    program::Program,
    public_transaction::{Message as TxMessage, WitnessSet},
};
use lee_core::{
    account::{Account, AccountId, Actor},
    native_token::{self, NATIVE_TOKEN_PROGRAM_ID},
    program::TransactionEvent,
};
use token_core::{TokenHolding, TokenState};
use vesting_core::{
    Asset, BatchItem, BatchShape, BatchTerms, Event, Kind, Message, ScheduleState, ScheduleV1, Terms, parse_code,
};
use vesting_fees::FeeConfig;
use vesting_program::{FEES_PROGRAM_ID, TOKEN_PROGRAM_ID, fees_config_actor, schedule_account, treasury_ata};

/// The chain's execution cap per transaction and per block (rc3 fee market `MAX_GAS_EXEC`).
pub const CYCLE_CAP: u64 = 10_000_000;
/// Where the tests deploy `vesting`. A second deployment of the same ELF uses [`COPY`].
pub const VESTING: AccountId = AccountId::new(*b"cliff-veil/test/vesting/00000000");
/// A second deployment of the same `vesting` ELF.
pub const COPY: AccountId = AccountId::new(*b"cliff-veil/test/vesting-copy/000");

pub const T0: u64 = 1_700_000_000_000;
pub const DAY: u64 = 86_400_000;

/// The people in every test.
pub struct Parties {
    pub creator: PrivateKey,
    pub beneficiary: PrivateKey,
    pub stranger: PrivateKey,
    pub recipient: PrivateKey,
    pub canceller: PrivateKey,
    pub signaller: PrivateKey,
    pub treasury: PrivateKey,
    pub heir: PrivateKey,
    pub refund: PrivateKey,
}

#[must_use]
pub fn parties() -> Parties {
    Parties {
        creator: key(1),
        beneficiary: key(2),
        stranger: key(3),
        recipient: key(4),
        canceller: key(5),
        signaller: key(6),
        treasury: key(7),
        heir: key(8),
        refund: key(9),
    }
}

#[must_use]
pub fn key(byte: u8) -> PrivateKey {
    PrivateKey::try_new([byte; 32]).expect("valid test key")
}

#[must_use]
pub fn id(key: &PrivateKey) -> AccountId {
    AccountId::from(&PublicKey::new_from_private_key(key))
}

#[must_use]
pub fn core_id(id: AccountId) -> vesting_core::AccountId {
    vesting_core::AccountId::new(id.into_value())
}

#[must_use]
pub fn cid(key: &PrivateKey) -> vesting_core::AccountId {
    core_id(id(key))
}

#[must_use]
pub fn lez(id: vesting_core::AccountId) -> AccountId {
    AccountId::new(id.into_value())
}

#[must_use]
pub fn sid(n: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..4].copy_from_slice(&n.to_le_bytes());
    out
}

/// What a transaction did.
#[derive(Debug)]
pub struct Outcome {
    pub events: Vec<TransactionEvent>,
    pub cycles: u64,
}

impl Outcome {
    /// The events `vesting` emitted, in order.
    #[must_use]
    pub fn vesting_events(&self, program: AccountId) -> Vec<&TransactionEvent> {
        self.events.iter().filter(|e| e.account_id == program).collect()
    }

    /// The only event of type `E` emitted by `program`, decoded; checks its selector.
    #[must_use]
    pub fn one<E: Event + borsh::BorshDeserialize>(&self, program: AccountId) -> E {
        let mine: Vec<_> = self
            .vesting_events(program)
            .into_iter()
            .filter(|e| e.event.selector == E::SELECTOR)
            .collect();
        assert_eq!(mine.len(), 1, "exactly one {} event: {:?}", E::NAME, self.events);
        borsh::from_slice(&mine[0].event.data).expect("event data decodes with the SPEC 7 schema")
    }
}

pub type Sent = Result<Outcome, LeeError>;

/// The refusal code of a failed transaction (SPEC 5); panics when it succeeded or failed without a program code.
#[must_use]
pub fn code(sent: &Sent) -> u32 {
    match sent {
        Err(LeeError::ProgramExecutionFailed(text)) => {
            parse_code(text).unwrap_or_else(|| panic!("no cliff-veil error code in: {text}"))
        }
        other => panic!("expected a program refusal, got {other:?}"),
    }
}

/// The chain.
pub struct Chain {
    pub state: V03State,
    pub vesting: AccountId,
    pub block: u64,
    pub now: u64,
    /// Admission evidence added to every transaction (keys of accounts that may not exist yet).
    pub evidence: Vec<PublicAccountEvidence>,
}

impl Chain {
    /// `vesting` at [`VESTING`], `vesting_fees` and the token program at the ids `vesting` was built with, the clock
    /// at `now`, and an initialised fee config at rate 0. `None` when the guests were not built
    /// (`RISC0_SKIP_BUILD=1`): the caller skips the test, unless `CV_REQUIRE_GUESTS=1` (CI), which fails it instead.
    #[must_use]
    pub fn new(now: u64) -> Option<Self> {
        Self::with_programs(now, &[(VESTING, vesting_methods::VESTING_ELF)])
    }

    /// As [`Self::new`], with the given deployments of `vesting` (the first is the one the helpers use).
    #[must_use]
    pub fn with_programs(now: u64, vestings: &[(AccountId, &'static [u8])]) -> Option<Self> {
        if vesting_methods::VESTING_ELF.is_empty() {
            assert_ne!(
                std::env::var("CV_REQUIRE_GUESTS").as_deref(),
                Ok("1"),
                "CV_REQUIRE_GUESTS=1 but the guests were not built (unset RISC0_SKIP_BUILD)"
            );
            assert_eq!(
                std::env::var("RISC0_SKIP_BUILD").as_deref(),
                Ok("1"),
                "the vesting guest is empty although RISC0_SKIP_BUILD is not set"
            );
            eprintln!("guests not built (RISC0_SKIP_BUILD=1): executor test skipped");
            return None;
        }
        let load = |elf: &'static [u8]| Program::new(Cow::Borrowed(elf)).expect("the guest loads");
        let mut named: Vec<(AccountId, Program)> = vestings.iter().map(|(id, elf)| (*id, load(elf))).collect();
        named.push((FEES_PROGRAM_ID, load(vesting_methods::VESTING_FEES_ELF)));
        named.push((TOKEN_PROGRAM_ID, load(vesting_methods::TOKEN_ELF)));
        named.push((clock_account_id(), programs::clock()));
        let clock = ClockAccountData {
            block_id: 1,
            timestamp: now,
        };
        let state = V03State::new().with_named_programs(named).with_public_accounts([(
            CLOCK_PROGRAM_ACCOUNT_ID,
            Account::default().with_actor_state(clock_account_id(), clock.to_bytes().into()),
        )]);
        let p = parties();
        let evidence = [
            &p.beneficiary,
            &p.stranger,
            &p.recipient,
            &p.canceller,
            &p.signaller,
            &p.treasury,
            &p.heir,
            &p.refund,
        ]
        .into_iter()
        .map(key_evidence)
        .collect();
        let mut chain = Self {
            state,
            vesting: vestings[0].0,
            block: 1,
            now,
            evidence,
        };
        chain.set_fee_config(0, AccountId::new([0; 32]));
        Some(chain)
    }

    /// Writes the `vesting_fees` config (test setup: the state `Init` then `SetFee` would leave).
    pub fn set_fee_config(&mut self, rate_bps: u16, treasury: AccountId) {
        let config = FeeConfig {
            rate_bps,
            treasury,
            ..FeeConfig::default()
        };
        let actor = fees_config_actor();
        let mut account = self.state.get_account_by_id(actor.account_id);
        account
            .data
            .set_actor_state(FEES_PROGRAM_ID, borsh::to_vec(&config).expect("config encodes").into());
        self.state.force_insert_account(actor.account_id, account);
    }

    /// Ends the block and moves the clock to `now`, as the sequencer's last transaction of a block does.
    pub fn tick(&mut self, now: u64) {
        self.block += 1;
        self.now = now;
        let msg = clock_core::Message::Tick {
            timestamp: now,
            block_id: self.block,
        };
        let tx = self.transaction(clock_actor(), &[clock_actor()], &[], &msg);
        let (diff, _) =
            ValidatedStateDiff::from_public_transaction_with_cycle_budget(&tx, &self.state, self.block, now, CYCLE_CAP)
                .expect("the clock ticks");
        self.state.apply_state_diff(diff).expect("tick applies");
    }

    /// Gives `id` a native balance (test setup).
    pub fn fund_native(&mut self, id: AccountId, amount: u128) {
        let mut account = self.state.get_account_by_id(id);
        account
            .data
            .set_actor_state(NATIVE_TOKEN_PROGRAM_ID, native_token::encode_balance(amount));
        self.state.force_insert_account(id, account);
    }

    #[must_use]
    pub fn native(&self, id: AccountId) -> u128 {
        native_token::decode_balance(
            self.state
                .get_account_by_id(id)
                .data
                .actor_state(NATIVE_TOKEN_PROGRAM_ID),
        )
        .expect("canonical balance")
    }

    #[must_use]
    pub fn token(&self, id: AccountId) -> Option<u128> {
        match TokenState::decode(self.state.get_account_by_id(id).data.actor_state(TOKEN_PROGRAM_ID)) {
            Some(TokenState::Holding(holding)) => Some(holding.balance),
            _ => None,
        }
    }

    /// The balance of `id` in `asset`.
    #[must_use]
    pub fn balance(&self, asset: Asset, id: AccountId) -> u128 {
        match asset {
            Asset::Native => self.native(id),
            Asset::Token { .. } => self.token(id).unwrap_or(0),
        }
    }

    /// The schedule at `schedule`, if any.
    #[must_use]
    pub fn schedule(&self, schedule: AccountId) -> Option<ScheduleV1> {
        let bytes = self
            .state
            .get_account_by_id(schedule)
            .data
            .actor_state(self.vesting)
            .clone();
        (!bytes.is_empty()).then(|| ScheduleState::decode_v1(&bytes).expect("schedule decodes"))
    }

    /// Runs a public transaction at the current block and clock time under the chain cap.
    pub fn submit(
        &mut self,
        root: Actor,
        declared: &[Actor],
        signers: &[&PrivateKey],
        message: &impl BorshSerialize,
    ) -> Sent {
        let now = self.now;
        self.submit_at(root, declared, signers, message, now, CYCLE_CAP)
    }

    /// [`Self::submit`] in a block with timestamp `timestamp` and a cycle budget of `budget`.
    pub fn submit_at(
        &mut self,
        root: Actor,
        declared: &[Actor],
        signers: &[&PrivateKey],
        message: &impl BorshSerialize,
        timestamp: u64,
        budget: u64,
    ) -> Sent {
        let tx = self.transaction(root, declared, signers, message);
        let (diff, charge) = ValidatedStateDiff::from_public_transaction_with_cycle_budget(
            &tx,
            &self.state,
            self.block,
            timestamp,
            budget,
        )?;
        let events = self.state.apply_state_diff(diff)?;
        Ok(Outcome {
            events,
            cycles: charge.cycles,
        })
    }

    #[must_use]
    pub fn transaction(
        &self,
        root: Actor,
        declared: &[Actor],
        signers: &[&PrivateKey],
        message: &impl BorshSerialize,
    ) -> PublicTransaction {
        let nonces = signers
            .iter()
            .map(|k| {
                let account = id(k);
                (account, self.state.get_account_by_id(account).nonce)
            })
            .collect();
        let mut msg = TxMessage::try_new(root, declared.iter().copied(), nonces, message).expect("message encodes");
        msg.admission_evidence = self.evidence.clone();
        let witness_set = WitnessSet::for_message(&msg, signers);
        PublicTransaction::new(msg, witness_set)
    }

    /// The caller actor ("seat") of `account`.
    #[must_use]
    pub const fn seat(&self, account: AccountId) -> Actor {
        Actor::new(account, self.vesting)
    }

    #[must_use]
    pub fn schedule_of(&self, creator: &PrivateKey, n: u32) -> AccountId {
        schedule_account(&self.vesting, &cid(creator), &sid(n))
    }

    /// The actor holding `asset` on `account`.
    #[must_use]
    pub const fn asset_actor(asset: Asset, account: AccountId) -> Actor {
        match asset {
            Asset::Native => Actor::native_balance(account),
            Asset::Token { .. } => Actor::new(account, TOKEN_PROGRAM_ID),
        }
    }

    // ------------------------------------------------------------------------------------ messages (SPEC 3.2)

    /// `Create` by `creator`, funded from the creator's own account.
    pub fn create(&mut self, creator: &PrivateKey, n: u32, terms: Terms, fee: u128, treasury: AccountId) -> Sent {
        let source = id(creator);
        let schedule = self.schedule_of(creator, n);
        let asset = terms.asset;
        let mut declared = vec![self.seat(source), Actor::new(schedule, self.vesting)];
        match asset {
            Asset::Native => {
                declared.push(Actor::native_balance(source));
                declared.push(Actor::native_balance(schedule));
            }
            Asset::Token { definition } => {
                declared.push(Actor::new(schedule, TOKEN_PROGRAM_ID));
                declared.push(Actor::new(source, TOKEN_PROGRAM_ID));
                declared.push(Actor::new(lez(definition), TOKEN_PROGRAM_ID));
            }
        }
        declared.push(fees_config_actor());
        if fee > 0 {
            declared.push(fee_actor(asset, treasury));
        }
        let message = Message::Create {
            schedule_id: sid(n),
            terms,
            source: core_id(source),
            fee,
            treasury: core_id(treasury),
        };
        self.submit(self.seat(source), &declared, &[creator], &message)
    }

    /// `CreateBatch` by `creator` of items `first..first + count`, each with `total`.
    pub fn create_batch(
        &mut self,
        creator: &PrivateKey,
        common: BatchTerms,
        items: Vec<BatchItem>,
        fee: u128,
        treasury: AccountId,
        budget: u64,
    ) -> Sent {
        let source = id(creator);
        let asset = common.asset;
        let mut declared = vec![self.seat(source)];
        for item in &items {
            let schedule = schedule_account(&self.vesting, &cid(creator), &item.schedule_id);
            declared.push(Actor::new(schedule, self.vesting));
            declared.push(Self::asset_actor(asset, schedule));
        }
        declared.push(Self::asset_actor(asset, source));
        if let Asset::Token { definition } = asset {
            declared.push(Actor::new(lez(definition), TOKEN_PROGRAM_ID));
        }
        declared.push(fees_config_actor());
        if fee > 0 {
            declared.push(fee_actor(asset, treasury));
        }
        let message = Message::CreateBatch {
            common,
            items,
            source: core_id(source),
            fee,
            treasury: core_id(treasury),
        };
        let now = self.now;
        self.submit_at(self.seat(source), &declared, &[creator], &message, now, budget)
    }

    /// `Claim` by `signer` (normally the beneficiary) of `schedule` to `to`.
    pub fn claim(&mut self, signer: &PrivateKey, schedule: AccountId, to: AccountId) -> Sent {
        let asset = self.schedule(schedule).map_or(Asset::Native, |s| s.asset);
        let who = id(signer);
        let declared = [
            self.seat(who),
            Actor::new(schedule, self.vesting),
            clock_actor(),
            Self::asset_actor(asset, schedule),
            Self::asset_actor(asset, to),
        ];
        let message = Message::Claim {
            schedule: core_id(schedule),
            to: core_id(to),
        };
        self.submit(self.seat(who), &declared, &[signer], &message)
    }

    /// `ClaimAsOf` by `signer` at `as_of`, included in a block with timestamp `block_time`.
    pub fn claim_as_of(
        &mut self,
        signer: &PrivateKey,
        schedule: AccountId,
        to: AccountId,
        as_of: u64,
        block_time: u64,
    ) -> Sent {
        let asset = self.schedule(schedule).map_or(Asset::Native, |s| s.asset);
        let who = id(signer);
        let declared = [
            self.seat(who),
            Actor::new(schedule, self.vesting),
            Self::asset_actor(asset, schedule),
            Self::asset_actor(asset, to),
        ];
        let message = Message::ClaimAsOf {
            schedule: core_id(schedule),
            to: core_id(to),
            as_of,
        };
        self.submit_at(self.seat(who), &declared, &[signer], &message, block_time, CYCLE_CAP)
    }

    /// `Cancel` by `signer`.
    pub fn cancel(&mut self, signer: &PrivateKey, schedule: AccountId) -> Sent {
        let s = self.schedule(schedule);
        let asset = s.as_ref().map_or(Asset::Native, |s| s.asset);
        let refund_to = s.map_or(id(signer), |s| lez(s.refund_to));
        let who = id(signer);
        let declared = [
            self.seat(who),
            Actor::new(schedule, self.vesting),
            clock_actor(),
            Self::asset_actor(asset, schedule),
            Self::asset_actor(asset, refund_to),
        ];
        let message = Message::Cancel {
            schedule: core_id(schedule),
        };
        self.submit(self.seat(who), &declared, &[signer], &message)
    }

    /// A message with the short shape (root, schedule): `MakeNonCancelable`, `SignalMilestone`,
    /// `TransferBeneficiary`.
    pub fn simple(&mut self, signer: &PrivateKey, schedule: AccountId, message: &Message) -> Sent {
        let who = id(signer);
        let declared = [self.seat(who), Actor::new(schedule, self.vesting)];
        self.submit(self.seat(who), &declared, &[signer], message)
    }

    pub fn make_non_cancelable(&mut self, signer: &PrivateKey, schedule: AccountId) -> Sent {
        let message = Message::MakeNonCancelable {
            schedule: core_id(schedule),
        };
        self.simple(signer, schedule, &message)
    }

    pub fn signal(&mut self, signer: &PrivateKey, schedule: AccountId, index: u8) -> Sent {
        let message = Message::SignalMilestone {
            schedule: core_id(schedule),
            index,
        };
        self.simple(signer, schedule, &message)
    }

    pub fn transfer(&mut self, signer: &PrivateKey, schedule: AccountId, new_beneficiary: AccountId) -> Sent {
        let message = Message::TransferBeneficiary {
            schedule: core_id(schedule),
            new_beneficiary: core_id(new_beneficiary),
        };
        self.simple(signer, schedule, &message)
    }

    // ------------------------------------------------------------------------------------ token setup

    /// A new token definition `def_key` with `supply` minted to `holder` (both sign).
    pub fn new_token(&mut self, def_key: &PrivateKey, holder: &PrivateKey, supply: u128) -> AccountId {
        let def = id(def_key);
        let h = id(holder);
        let msg = token_core::Message::NewDefinition {
            name: "VEST".to_owned(),
            total_supply: supply,
            mint_authority: None,
            holding: h,
        };
        let root = Actor::new(def, TOKEN_PROGRAM_ID);
        self.submit(root, &[root, Actor::new(h, TOKEN_PROGRAM_ID)], &[def_key, holder], &msg)
            .expect("token definition created");
        def
    }

    /// `holder` initialises its own holding of `def` (an earlier transaction of its own, SPEC 3.1).
    pub fn init_holding(&mut self, holder: &PrivateKey, def: AccountId) {
        let h = Actor::new(id(holder), TOKEN_PROGRAM_ID);
        let msg = token_core::Message::InitializeHolding { definition: def };
        self.submit(h, &[h, Actor::new(def, TOKEN_PROGRAM_ID)], &[holder], &msg)
            .expect("holding initialised");
    }

    /// Writes an empty holding of `def` at the treasury's associated token account (test setup: the state the ATA
    /// program's `Create` leaves). Returns the ATA.
    pub fn put_treasury_ata(&mut self, treasury: AccountId, def: AccountId) -> AccountId {
        let ata = treasury_ata(core_id(treasury), core_id(def));
        let holding = TokenState::Holding(TokenHolding {
            definition_id: def,
            balance: 0,
        });
        let mut account = self.state.get_account_by_id(ata);
        account.data.set_actor_state(TOKEN_PROGRAM_ID, holding.encode().into());
        self.state.force_insert_account(ata, account);
        ata
    }
}

/// The actor credited with a non-zero fee (SPEC 6).
#[must_use]
pub fn fee_actor(asset: Asset, treasury: AccountId) -> Actor {
    match asset {
        Asset::Native => Actor::native_balance(treasury),
        Asset::Token { definition } => Actor::new(treasury_ata(core_id(treasury), definition), TOKEN_PROGRAM_ID),
    }
}

#[must_use]
pub fn key_evidence(key: &PrivateKey) -> PublicAccountEvidence {
    PublicAccountEvidence::Key(PublicKey::new_from_private_key(key))
}

// ---------------------------------------------------------------------------------------------- terms

/// Linear terms for `beneficiary`: `total` over `[start, end)` with the cliff at `cliff`.
#[must_use]
pub fn linear(beneficiary: &PrivateKey, asset: Asset, total: u128, start: u64, cliff: u64, end: u64) -> Terms {
    Terms {
        beneficiary: cid(beneficiary),
        asset,
        total,
        kind: Kind::Linear { start, cliff, end },
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    }
}

/// Milestone terms for `beneficiary`.
#[must_use]
pub fn milestones(beneficiary: &PrivateKey, asset: Asset, amounts: &[u128]) -> Terms {
    Terms {
        beneficiary: cid(beneficiary),
        asset,
        total: amounts.iter().sum(),
        kind: Kind::Milestones {
            amounts: amounts.to_vec(),
            signalled: 0,
        },
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    }
}

/// Batch terms: linear over `[T0, T0 + 100 days)` with a 10-day cliff.
#[must_use]
pub fn batch_linear(asset: Asset) -> BatchTerms {
    BatchTerms {
        asset,
        shape: BatchShape::Linear {
            start: T0,
            cliff: T0 + 10 * DAY,
            end: T0 + 100 * DAY,
        },
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    }
}

/// Batch terms of the heaviest item: 64 milestones (the most a schedule may have).
#[must_use]
pub fn batch_heaviest(asset: Asset) -> BatchTerms {
    let mut bps = vec![156u16; 63];
    bps.push(10_000 - 156 * 63);
    BatchTerms {
        shape: BatchShape::Milestones { bps },
        ..batch_linear(asset)
    }
}

/// `count` batch items with ids `first..`, distinct beneficiaries and `total` each.
#[must_use]
pub fn batch_items(first: u32, count: u32, total: u128) -> Vec<BatchItem> {
    (first..first + count)
        .map(|n| {
            let mut beneficiary = [0xb0u8; 32];
            beneficiary[..4].copy_from_slice(&n.to_le_bytes());
            BatchItem {
                schedule_id: sid(n),
                beneficiary: vesting_core::AccountId::new(beneficiary),
                total,
            }
        })
        .collect()
}
