//! A thin transaction builder for the `vesting` program: the messages of SPEC section 3.1, encoded with
//! `vesting_core`, and the declared actors of SPEC section 3.2. The SDK builds the same transactions; the tests use
//! this small copy so that they depend only on the program and on the frozen interface.

use anyhow::{Context as _, Result};
use lee_core::program::PdaSeed;
use lee_core::{account::AccountId, native_token::NATIVE_TOKEN_PROGRAM_ID};
use vesting_core::{Asset, BatchItem, BatchTerms, Kind, Message, ScheduleV1, Terms};
use wallet::{AccountIdentity, AccountMention};

use crate::{Ctx, Included, SendError};

/// `vesting_core` account id of a LEZ account id.
#[must_use]
pub const fn core_id(id: AccountId) -> vesting_core::AccountId {
    vesting_core::AccountId::new(*id.value())
}

/// LEZ account id of a `vesting_core` account id.
#[must_use]
pub const fn lez_id(id: vesting_core::AccountId) -> AccountId {
    AccountId::new(*id.value())
}

/// The deployed programs a vesting transaction reaches.
#[derive(Debug, Clone, Copy)]
pub struct Programs {
    /// `vesting`.
    pub vesting: AccountId,
    /// The token program, when deployed.
    pub token: Option<AccountId>,
    /// `vesting_fees` and its config account, when deployed.
    pub fees: Option<(AccountId, AccountId)>,
}

impl Programs {
    /// From the ids file: `vesting` (required), `token`, `vesting_fees` with `fees_config`.
    ///
    /// # Errors
    /// `vesting` is not deployed.
    pub fn from_ctx(ctx: &Ctx) -> Result<Self> {
        Ok(Self {
            vesting: ctx.ids.require("vesting")?,
            token: ctx.ids.get("token"),
            fees: ctx.ids.get("vesting_fees").zip(ctx.ids.get("fees_config")),
        })
    }

    /// The schedule account of `(creator, schedule_id)` (SPEC 1.2).
    #[must_use]
    pub fn schedule_account(&self, creator: AccountId, schedule_id: &[u8; 32]) -> AccountId {
        let seed = vesting_core::schedule_seed(&core_id(creator), schedule_id);
        AccountId::for_public_pda(&self.vesting, &PdaSeed::new(seed))
    }

    fn seat(&self, caller: AccountId) -> AccountMention {
        AccountIdentity::Public(caller).select_program_actor_state(self.vesting)
    }

    fn schedule(&self, schedule: AccountId) -> AccountMention {
        AccountIdentity::PublicNoSign(schedule).select_program_actor_state(self.vesting)
    }

    fn clock() -> AccountMention {
        AccountIdentity::PublicNoSign(clock_core::CLOCK_PROGRAM_ACCOUNT_ID)
            .select_program_actor_state(clock_core::clock_account_id())
    }

    /// The actor holding `asset` on `account`: its native balance or its token holding.
    fn holding(&self, account: AccountId, asset: &Asset) -> Result<AccountMention> {
        let identity = AccountIdentity::PublicNoSign(account);
        Ok(match asset {
            Asset::Native => identity.select_program_actor_state(NATIVE_TOKEN_PROGRAM_ID),
            Asset::Token { .. } => {
                identity.select_program_actor_state(self.token.context("token program not deployed")?)
            }
        })
    }

    /// A payout actor (destination or refund account). When it is the signer it is declared as the signer's own
    /// account, so one account never appears with two identities (LEZ refuses that), as the SDK does.
    fn payout(&self, account: AccountId, asset: &Asset, signer: AccountId) -> Result<AccountMention> {
        if account == signer {
            let program = match asset {
                Asset::Native => NATIVE_TOKEN_PROGRAM_ID,
                Asset::Token { .. } => self.token.context("token program not deployed")?,
            };
            Ok(AccountIdentity::Public(account).select_program_actor_state(program))
        } else {
            self.holding(account, asset)
        }
    }

    fn fee_actors(&self, out: &mut Vec<AccountMention>) {
        if let Some((program, config)) = self.fees {
            out.push(AccountIdentity::PublicNoSign(config).select_program_actor_state(program));
        }
    }

    /// `Create` with a zero fee (SPEC 3.1 tag 0, shapes in 3.2). The creator funds from its own account.
    ///
    /// # Errors
    /// A token schedule without a deployed token program.
    pub fn create(&self, creator: AccountId, schedule_id: [u8; 32], terms: Terms) -> Result<Tx> {
        self.create_with_fee(creator, schedule_id, terms, 0, creator)
    }

    /// `Create` quoting `fee` to `treasury` (native: `native(treasury)` is declared when `fee > 0`).
    ///
    /// # Errors
    /// A token schedule without a deployed token program, or a token schedule with a fee (not built here).
    pub fn create_with_fee(
        &self,
        creator: AccountId,
        schedule_id: [u8; 32],
        terms: Terms,
        fee: u128,
        treasury: AccountId,
    ) -> Result<Tx> {
        let schedule = self.schedule_account(creator, &schedule_id);
        let mut accounts = vec![self.seat(creator), self.schedule(schedule)];
        match terms.asset {
            Asset::Native => {
                accounts.push(AccountIdentity::Public(creator).balance());
                accounts.push(self.holding(schedule, &terms.asset)?);
            }
            Asset::Token { definition } => {
                let token = self.token.context("token program not deployed")?;
                accounts.push(self.holding(schedule, &terms.asset)?);
                accounts.push(AccountIdentity::Public(creator).select_program_actor_state(token));
                accounts.push(AccountIdentity::PublicNoSign(lez_id(definition)).select_program_actor_state(token));
            }
        }
        self.fee_actors(&mut accounts);
        if fee > 0 {
            anyhow::ensure!(
                terms.asset == Asset::Native,
                "a token fee needs the treasury's ATA (not built here)"
            );
            accounts.push(AccountIdentity::PublicNoSign(treasury).balance());
        }
        let message = Message::Create {
            schedule_id,
            terms,
            source: core_id(creator),
            fee,
            treasury: core_id(treasury),
        };
        Ok(Tx {
            accounts,
            message,
            schedules: vec![schedule],
        })
    }

    /// `CreateBatch` with a zero fee (SPEC 3.1 tag 1).
    ///
    /// # Errors
    /// A token batch without a deployed token program.
    pub fn create_batch(&self, creator: AccountId, common: BatchTerms, items: Vec<BatchItem>) -> Result<Tx> {
        let mut accounts = vec![self.seat(creator)];
        let mut schedules = Vec::with_capacity(items.len());
        for item in &items {
            let schedule = self.schedule_account(creator, &item.schedule_id);
            accounts.push(self.schedule(schedule));
            accounts.push(self.holding(schedule, &common.asset)?);
            schedules.push(schedule);
        }
        match common.asset {
            Asset::Native => accounts.push(AccountIdentity::Public(creator).balance()),
            Asset::Token { definition } => {
                let token = self.token.context("token program not deployed")?;
                accounts.push(AccountIdentity::Public(creator).select_program_actor_state(token));
                accounts.push(AccountIdentity::PublicNoSign(lez_id(definition)).select_program_actor_state(token));
            }
        }
        self.fee_actors(&mut accounts);
        let message = Message::CreateBatch {
            common,
            items,
            source: core_id(creator),
            fee: 0,
            treasury: core_id(creator),
        };
        Ok(Tx {
            accounts,
            message,
            schedules,
        })
    }

    /// Public `Claim` at clock time (tag 2).
    ///
    /// # Errors
    /// A token schedule without a deployed token program.
    pub fn claim(&self, beneficiary: AccountId, schedule: AccountId, to: AccountId, asset: &Asset) -> Result<Tx> {
        let accounts = vec![
            self.seat(beneficiary),
            self.schedule(schedule),
            Self::clock(),
            self.holding(schedule, asset)?,
            self.payout(to, asset, beneficiary)?,
        ];
        let message = Message::Claim {
            schedule: core_id(schedule),
            to: core_id(to),
        };
        Ok(Tx {
            accounts,
            message,
            schedules: vec![schedule],
        })
    }

    /// `ClaimAsOf` to a public destination (tag 3).
    ///
    /// # Errors
    /// A token schedule without a deployed token program.
    pub fn claim_as_of(
        &self,
        beneficiary: AccountId,
        schedule: AccountId,
        to: AccountId,
        as_of: u64,
        asset: &Asset,
    ) -> Result<Tx> {
        let accounts = vec![
            self.seat(beneficiary),
            self.schedule(schedule),
            self.holding(schedule, asset)?,
            self.payout(to, asset, beneficiary)?,
        ];
        let message = Message::ClaimAsOf {
            schedule: core_id(schedule),
            to: core_id(to),
            as_of,
        };
        Ok(Tx {
            accounts,
            message,
            schedules: vec![schedule],
        })
    }

    /// `Cancel` (tag 4); the refund goes to the schedule's `refund_to`.
    ///
    /// # Errors
    /// A token schedule without a deployed token program.
    pub fn cancel(&self, authority: AccountId, schedule: AccountId, state: &ScheduleV1) -> Result<Tx> {
        let accounts = vec![
            self.seat(authority),
            self.schedule(schedule),
            Self::clock(),
            self.holding(schedule, &state.asset)?,
            self.payout(lez_id(state.refund_to), &state.asset, authority)?,
        ];
        Ok(Tx {
            accounts,
            message: Message::Cancel {
                schedule: core_id(schedule),
            },
            schedules: vec![schedule],
        })
    }

    /// `MakeNonCancelable` (tag 5).
    #[must_use]
    pub fn make_non_cancelable(&self, caller: AccountId, schedule: AccountId) -> Tx {
        self.simple(
            caller,
            schedule,
            Message::MakeNonCancelable {
                schedule: core_id(schedule),
            },
        )
    }

    /// `SignalMilestone` (tag 6).
    #[must_use]
    pub fn signal(&self, caller: AccountId, schedule: AccountId, index: u8) -> Tx {
        self.simple(
            caller,
            schedule,
            Message::SignalMilestone {
                schedule: core_id(schedule),
                index,
            },
        )
    }

    /// `TransferBeneficiary` (tag 7).
    #[must_use]
    pub fn transfer(&self, caller: AccountId, schedule: AccountId, new_beneficiary: AccountId) -> Tx {
        self.simple(
            caller,
            schedule,
            Message::TransferBeneficiary {
                schedule: core_id(schedule),
                new_beneficiary: core_id(new_beneficiary),
            },
        )
    }

    fn simple(&self, caller: AccountId, schedule: AccountId, message: Message) -> Tx {
        Tx {
            accounts: vec![self.seat(caller), self.schedule(schedule)],
            message,
            schedules: vec![schedule],
        }
    }
}

/// One vesting transaction, ready to send.
#[derive(Clone)]
pub struct Tx {
    /// Declared actors; the first is the root.
    pub accounts: Vec<AccountMention>,
    /// The message to the root actor.
    pub message: Message,
    /// The schedule accounts it touches (for a create: the ones it opens).
    pub schedules: Vec<AccountId>,
}

impl Tx {
    /// Sends it through the context's wallet and waits for inclusion.
    ///
    /// # Errors
    /// As [`Ctx::send`].
    pub async fn send(self, ctx: &Ctx) -> Result<Included, SendError> {
        ctx.send(self.accounts, self.message.encode()).await
    }

    /// Sends it even if it reverts (it then pays its fee and changes nothing else).
    ///
    /// # Errors
    /// As [`Ctx::send_even_if_reverting`].
    pub async fn send_even_if_reverting(self, ctx: &mut Ctx) -> Result<Included, SendError> {
        ctx.send_even_if_reverting(self.accounts, self.message.encode()).await
    }
}

/// Terms of a native cliff + linear schedule with the RFP's defaults (cancelable, not transferable, authorities and
/// refund account defaulted).
#[must_use]
pub fn linear(beneficiary: AccountId, total: u128, start: u64, cliff: u64, end: u64) -> Terms {
    Terms {
        beneficiary: core_id(beneficiary),
        asset: Asset::Native,
        total,
        kind: Kind::Linear { start, cliff, end },
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    }
}

/// Terms of a native milestone schedule with the RFP's defaults.
#[must_use]
pub fn milestones(beneficiary: AccountId, amounts: Vec<u128>) -> Terms {
    Terms {
        beneficiary: core_id(beneficiary),
        asset: Asset::Native,
        total: amounts.iter().sum(),
        kind: Kind::Milestones { amounts, signalled: 0 },
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
        refund_to: None,
    }
}

impl Ctx {
    /// The deployed programs.
    ///
    /// # Errors
    /// `vesting` is not deployed.
    pub fn programs(&self) -> Result<Programs> {
        Programs::from_ctx(self)
    }

    /// The schedule state at `schedule`, `None` when the actor is empty.
    ///
    /// # Errors
    /// RPC failure or a state that does not decode.
    pub async fn schedule(&self, programs: &Programs, schedule: AccountId) -> Result<Option<ScheduleV1>> {
        let bytes = self.actor_state(schedule, programs.vesting).await?;
        if bytes.is_empty() {
            return Ok(None);
        }
        vesting_core::ScheduleState::decode_v1(&bytes)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("schedule {schedule} does not decode: {e:?}"))
    }

    /// Creates a schedule from `creator` with a fresh id; returns its account.
    ///
    /// # Errors
    /// The creation was refused or not included.
    pub async fn create(&mut self, programs: &Programs, creator: &str, terms: Terms) -> Result<AccountId> {
        let id = self.fresh_id();
        let tx = programs.create(self.id(creator), id, terms)?;
        let schedule = tx.schedules[0];
        tx.send(self).await?;
        Ok(schedule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn programs() -> Programs {
        Programs {
            vesting: AccountId::new([5; 32]),
            token: None,
            fees: None,
        }
    }

    #[test]
    fn schedule_account_depends_on_creator_and_id() {
        let p = programs();
        let a = p.schedule_account(AccountId::new([1; 32]), &[2; 32]);
        assert_eq!(a, p.schedule_account(AccountId::new([1; 32]), &[2; 32]));
        assert_ne!(a, p.schedule_account(AccountId::new([1; 32]), &[3; 32]));
        assert_ne!(a, p.schedule_account(AccountId::new([9; 32]), &[2; 32]));
    }

    #[test]
    fn claim_declares_the_spec_actors_in_order() {
        let p = programs();
        let (ben, sch, to) = (
            AccountId::new([1; 32]),
            AccountId::new([2; 32]),
            AccountId::new([3; 32]),
        );
        let tx = p.claim(ben, sch, to, &Asset::Native).expect("native claim");
        let actors: Vec<_> = tx.accounts.iter().map(AccountMention::actor).collect();
        assert_eq!(
            actors,
            vec![
                lee_core::account::Actor::new(ben, p.vesting),
                lee_core::account::Actor::new(sch, p.vesting),
                clock_core::clock_actor(),
                lee_core::account::Actor::native_balance(sch),
                lee_core::account::Actor::native_balance(to),
            ]
        );
        assert_eq!(tx.message.encode()[0], 2, "Claim is tag 2");
    }

    #[test]
    fn token_schedules_need_the_token_program() {
        let p = programs();
        let terms = Terms {
            asset: Asset::Token {
                definition: vesting_core::AccountId::new([8; 32]),
            },
            ..linear(AccountId::new([1; 32]), 10, 0, 0, 10)
        };
        assert!(p.create(AccountId::new([2; 32]), [0; 32], terms).is_err());
    }

    #[test]
    fn id_conversions_roundtrip() {
        let id = AccountId::new([42; 32]);
        assert_eq!(lez_id(core_id(id)), id);
    }
}
