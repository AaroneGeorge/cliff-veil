//! The privacy disclosure shown before every claim to a private account (RFP Usability 5, Privacy 2; SPEC 9).
//!
//! One text, defined once here, so the CLI and the mini-app show exactly the same words. It names what a private
//! claim makes public and what it does not, and the limitation of the model.

/// The disclosure shown, and confirmed, before each claim to a private account.
pub const PRIVACY_DISCLOSURE: &str = "\
Privacy of this claim (claim to a private account)
  Visible on-chain, to everyone:
    - the claim amount
    - the beneficiary address
    - the vesting schedule address
    - the time of the claim
  Not recorded in public on-chain state:
    - the destination private account
    - later movements of the claimed tokens
  Limitation: the beneficiary address is publicly linked to this schedule from its creation, and every claim
  from it is linked to that address. Privacy covers where the claimed tokens go, not who claimed them.";

/// What is visible on-chain for a private claim, as separate items (for a structured display).
pub const VISIBLE_ON_CHAIN: [&str; 4] = [
    "the claim amount",
    "the beneficiary address",
    "the vesting schedule address",
    "the time of the claim",
];

/// What a private claim does not record in public on-chain state, as separate items.
pub const NOT_RECORDED: [&str; 2] = [
    "the destination private account",
    "later movements of the claimed tokens",
];

/// Shown next to every private claim made while proofs are not generated (`RISC0_DEV_MODE=1`).
pub const DEV_MODE_NOTE: &str = "dev mode: RISC0_DEV_MODE=1, the privacy-preserving transaction carries no real \
proof; on a network that verifies proofs the claim is proved locally first, which takes minutes";

/// The privacy disclosure text ([`PRIVACY_DISCLOSURE`]).
#[must_use]
pub const fn privacy_disclosure() -> &'static str {
    PRIVACY_DISCLOSURE
}

/// `true` when this process runs with RISC Zero dev mode on (no real proofs).
#[must_use]
pub fn dev_mode() -> bool {
    std::env::var("RISC0_DEV_MODE").is_ok_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr2_disclosure_names_every_visible_item() {
        let text = privacy_disclosure();
        for item in VISIBLE_ON_CHAIN {
            assert!(text.contains(item), "disclosure misses visible item `{item}`");
        }
    }

    #[test]
    fn pr2_disclosure_names_what_is_not_recorded() {
        let text = privacy_disclosure();
        let (_, private_part) = text
            .split_once("Not recorded in public on-chain state:")
            .expect("the disclosure has a not-recorded section");
        for item in NOT_RECORDED {
            assert!(private_part.contains(item), "disclosure misses private item `{item}`");
        }
    }

    #[test]
    fn pr2_disclosure_states_the_limitation() {
        assert!(privacy_disclosure().contains("beneficiary address is publicly linked to this schedule"));
    }
}
