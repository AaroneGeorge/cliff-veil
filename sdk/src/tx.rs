//! Transactions of the `vesting` program: the messages of SPEC section 3.1, encoded with `vesting_core`, with the
//! declared actors of SPEC section 3.2, root first.
//!
//! This module only builds. It reads no state and sends nothing; [`crate::Client`] checks an operation against
//! current state first and a [`crate::Backend`] sends the result.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context as _, anyhow};
use lee_core::account::AccountId;
use lee_core::native_token::NATIVE_TOKEN_PROGRAM_ID;
use lee_core::program::PdaSeed;
use vesting_core::{Asset, BatchItem, BatchTerms, Message, Terms};
use wallet::{AccountIdentity, AccountMention};

/// `vesting_core` account id of a LEZ account id (same 32 bytes).
#[must_use]
pub const fn core_id(id: AccountId) -> vesting_core::AccountId {
    vesting_core::AccountId::new(*id.value())
}

/// LEZ account id of a `vesting_core` account id (same 32 bytes).
#[must_use]
pub const fn lez_id(id: vesting_core::AccountId) -> AccountId {
    AccountId::new(*id.value())
}

/// The deployed programs a vesting transaction reaches (SPEC 1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Programs {
    /// `vesting`.
    pub vesting: AccountId,
    /// `vesting_fees`, when deployed. Without it a creation declares no fee actor and quotes a zero fee.
    pub fees: Option<AccountId>,
    /// The token program, for token schedules.
    pub token: Option<AccountId>,
    /// The associated token account program, for a non-zero token fee (SPEC 6).
    pub ata: Option<AccountId>,
}

impl Programs {
    /// Only `vesting`.
    #[must_use]
    pub const fn new(vesting: AccountId) -> Self {
        Self {
            vesting,
            fees: None,
            token: None,
            ata: None,
        }
    }

    /// Reads the `[programs]` table of a `deploy/ids-<network>.toml` file (`name = "<base58 id>"`), as written by
    /// `scripts/deploy_local.sh`: `vesting` (required), `vesting_fees`, `token`, `ata`.
    ///
    /// # Errors
    /// The file is unreadable, a value is not an account id, or `vesting` is missing.
    pub fn from_ids_file(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Self::parse_ids(&text)
    }

    /// As [`Self::from_ids_file`], from the file's text.
    ///
    /// # Errors
    /// As [`Self::from_ids_file`].
    pub fn parse_ids(text: &str) -> anyhow::Result<Self> {
        let mut in_programs = false;
        let mut found: Vec<(String, AccountId)> = Vec::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') {
                in_programs = line == "[programs]";
                continue;
            }
            if !in_programs || line.is_empty() {
                continue;
            }
            let (name, value) = line
                .split_once('=')
                .with_context(|| format!("bad line in [programs]: {line}"))?;
            let id = value
                .trim()
                .trim_matches('"')
                .parse()
                .map_err(|e| anyhow!("{}: not an account id: {e:?}", name.trim()))?;
            found.push((name.trim().to_owned(), id));
        }
        let get = |n: &str| found.iter().find(|(k, _)| k == n).map(|(_, v)| *v);
        Ok(Self {
            vesting: get("vesting").context("no `vesting` in [programs]")?,
            fees: get("vesting_fees"),
            token: get("token"),
            ata: get("ata"),
        })
    }

    /// The schedule account of `(creator, schedule_id)`: `for_public_pda(vesting, schedule_seed)` (SPEC 1.2).
    #[must_use]
    pub fn schedule_account(&self, creator: AccountId, schedule_id: &[u8; 32]) -> AccountId {
        AccountId::for_public_pda(&self.vesting, &Self::schedule_seed(creator, schedule_id))
    }

    fn schedule_seed(creator: AccountId, schedule_id: &[u8; 32]) -> PdaSeed {
        PdaSeed::new(vesting_core::schedule_seed(&core_id(creator), schedule_id))
    }

    /// The `vesting_fees` config account, when the fee program is deployed.
    #[must_use]
    pub fn fees_config(&self) -> Option<AccountId> {
        self.fees.map(vesting_fees::config_account)
    }

    fn token(&self) -> anyhow::Result<AccountId> {
        self.token
            .context("the token program id is not configured (ids file: token)")
    }

    /// The program whose actor holds `asset` on an account: the native token program or the token program.
    ///
    /// # Errors
    /// A token asset without a configured token program.
    pub fn asset_program(&self, asset: &Asset) -> anyhow::Result<AccountId> {
        match asset {
            Asset::Native => Ok(NATIVE_TOKEN_PROGRAM_ID),
            Asset::Token { .. } => self.token(),
        }
    }

    /// The treasury's associated token account for `definition` (SPEC 6).
    ///
    /// # Errors
    /// The token or ATA program id is not configured.
    pub fn treasury_ata(&self, treasury: AccountId, definition: AccountId) -> anyhow::Result<AccountId> {
        let ata = self
            .ata
            .context("the ATA program id is not configured (ids file: ata)")?;
        let seed = vesting_core::ata_seed(&core_id(self.token()?), &core_id(treasury), &core_id(definition));
        Ok(AccountId::for_public_pda(&ata, &PdaSeed::new(seed)))
    }

    // ---------------------------------------------------------------- declared actors

    /// The caller's own actor `(caller, vesting)`: the root; the caller signs.
    fn seat(&self, caller: AccountId) -> AccountMention {
        AccountIdentity::Public(caller).select_program_actor_state(self.vesting)
    }

    fn schedule(&self, schedule: AccountId) -> AccountMention {
        AccountIdentity::PublicNoSign(schedule)
            .select_program_actor_state(self.vesting)
            .without_authorization()
    }

    fn clock() -> AccountMention {
        AccountIdentity::PublicNoSign(clock_core::CLOCK_PROGRAM_ACCOUNT_ID)
            .select_program_actor_state(clock_core::clock_account_id())
            .without_authorization()
    }

    /// The escrow actor of a schedule: its native balance or its token holding.
    fn escrow(&self, schedule: AccountId, asset: &Asset) -> anyhow::Result<AccountMention> {
        Ok(AccountIdentity::PublicNoSign(schedule)
            .select_program_actor_state(self.asset_program(asset)?)
            .without_authorization())
    }

    /// A payout actor (destination or refund account). When it is the signer it is declared as the signer's own
    /// account, so one account never appears with two identities.
    fn payout(&self, account: AccountId, asset: &Asset, signer: AccountId) -> anyhow::Result<AccountMention> {
        let identity = if account == signer {
            AccountIdentity::Public(account)
        } else {
            AccountIdentity::PublicNoSign(account)
        };
        let mention = identity.select_program_actor_state(self.asset_program(asset)?);
        Ok(if account == signer {
            mention
        } else {
            mention.without_authorization()
        })
    }

    fn fee_actors(
        &self,
        out: &mut Vec<AccountMention>,
        fee: u128,
        treasury: AccountId,
        asset: &Asset,
    ) -> anyhow::Result<()> {
        if let (Some(program), Some(config)) = (self.fees, self.fees_config()) {
            out.push(
                AccountIdentity::PublicNoSign(config)
                    .select_program_actor_state(program)
                    .without_authorization(),
            );
        }
        if fee > 0 {
            let actor = match asset {
                Asset::Native => AccountIdentity::PublicNoSign(treasury).balance(),
                Asset::Token { definition } => {
                    AccountIdentity::PublicNoSign(self.treasury_ata(treasury, lez_id(*definition))?)
                        .select_program_actor_state(self.token()?)
                }
            };
            out.push(actor.without_authorization());
        }
        Ok(())
    }

    /// The funding actors of a creation: `native(source)`, or `(source, token)` and `(definition, token)`.
    fn funding(&self, out: &mut Vec<AccountMention>, source: AccountId, asset: &Asset) -> anyhow::Result<()> {
        match asset {
            Asset::Native => out.push(AccountIdentity::Public(source).balance()),
            Asset::Token { definition } => {
                let token = self.token()?;
                out.push(AccountIdentity::Public(source).select_program_actor_state(token));
                out.push(
                    AccountIdentity::PublicNoSign(lez_id(*definition))
                        .select_program_actor_state(token)
                        .without_authorization(),
                );
            }
        }
        Ok(())
    }

    /// The new schedule's own actors, declared by its PDA seed so the transaction admits the new account.
    fn new_schedule(
        &self,
        creator: AccountId,
        schedule_id: &[u8; 32],
        asset: &Asset,
    ) -> anyhow::Result<[AccountMention; 2]> {
        let pda = AccountIdentity::PublicPda {
            program: self.vesting,
            seed: Self::schedule_seed(creator, schedule_id),
        };
        Ok([
            pda.clone()
                .select_program_actor_state(self.vesting)
                .without_authorization(),
            pda.select_program_actor_state(self.asset_program(asset)?)
                .without_authorization(),
        ])
    }

    // ---------------------------------------------------------------- transactions

    /// `Create` (tag 0), funded from the creator's own account. Declared actors as SPEC 3.2 "Create".
    ///
    /// # Errors
    /// A token schedule without the token program (or, with a fee, the ATA program) configured.
    pub fn create(
        &self,
        creator: AccountId,
        schedule_id: [u8; 32],
        terms: Terms,
        fee: u128,
        treasury: AccountId,
    ) -> anyhow::Result<Tx> {
        let schedule = self.schedule_account(creator, &schedule_id);
        let asset = terms.asset;
        let [state, escrow] = self.new_schedule(creator, &schedule_id, &asset)?;
        let mut accounts = vec![self.seat(creator), state];
        match asset {
            // root, (schedule, vesting), native(source), native(schedule), fees config, native(treasury)
            Asset::Native => {
                self.funding(&mut accounts, creator, &asset)?;
                accounts.push(escrow);
            }
            // root, (schedule, vesting), (schedule, token), (source, token), (definition, token), fees config, ATA
            Asset::Token { .. } => {
                accounts.push(escrow);
                self.funding(&mut accounts, creator, &asset)?;
            }
        }
        self.fee_actors(&mut accounts, fee, treasury, &asset)?;
        let message = Message::Create {
            schedule_id,
            terms,
            source: core_id(creator),
            fee,
            treasury: core_id(treasury),
        };
        Ok(Tx::public(creator, message, accounts, vec![schedule]))
    }

    /// `CreateBatch` (tag 1), funded from the creator's own account. Declared actors as SPEC 3.2 "CreateBatch".
    ///
    /// # Errors
    /// As [`Self::create`].
    pub fn create_batch(
        &self,
        creator: AccountId,
        common: BatchTerms,
        items: Vec<BatchItem>,
        fee: u128,
        treasury: AccountId,
    ) -> anyhow::Result<Tx> {
        let mut accounts = vec![self.seat(creator)];
        let mut schedules = Vec::with_capacity(items.len());
        for item in &items {
            accounts.extend(self.new_schedule(creator, &item.schedule_id, &common.asset)?);
            schedules.push(self.schedule_account(creator, &item.schedule_id));
        }
        self.funding(&mut accounts, creator, &common.asset)?;
        self.fee_actors(&mut accounts, fee, treasury, &common.asset)?;
        let message = Message::CreateBatch {
            common,
            items,
            source: core_id(creator),
            fee,
            treasury: core_id(treasury),
        };
        Ok(Tx::public(creator, message, accounts, schedules))
    }

    /// Public `Claim` at clock time (tag 2): root, `(schedule, vesting)`, clock, escrow, destination.
    ///
    /// # Errors
    /// A token schedule without the token program configured.
    pub fn claim(
        &self,
        beneficiary: AccountId,
        schedule: AccountId,
        to: AccountId,
        asset: &Asset,
    ) -> anyhow::Result<Tx> {
        let accounts = vec![
            self.seat(beneficiary),
            self.schedule(schedule),
            Self::clock(),
            self.escrow(schedule, asset)?,
            self.payout(to, asset, beneficiary)?,
        ];
        let message = Message::Claim {
            schedule: core_id(schedule),
            to: core_id(to),
        };
        Ok(Tx::public(beneficiary, message, accounts, vec![schedule]))
    }

    /// Privacy-preserving `ClaimAsOf` (tag 3) to the one-time blinded address `destination.blinded(opening)` of an
    /// owned private account (SPEC 9). The public part declares root, `(schedule, vesting)` and the escrow; the
    /// private part is the destination, which opens the blinded address inside the proof and may pay the network
    /// fee. The destination's canonical id is in no declared public actor and not in the message.
    ///
    /// # Errors
    /// A token schedule without the token program configured.
    #[expect(
        clippy::too_many_arguments,
        reason = "every input of the private claim is distinct and named at the call site"
    )]
    pub fn claim_private(
        &self,
        beneficiary: AccountId,
        schedule: AccountId,
        destination: AccountId,
        opening: [u8; 32],
        as_of: u64,
        asset: &Asset,
        fee_payer: Option<AccountId>,
    ) -> anyhow::Result<Tx> {
        let to = destination.blinded(&opening);
        let accounts = vec![
            self.seat(beneficiary),
            self.schedule(schedule),
            self.escrow(schedule, asset)?,
            AccountMention {
                identity: AccountIdentity::PrivateOwned(destination),
                program_account_id: self.asset_program(asset)?,
                // It authorises so that, for a native claim, it can pay the in-proof fee out of the claim.
                authorizes: true,
                address_openings: BTreeSet::from([opening]),
            },
        ];
        let message = Message::ClaimAsOf {
            schedule: core_id(schedule),
            to: core_id(to),
            as_of,
        };
        Ok(Tx {
            signer: beneficiary,
            message,
            accounts,
            route: Route::Private {
                destination,
                opening,
                fee_payer,
            },
            schedules: vec![schedule],
        })
    }

    /// `Cancel` (tag 4): root, `(schedule, vesting)`, clock, escrow, refund account.
    ///
    /// # Errors
    /// A token schedule without the token program configured.
    pub fn cancel(
        &self,
        authority: AccountId,
        schedule: AccountId,
        refund_to: AccountId,
        asset: &Asset,
    ) -> anyhow::Result<Tx> {
        let accounts = vec![
            self.seat(authority),
            self.schedule(schedule),
            Self::clock(),
            self.escrow(schedule, asset)?,
            self.payout(refund_to, asset, authority)?,
        ];
        let message = Message::Cancel {
            schedule: core_id(schedule),
        };
        Ok(Tx::public(authority, message, accounts, vec![schedule]))
    }

    /// `MakeNonCancelable` (tag 5): root, `(schedule, vesting)`.
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

    /// `SignalMilestone` (tag 6): root, `(schedule, vesting)`.
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

    /// `TransferBeneficiary` (tag 7): root, `(schedule, vesting)`.
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
        Tx::public(
            caller,
            message,
            vec![self.seat(caller), self.schedule(schedule)],
            vec![schedule],
        )
    }
}

/// How a transaction is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// A public transaction, signed by [`Tx::signer`].
    Public,
    /// A privacy-preserving transaction proved by this client: a claim to `destination`, reached at
    /// `destination.blinded(opening)`.
    Private {
        /// The canonical id of the owned private account that receives the claim. Never sent.
        destination: AccountId,
        /// The one-time opening of the blinded address. Never sent.
        opening: [u8; 32],
        /// The owned private account that pays the network fee; `None`: the wallet picks one (normally the
        /// destination itself, out of a native claim).
        fee_payer: Option<AccountId>,
    },
}

/// One vesting transaction, ready to price and send.
#[derive(Clone)]
pub struct Tx {
    /// The public account that signs: the account of the root actor.
    pub signer: AccountId,
    /// The message to the root actor.
    pub message: Message,
    /// Declared actors, root first (SPEC 3.2).
    pub accounts: Vec<AccountMention>,
    /// Public or privacy-preserving.
    pub route: Route,
    /// The schedule accounts it touches (for a creation: the ones it opens).
    pub schedules: Vec<AccountId>,
}

impl Tx {
    fn public(signer: AccountId, message: Message, accounts: Vec<AccountMention>, schedules: Vec<AccountId>) -> Self {
        Self {
            signer,
            message,
            accounts,
            route: Route::Public,
            schedules,
        }
    }

    /// The actors the transaction declares, root first.
    #[must_use]
    pub fn actors(&self) -> Vec<lee_core::account::Actor> {
        self.accounts.iter().map(AccountMention::actor).collect()
    }

    /// `true` for a privacy-preserving transaction.
    #[must_use]
    pub const fn is_private(&self) -> bool {
        matches!(self.route, Route::Private { .. })
    }
}

impl std::fmt::Debug for Tx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tx")
            .field("signer", &self.signer)
            .field("message", &self.message)
            .field("actors", &self.actors())
            .field("route", &self.route)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use lee_core::account::Actor;
    use vesting_core::Kind;

    use super::*;

    fn programs() -> Programs {
        Programs {
            vesting: AccountId::new([5; 32]),
            fees: Some(AccountId::new([6; 32])),
            token: Some(AccountId::new([7; 32])),
            ata: Some(AccountId::new([8; 32])),
        }
    }

    fn terms(beneficiary: AccountId, asset: Asset) -> Terms {
        Terms {
            beneficiary: core_id(beneficiary),
            asset,
            total: 100,
            kind: Kind::Linear {
                start: 0,
                cliff: 0,
                end: 10,
            },
            cancelable: true,
            transferable: false,
            cancel_authority: None,
            milestone_authority: None,
            refund_to: None,
        }
    }

    const A: AccountId = AccountId::new([1; 32]);
    const B: AccountId = AccountId::new([2; 32]);
    const C: AccountId = AccountId::new([3; 32]);

    #[test]
    fn u1_create_native_declares_the_spec_actors_in_order() {
        let p = programs();
        let tx = p.create(A, [9; 32], terms(B, Asset::Native), 0, A).expect("builds");
        let s = p.schedule_account(A, &[9; 32]);
        assert_eq!(
            tx.actors(),
            vec![
                Actor::new(A, p.vesting),
                Actor::new(s, p.vesting),
                Actor::native_balance(A),
                Actor::native_balance(s),
                Actor::new(p.fees_config().expect("fees"), p.fees.expect("fees")),
            ]
        );
        assert_eq!(tx.message.encode()[0], 0, "Create is tag 0");
        assert_eq!(tx.schedules, vec![s]);
    }

    #[test]
    fn u1_create_with_fee_declares_the_treasury() {
        let p = programs();
        let tx = p.create(A, [9; 32], terms(B, Asset::Native), 3, C).expect("builds");
        assert_eq!(tx.actors().last(), Some(&Actor::native_balance(C)));
        let token = Asset::Token {
            definition: core_id(AccountId::new([4; 32])),
        };
        let tx = p.create(A, [9; 32], terms(B, token), 3, C).expect("builds");
        let ata = p.treasury_ata(C, AccountId::new([4; 32])).expect("ata");
        assert_eq!(tx.actors().last(), Some(&Actor::new(ata, p.token.expect("token"))));
    }

    #[test]
    fn u1_batch_opens_one_escrow_per_item() {
        let p = programs();
        let items: Vec<BatchItem> = (0..3u8)
            .map(|i| BatchItem {
                schedule_id: [i; 32],
                beneficiary: core_id(B),
                total: 10,
            })
            .collect();
        let common = BatchTerms {
            asset: Asset::Native,
            shape: vesting_core::BatchShape::Linear {
                start: 0,
                cliff: 0,
                end: 10,
            },
            cancelable: true,
            transferable: false,
            cancel_authority: None,
            milestone_authority: None,
            refund_to: None,
        };
        let tx = p.create_batch(A, common, items, 0, A).expect("builds");
        assert_eq!(tx.schedules.len(), 3);
        // root + 2 per item + native(source) + fees config
        assert_eq!(tx.accounts.len(), 1 + 6 + 1 + 1);
        assert_eq!(tx.message.encode()[0], 1, "CreateBatch is tag 1");
    }

    #[test]
    fn u3_public_claim_declares_clock_escrow_and_destination() {
        let p = programs();
        let tx = p.claim(A, B, A, &Asset::Native).expect("builds");
        assert_eq!(
            tx.actors(),
            vec![
                Actor::new(A, p.vesting),
                Actor::new(B, p.vesting),
                clock_core::clock_actor(),
                Actor::native_balance(B),
                Actor::native_balance(A),
            ]
        );
        assert!(!tx.is_private());
        assert_eq!(tx.message.encode()[0], 2, "Claim is tag 2");
    }

    #[test]
    fn pr1_private_claim_sends_only_the_blinded_address() {
        let p = programs();
        let (dest, opening) = (C, [42; 32]);
        let tx = p
            .claim_private(A, B, dest, opening, 77, &Asset::Native, None)
            .expect("builds");
        let blinded = dest.blinded(&opening);
        assert!(tx.is_private());
        let Message::ClaimAsOf { to, as_of, .. } = tx.message else {
            panic!("ClaimAsOf expected")
        };
        assert_eq!((lez_id(to), as_of), (blinded, 77));
        let bytes = tx.message.encode();
        assert_eq!(bytes[0], 3, "ClaimAsOf is tag 3");
        assert!(
            !bytes.windows(32).any(|w| w == dest.value()),
            "canonical id never in the message"
        );
        // Public actors: root, schedule, escrow; the destination is the private mention with the opening.
        let private = &tx.accounts[3];
        assert!(private.identity.is_private());
        assert_eq!(private.address_openings, BTreeSet::from([opening]));
    }

    #[test]
    fn cancel_pays_the_refund_account() {
        let p = programs();
        let tx = p.cancel(A, B, C, &Asset::Native).expect("builds");
        assert_eq!(tx.actors().last(), Some(&Actor::native_balance(C)));
        assert_eq!(tx.message.encode()[0], 4);
        assert_eq!(p.make_non_cancelable(A, B).message.encode()[0], 5);
        assert_eq!(p.signal(A, B, 0).message.encode()[0], 6);
        assert_eq!(p.transfer(A, B, C).message.encode()[0], 7);
    }

    #[test]
    fn ids_file_parses() {
        let v = AccountId::new([7; 32]);
        let text = format!("[deploy]\nx = 1\n[programs]\nvesting = \"{v}\" # note\ntoken = \"{v}\"\n");
        let p = Programs::parse_ids(&text).expect("parses");
        assert_eq!((p.vesting, p.token, p.fees), (v, Some(v), None));
        assert!(Programs::parse_ids("[programs]\n").is_err());
    }
}
