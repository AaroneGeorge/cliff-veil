//! The privacy disclosure before a claim to a private account (RFP Usability 5, Privacy 2; SPEC 9).
//!
//! The text itself is the SDK's (`cliff_veil_sdk::privacy_disclosure()`), delivered with each prepared private claim,
//! so the CLI and the mini-app show the same words. The mini-app does not trust that it arrived intact: before it shows
//! a claim to a private account it checks that the text names every item the RFP requires, in the right section, and
//! refuses the claim otherwise. An acknowledgement is bound to the exact text shown by its digest.

use crate::model::ErrorInfo;

/// Heading of the section listing what is public.
pub const VISIBLE_HEADING: &str = "Visible on-chain";
/// Heading of the section listing what is not.
pub const NOT_RECORDED_HEADING: &str = "Not recorded in public on-chain state";

/// What the RFP requires the disclosure to name as visible on-chain.
pub const REQUIRED_VISIBLE: [&str; 3] = [
    "the claim amount",
    "the beneficiary address",
    "the vesting schedule address",
];

/// What the RFP requires the disclosure to name as not traceable.
pub const REQUIRED_NOT_RECORDED: [&str; 2] = [
    "the destination private account",
    "later movements of the claimed tokens",
];

/// Checks that `text` names every required item under the right heading: visible items between the visible heading
/// and the not-recorded heading, not-recorded items after it.
///
/// # Errors
/// `disclosure_incomplete`, naming the first missing item or heading.
pub fn check(text: &str) -> Result<(), ErrorInfo> {
    let incomplete = |what: &str| {
        ErrorInfo::new(
            "disclosure_incomplete",
            format!("the privacy disclosure supplied for this claim does not name {what}; the claim is blocked"),
            Some("update the cliff-veil command line to the version this mini-app was built with"),
        )
    };
    let v = text
        .find(VISIBLE_HEADING)
        .ok_or_else(|| incomplete("what is visible on-chain"))?;
    let n = text
        .find(NOT_RECORDED_HEADING)
        .ok_or_else(|| incomplete("what is not recorded on-chain"))?;
    if n < v {
        return Err(incomplete("what is visible on-chain before what is not"));
    }
    let (visible, private) = (&text[v..n], &text[n..]);
    for item in REQUIRED_VISIBLE {
        if !visible.contains(item) {
            return Err(incomplete(&format!("`{item}` as visible")));
        }
    }
    for item in REQUIRED_NOT_RECORDED {
        if !private.contains(item) {
            return Err(incomplete(&format!("`{item}` as not recorded")));
        }
    }
    Ok(())
}

/// The digest an acknowledgement must carry: FNV-1a (64 bit) of the text, as 16 hex digits. It binds an
/// acknowledgement to the words on screen; it is not a security boundary (the view and the module are one user's).
#[must_use]
pub fn digest(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The SDK's disclosure, read from its source (`PRIVACY_DISCLOSURE` in `sdk/src/disclosure.rs`) so these tests
    /// check the text users actually see, not a copy.
    pub(crate) fn sdk_text() -> String {
        let src = include_str!("../../../../sdk/src/disclosure.rs");
        let start = src
            .find("pub const PRIVACY_DISCLOSURE: &str = \"")
            .expect("the SDK defines the disclosure");
        let body = &src[start + "pub const PRIVACY_DISCLOSURE: &str = \"".len()..];
        let body = &body[..body.find("\";").expect("the literal ends")];
        // Rust string continuation: a `\` at the end of a line drops the newline and the next line's indentation.
        let mut out = String::new();
        let mut rest = body;
        while let Some(i) = rest.find("\\\n") {
            out.push_str(&rest[..i]);
            rest = rest[i + 2..].trim_start();
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn pr2_disclosure_from_the_sdk_names_every_required_item() {
        let text = sdk_text();
        assert!(text.starts_with("Privacy of this claim"), "{text}");
        assert_eq!(check(&text), Ok(()));
    }

    #[test]
    fn pr2_disclosure_missing_a_visible_item_is_refused() {
        let text = sdk_text();
        for item in REQUIRED_VISIBLE {
            let cut = text.replacen(item, "", 1);
            let err = check(&cut).unwrap_err();
            assert_eq!(err.code, "disclosure_incomplete");
            assert!(err.message.contains(item), "{}", err.message);
        }
    }

    #[test]
    fn pr2_disclosure_missing_a_private_item_is_refused() {
        let text = sdk_text();
        for item in REQUIRED_NOT_RECORDED {
            let err = check(&text.replacen(item, "", 1)).unwrap_err();
            assert!(err.message.contains(item), "{}", err.message);
        }
    }

    #[test]
    fn pr2_disclosure_with_items_in_the_wrong_section_is_refused() {
        // The destination listed as visible: the private section then lacks it.
        let swapped = format!(
            "{VISIBLE_HEADING}:\n - the claim amount\n - the beneficiary address\n - the vesting schedule address\n \
             - the destination private account\n{NOT_RECORDED_HEADING}:\n - later movements of the claimed tokens\n"
        );
        assert!(check(&swapped)
            .unwrap_err()
            .message
            .contains("the destination private account"));
        assert!(check("").is_err());
    }

    #[test]
    fn digests_differ_for_different_texts() {
        assert_eq!(digest(""), "cbf29ce484222325");
        assert_eq!(digest("a"), "af63dc4c8601ec8c");
        assert_ne!(digest(&sdk_text()), digest(&sdk_text().replace("amount", "amounts")));
    }
}
