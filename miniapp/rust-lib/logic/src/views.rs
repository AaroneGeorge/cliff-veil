//! The recipient view and the creator view (RFP Usability 2).
//!
//! Recipient: current vesting position, total locked, amount claimable now, next unlock event, and whether the claim
//! action is available. Creator: the active schedules a creator made, and for each whether the caller may cancel it or
//! signal a milestone. All numbers come from the SDK's schedule status ([`ScheduleStatus`]); this module only words
//! and arranges them.

use serde::Serialize;

use crate::account::short;
use crate::amount::{bps, display, percent};
use crate::model::{Asset, Kind, NextUnlock, Outcome, ScheduleStatus, Standing};
use crate::time::{duration, utc};

/// The asset as a label.
#[must_use]
pub fn asset_label(asset: &Asset) -> String {
    match asset {
        Asset::Native => "native".to_owned(),
        Asset::Token { definition } => format!("token {}", short(definition, 6)),
    }
}

/// The next unlock as text, with the time left: `2026-01-01T00:00:00.000Z (in 3d 4h)`, the milestone wording, or
/// none. The same words as the SDK's `ScheduleView::next_unlock_text`.
#[must_use]
pub fn next_unlock_text(next: NextUnlock, now: u64) -> String {
    match next {
        NextUnlock::At(t) if t > now => format!("{} (in {})", utc(t), duration(t - now)),
        NextUnlock::At(t) => utc(t),
        NextUnlock::WhenMilestoneSignalled => "when the milestone authority signals a milestone".to_owned(),
        NextUnlock::Never => "none (nothing more vests)".to_owned(),
    }
}

/// The schedule's shape as text lines.
#[must_use]
pub fn kind_lines(kind: &Kind) -> Vec<String> {
    match kind {
        Kind::Linear { start, cliff, end } => {
            let shape = if cliff == start {
                "fully linear"
            } else if cliff == end {
                "single cliff"
            } else {
                "cliff, then linear"
            };
            vec![
                format!("Linear ({shape})"),
                format!("start {}", utc(*start)),
                format!("cliff {}", utc(*cliff)),
                format!("end   {}", utc(*end)),
            ]
        }
        Kind::Milestones { amounts, signalled } => {
            let done = (0..amounts.len()).filter(|i| signalled >> i & 1 == 1).count();
            let mut lines = vec![format!("Milestones ({done} of {} signalled)", amounts.len())];
            lines.extend(amounts.iter().enumerate().map(|(i, a)| {
                format!(
                    "#{i}: {} {}",
                    display(*a),
                    if signalled >> i & 1 == 1 {
                        "signalled"
                    } else {
                        "pending"
                    }
                )
            }));
            lines
        }
    }
}

fn standing_label(s: Standing) -> &'static str {
    match s {
        Standing::Active => "active",
        Standing::Cancelled => "cancelled",
        Standing::Settled => "settled (fully paid out)",
    }
}

/// The recipient view of one schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PositionView {
    /// Schedule account.
    pub schedule: String,
    /// Beneficiary.
    pub beneficiary: String,
    /// Creator.
    pub creator: String,
    /// Asset.
    pub asset: String,
    /// Active, cancelled or settled.
    pub standing: String,
    /// Total escrowed at creation.
    pub total: String,
    /// Vested so far (frozen at the cancel when cancelled).
    pub vested: String,
    /// Claimed so far.
    pub claimed: String,
    /// Total locked: not vested yet.
    pub locked: String,
    /// Claimable now.
    pub claimable_now: String,
    /// Next unlock event.
    pub next_unlock: String,
    /// Vested share of the total, `12.34%`.
    pub vested_share: String,
    /// Vested share in basis points (for a progress bar).
    pub vested_bps: u32,
    /// Claimed share in basis points.
    pub claimed_bps: u32,
    /// The schedule's shape.
    pub terms: Vec<String>,
    /// Cancelable / transferable, as text.
    pub flags: String,
    /// The clock time the view is computed at.
    pub as_of: String,
    /// The claim action is available.
    pub can_claim: bool,
    /// Why not, when it is not.
    pub claim_hint: Option<String>,
}

impl PositionView {
    /// The view of `s`.
    #[must_use]
    pub fn new(s: &ScheduleStatus) -> Self {
        let next = next_unlock_text(s.next_unlock, s.now);
        let (can_claim, claim_hint) = if s.claimable > 0 {
            (true, None)
        } else if s.standing == Standing::Settled {
            (
                false,
                Some("everything this schedule will pay has been claimed".to_owned()),
            )
        } else {
            (false, Some(format!("nothing is claimable now; next unlock: {next}")))
        };
        let mut flags = vec![if s.cancelable && s.cancelled.is_none() {
            "cancelable"
        } else {
            "not cancelable"
        }];
        flags.push(if s.transferable {
            "transferable"
        } else {
            "not transferable"
        });
        let mut terms = kind_lines(&s.kind);
        if let Some(c) = &s.cancelled {
            terms.push(format!(
                "cancelled at {}: {} vested, {} refunded",
                utc(c.at),
                display(c.vested_at_cancel),
                display(c.refunded)
            ));
        }
        let vested_bps = bps(s.vested, s.total);
        Self {
            schedule: s.account.clone(),
            beneficiary: s.beneficiary.clone(),
            creator: s.creator.clone(),
            asset: asset_label(&s.asset),
            standing: standing_label(s.standing).to_owned(),
            total: display(s.total),
            vested: display(s.vested),
            claimed: display(s.claimed),
            locked: display(s.locked),
            claimable_now: display(s.claimable),
            next_unlock: next,
            vested_share: percent(vested_bps),
            vested_bps,
            claimed_bps: bps(s.claimed, s.total),
            terms,
            flags: flags.join(", "),
            as_of: utc(s.now),
            can_claim,
            claim_hint,
        }
    }
}

/// Totals over every position of a beneficiary (top of the recipient view).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PositionTotals {
    /// Number of schedules.
    pub schedules: usize,
    /// Sum of locked, per asset label.
    pub locked: Vec<(String, String)>,
    /// Sum of claimable now, per asset label.
    pub claimable_now: Vec<(String, String)>,
}

/// Sums `statuses` per asset (amounts of different assets are never added together).
#[must_use]
pub fn totals(statuses: &[ScheduleStatus]) -> PositionTotals {
    let mut assets: Vec<(String, u128, u128)> = Vec::new();
    for s in statuses {
        let label = asset_label(&s.asset);
        match assets.iter_mut().find(|(l, _, _)| *l == label) {
            Some((_, locked, claimable)) => {
                *locked = locked.saturating_add(s.locked);
                *claimable = claimable.saturating_add(s.claimable);
            }
            None => assets.push((label, s.locked, s.claimable)),
        }
    }
    PositionTotals {
        schedules: statuses.len(),
        locked: assets.iter().map(|(l, v, _)| (l.clone(), display(*v))).collect(),
        claimable_now: assets.iter().map(|(l, _, c)| (l.clone(), display(*c))).collect(),
    }
}

/// One row of the creator view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreatorRow {
    /// Schedule account.
    pub schedule: String,
    /// Beneficiary.
    pub beneficiary: String,
    /// Asset.
    pub asset: String,
    /// Total.
    pub total: String,
    /// Claimed.
    pub claimed: String,
    /// Claimable by the beneficiary now.
    pub claimable_now: String,
    /// Not vested yet.
    pub locked: String,
    /// Active or cancelled.
    pub standing: String,
    /// The schedule's shape.
    pub terms: Vec<String>,
    /// The caller may cancel it.
    pub can_cancel: bool,
    /// Why not.
    pub cancel_hint: Option<String>,
    /// Milestones the caller may signal (indices).
    pub signal_indices: Vec<u8>,
    /// Why none, when none.
    pub signal_hint: Option<String>,
}

impl CreatorRow {
    /// The row of `s` as `caller` sees it.
    #[must_use]
    pub fn new(s: &ScheduleStatus, caller: &str) -> Self {
        let cancel_hint = if s.cancelled.is_some() {
            Some("already cancelled".to_owned())
        } else if !s.cancelable {
            Some("not cancelable".to_owned())
        } else if s.cancel_authority != caller {
            Some(format!(
                "only the cancel authority {} may cancel",
                short(&s.cancel_authority, 6)
            ))
        } else {
            None
        };
        let (signal_indices, signal_hint) = match &s.kind {
            Kind::Linear { .. } => (Vec::new(), Some("linear schedule: no milestones".to_owned())),
            Kind::Milestones { .. } if s.cancelled.is_some() => (Vec::new(), Some("cancelled".to_owned())),
            Kind::Milestones { .. } if s.milestone_authority != caller => (
                Vec::new(),
                Some(format!(
                    "only the milestone authority {} may signal",
                    short(&s.milestone_authority, 6)
                )),
            ),
            Kind::Milestones { amounts, signalled } => {
                let open: Vec<u8> = (0..amounts.len())
                    .filter(|i| signalled >> i & 1 == 0)
                    .filter_map(|i| u8::try_from(i).ok())
                    .collect();
                let hint = open.is_empty().then(|| "every milestone is signalled".to_owned());
                (open, hint)
            }
        };
        Self {
            schedule: s.account.clone(),
            beneficiary: s.beneficiary.clone(),
            asset: asset_label(&s.asset),
            total: display(s.total),
            claimed: display(s.claimed),
            claimable_now: display(s.claimable),
            locked: display(s.locked),
            standing: standing_label(s.standing).to_owned(),
            terms: kind_lines(&s.kind),
            can_cancel: cancel_hint.is_none(),
            cancel_hint,
            signal_indices,
            signal_hint,
        }
    }
}

/// The creator view: the active schedules of `statuses` (settled ones are left out; cancelled ones stay while
/// their beneficiary still has something to claim), as `caller` sees them.
#[must_use]
pub fn creator_rows(statuses: &[ScheduleStatus], caller: &str) -> Vec<CreatorRow> {
    statuses
        .iter()
        .filter(|s| s.standing != Standing::Settled)
        .map(|s| CreatorRow::new(s, caller))
        .collect()
}

/// The predicted outcome of a creation, cancel or signal, as review lines.
#[must_use]
pub fn outcome_lines(outcome: &Outcome) -> Vec<(String, String)> {
    let l = |k: &str, v: String| (k.to_owned(), v);
    match outcome {
        Outcome::Created {
            schedules,
            total,
            protocol_fee,
            treasury,
        } => vec![
            l("Schedules created", schedules.join(", ")),
            l("Escrowed", display(*total)),
            l("Creation fee", format!("{} to {treasury}", display(*protocol_fee))),
        ],
        Outcome::Cancelled {
            at,
            vested_at_cancel,
            refunded,
            refund_to,
            still_claimable,
        } => vec![
            l("Effective at (at least)", utc(*at)),
            l("Vested, frozen by the cancel", display(*vested_at_cancel)),
            l("Refunded", format!("{} to {refund_to}", display(*refunded))),
            l("Stays claimable by the beneficiary", display(*still_claimable)),
        ],
        Outcome::Signalled { index, amount } => {
            vec![
                l("Milestone signalled", format!("#{index}")),
                l("Unlocks", display(*amount)),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::id;
    use crate::model::tests::{linear_status, milestone_status};
    use crate::model::Cancellation;

    #[test]
    fn u2_recipient_view_shows_position_locked_claimable_and_next_unlock() {
        let v = PositionView::new(&linear_status());
        assert_eq!(
            (v.total.as_str(), v.vested.as_str(), v.claimed.as_str()),
            ("1,000", "500", "100")
        );
        assert_eq!(v.locked, "500");
        assert_eq!(v.claimable_now, "400");
        assert_eq!(v.next_unlock, "1970-01-01T00:25:01.000Z (in 1s)");
        assert_eq!(
            (v.vested_bps, v.claimed_bps, v.vested_share.as_str()),
            (5000, 1000, "50.00%")
        );
        assert_eq!(v.terms[0], "Linear (cliff, then linear)");
        assert_eq!(v.flags, "cancelable, not transferable");
        assert!(v.can_claim && v.claim_hint.is_none());
        assert_eq!(v.as_of, "1970-01-01T00:25:00.000Z");
    }

    #[test]
    fn u2_next_unlock_wording_for_every_case() {
        assert_eq!(
            next_unlock_text(NextUnlock::At(90_061_000), 0),
            "1970-01-02T01:01:01.000Z (in 1d 1h)"
        );
        assert_eq!(next_unlock_text(NextUnlock::At(5), 9), "1970-01-01T00:00:00.005Z");
        assert_eq!(
            next_unlock_text(NextUnlock::WhenMilestoneSignalled, 0),
            "when the milestone authority signals a milestone"
        );
        assert_eq!(next_unlock_text(NextUnlock::Never, 0), "none (nothing more vests)");
    }

    #[test]
    fn u2_claim_action_disabled_with_the_reason_when_nothing_is_claimable() {
        let v = PositionView::new(&milestone_status());
        assert!(!v.can_claim);
        assert_eq!(
            v.claim_hint.as_deref(),
            Some("nothing is claimable now; next unlock: when the milestone authority signals a milestone")
        );
        assert_eq!(
            v.terms,
            [
                "Milestones (1 of 3 signalled)",
                "#0: 100 signalled",
                "#1: 200 pending",
                "#2: 300 pending"
            ]
        );
        let settled = ScheduleStatus {
            standing: Standing::Settled,
            claimable: 0,
            claimed: 1000,
            next_unlock: NextUnlock::Never,
            ..linear_status()
        };
        assert!(PositionView::new(&settled)
            .claim_hint
            .unwrap()
            .contains("has been claimed"));
    }

    #[test]
    fn u2_cancelled_position_shows_the_cancellation() {
        let s = ScheduleStatus {
            standing: Standing::Cancelled,
            locked: 0,
            cancelled: Some(Cancellation {
                at: 1_500_000,
                vested_at_cancel: 500,
                refunded: 500,
            }),
            next_unlock: NextUnlock::Never,
            ..linear_status()
        };
        let v = PositionView::new(&s);
        assert_eq!(v.standing, "cancelled");
        assert_eq!(v.locked, "0");
        assert!(v.can_claim, "the vested remainder stays claimable");
        assert_eq!(
            v.terms.last().unwrap(),
            "cancelled at 1970-01-01T00:25:00.000Z: 500 vested, 500 refunded"
        );
        assert!(v.flags.starts_with("not cancelable"));
    }

    #[test]
    fn u2_totals_never_mix_assets() {
        let token = ScheduleStatus {
            asset: Asset::Token { definition: id(6) },
            ..linear_status()
        };
        let t = totals(&[linear_status(), milestone_status(), token]);
        assert_eq!(t.schedules, 3);
        assert_eq!(t.locked[0], ("native".to_owned(), "1,000".to_owned()));
        assert_eq!(t.claimable_now[0], ("native".to_owned(), "400".to_owned()));
        assert!(t.locked[1].0.starts_with("token "));
        assert_eq!(t.locked[1].1, "500");
    }

    #[test]
    fn u2_creator_view_lists_active_schedules_with_cancel_and_signal() {
        let creator = id(2);
        let settled = ScheduleStatus {
            standing: Standing::Settled,
            ..linear_status()
        };
        let rows = creator_rows(&[linear_status(), milestone_status(), settled], &creator);
        assert_eq!(rows.len(), 2, "settled schedules are not listed");
        // The creator is the cancel authority of both, and not the milestone authority of the second.
        assert!(rows[0].can_cancel && rows[1].can_cancel);
        assert!(rows[0].signal_indices.is_empty());
        assert_eq!(rows[0].signal_hint.as_deref(), Some("linear schedule: no milestones"));
        assert!(rows[1].signal_indices.is_empty());
        assert!(rows[1]
            .signal_hint
            .as_deref()
            .unwrap()
            .starts_with("only the milestone authority"));
        // The milestone authority sees the open milestones.
        let row = CreatorRow::new(&milestone_status(), &id(5));
        assert_eq!(row.signal_indices, [1, 2]);
        assert!(!row.can_cancel);
        assert!(row.cancel_hint.unwrap().starts_with("only the cancel authority"));
    }

    #[test]
    fn u2_creator_view_explains_why_cancel_is_unavailable() {
        let fixed = ScheduleStatus {
            cancelable: false,
            ..linear_status()
        };
        assert_eq!(
            CreatorRow::new(&fixed, &id(2)).cancel_hint.as_deref(),
            Some("not cancelable")
        );
        let gone = ScheduleStatus {
            standing: Standing::Cancelled,
            cancelled: Some(Cancellation {
                at: 1,
                vested_at_cancel: 0,
                refunded: 1000,
            }),
            ..milestone_status()
        };
        let row = CreatorRow::new(&gone, &id(5));
        assert_eq!(row.cancel_hint.as_deref(), Some("already cancelled"));
        assert!(row.signal_indices.is_empty());
        let all = ScheduleStatus {
            kind: Kind::Milestones {
                amounts: vec![1, 2],
                signalled: 0b11,
            },
            ..milestone_status()
        };
        assert_eq!(
            CreatorRow::new(&all, &id(5)).signal_hint.as_deref(),
            Some("every milestone is signalled")
        );
    }

    #[test]
    fn u2_signal_and_cancel_outcomes_read_as_lines() {
        let lines = outcome_lines(&Outcome::Signalled {
            index: 2,
            amount: 300_000,
        });
        assert_eq!(
            lines,
            [
                ("Milestone signalled".to_owned(), "#2".to_owned()),
                ("Unlocks".to_owned(), "300,000".to_owned())
            ]
        );
        let c = Outcome::Cancelled {
            at: 0,
            vested_at_cancel: 5,
            refunded: 1_000,
            refund_to: id(2),
            still_claimable: 3,
        };
        assert_eq!(outcome_lines(&c)[2].1, format!("1,000 to {}", id(2)));
    }
}
