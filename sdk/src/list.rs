//! Listing schedules by creator or beneficiary (SPEC 11; RFP mini-app creator view).
//!
//! A standalone rc3 sequencer serves no events, so the SDK finds schedules from public transaction data: it scans
//! blocks for root calls to a `vesting` caller actor carrying `Create` or `CreateBatch`, derives each schedule account
//! from the caller (the creator) and the schedule id, and then reads each schedule's current state. The state holds
//! the current beneficiary, so a schedule transferred to someone is listed for them without following transfers.
//!
//! A [`ScanCache`] keeps the schedule accounts found and the next block to scan, so a later listing reads only new
//! blocks. A reverted transaction opened nothing and is skipped; a schedule whose state is empty is dropped.

use std::collections::BTreeSet;

use anyhow::{Context as _, anyhow};
use lee_core::account::AccountId;
use vesting_core::Message;

use crate::backend::RootCallSeen;
use crate::tx::Programs;

/// Schedules found so far by a block scan of one `vesting` deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCache {
    /// The `vesting` program the cache belongs to.
    pub vesting: AccountId,
    /// The next block to scan.
    pub next_block: u64,
    /// Schedule accounts opened by the blocks scanned.
    pub schedules: BTreeSet<AccountId>,
}

impl ScanCache {
    /// An empty cache for `vesting`, scanning from block 0.
    #[must_use]
    pub const fn new(vesting: AccountId) -> Self {
        Self {
            vesting,
            next_block: 0,
            schedules: BTreeSet::new(),
        }
    }

    /// The cache as text: `vesting <id>`, `next_block <n>`, then one `schedule <id>` line each.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = format!("vesting {}\nnext_block {}\n", self.vesting, self.next_block);
        for s in &self.schedules {
            out.push_str(&format!("schedule {s}\n"));
        }
        out
    }

    /// Reads [`Self::to_text`] back.
    ///
    /// # Errors
    /// A line that is not one of the three forms, or a missing `vesting` line.
    pub fn from_text(text: &str) -> anyhow::Result<Self> {
        let mut vesting = None;
        let mut next_block = 0;
        let mut schedules = BTreeSet::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (key, value) = line
                .split_once(' ')
                .with_context(|| format!("scan cache: bad line `{line}`"))?;
            let id = || -> anyhow::Result<AccountId> { value.parse().map_err(|e| anyhow!("scan cache: {e:?}")) };
            match key {
                "vesting" => vesting = Some(id()?),
                "next_block" => next_block = value.parse().context("scan cache: next_block")?,
                "schedule" => {
                    schedules.insert(id()?);
                }
                other => return Err(anyhow!("scan cache: unknown key `{other}`")),
            }
        }
        Ok(Self {
            vesting: vesting.context("scan cache: no vesting line")?,
            next_block,
            schedules,
        })
    }

    /// Adds the schedules `calls` open.
    pub fn absorb(&mut self, programs: &Programs, calls: &[RootCallSeen]) {
        for call in calls {
            self.schedules.extend(schedules_opened(programs, call));
        }
    }
}

/// The schedule accounts one root call opens: those of a non-reverted `Create` or `CreateBatch` sent to a
/// `vesting` caller actor. Anything else opens none.
#[must_use]
pub fn schedules_opened(programs: &Programs, call: &RootCallSeen) -> Vec<AccountId> {
    if call.reverted || call.program != programs.vesting {
        return Vec::new();
    }
    match Message::decode(&call.message) {
        Ok(Message::Create { schedule_id, .. }) => vec![programs.schedule_account(call.account, &schedule_id)],
        Ok(Message::CreateBatch { items, .. }) => items
            .iter()
            .map(|i| programs.schedule_account(call.account, &i.schedule_id))
            .collect(),
        _ => Vec::new(),
    }
}

/// Which schedules to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFilter {
    /// Created by this account.
    Creator(AccountId),
    /// Whose current beneficiary is this account.
    Beneficiary(AccountId),
}

impl ListFilter {
    /// `true` when `state` matches.
    #[must_use]
    pub fn matches(&self, state: &vesting_core::ScheduleV1) -> bool {
        match self {
            Self::Creator(a) => state.creator == crate::core_id(*a),
            Self::Beneficiary(a) => state.beneficiary == crate::core_id(*a),
        }
    }
}

#[cfg(test)]
mod tests {
    use vesting_core::{Asset, Kind, Terms};

    use super::*;
    use crate::core_id;

    const VESTING: AccountId = AccountId::new([9; 32]);
    const A: AccountId = AccountId::new([1; 32]);

    fn call(program: AccountId, message: Message, reverted: bool) -> RootCallSeen {
        RootCallSeen {
            block: 1,
            account: A,
            program,
            message: message.encode(),
            reverted,
        }
    }

    fn create(id: u8) -> Message {
        Message::Create {
            schedule_id: [id; 32],
            terms: Terms {
                beneficiary: core_id(A),
                asset: Asset::Native,
                total: 1,
                kind: Kind::Linear {
                    start: 0,
                    cliff: 0,
                    end: 1,
                },
                cancelable: true,
                transferable: false,
                cancel_authority: None,
                milestone_authority: None,
                refund_to: None,
            },
            source: core_id(A),
            fee: 0,
            treasury: core_id(A),
        }
    }

    #[test]
    fn u2_only_vesting_creations_that_did_not_revert_open_schedules() {
        let p = Programs::new(VESTING);
        assert_eq!(
            schedules_opened(&p, &call(VESTING, create(1), false)),
            vec![p.schedule_account(A, &[1; 32])]
        );
        assert!(
            schedules_opened(&p, &call(VESTING, create(1), true)).is_empty(),
            "reverted"
        );
        assert!(
            schedules_opened(&p, &call(AccountId::new([8; 32]), create(1), false)).is_empty(),
            "other program"
        );
        let claim = Message::Claim {
            schedule: core_id(A),
            to: core_id(A),
        };
        assert!(
            schedules_opened(&p, &call(VESTING, claim, false)).is_empty(),
            "not a creation"
        );
        let mut garbage = call(VESTING, create(1), false);
        garbage.message = vec![0xff, 1, 2];
        assert!(schedules_opened(&p, &garbage).is_empty(), "undecodable");
    }

    #[test]
    fn u2_scan_cache_round_trips_as_text() {
        let p = Programs::new(VESTING);
        let mut c = ScanCache::new(VESTING);
        c.absorb(&p, &[call(VESTING, create(1), false), call(VESTING, create(2), false)]);
        c.next_block = 42;
        let back = ScanCache::from_text(&c.to_text()).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.schedules.len(), 2);
        assert!(ScanCache::from_text("next_block 3\n").is_err());
        assert!(ScanCache::from_text("vesting x\n").is_err());
    }
}
