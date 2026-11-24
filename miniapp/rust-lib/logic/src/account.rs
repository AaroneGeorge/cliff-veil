//! Account ids as people type them: base58 of 32 bytes, as LEZ prints `AccountId` (Bitcoin alphabet). The wallet's
//! `Public/` and `Private/` prefixes are accepted and stripped. The mini-app only checks the shape; the SDK parses the
//! id again before it builds anything.

const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Decodes base58 text to bytes (leading `1`s are leading zero bytes).
fn decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes: Vec<u8> = Vec::new();
    for c in text.bytes() {
        let mut carry = u32::try_from(ALPHABET.iter().position(|a| *a == c)?).ok()?;
        for b in bytes.iter_mut().rev() {
            carry += u32::from(*b) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let zeros = text.bytes().take_while(|c| *c == b'1').count();
    let mut out = vec![0u8; zeros];
    out.extend(bytes);
    Some(out)
}

/// The account id in `text` (trimmed, prefix stripped), or why it is not one.
///
/// # Errors
/// Empty text, a character outside the base58 alphabet, or a length other than 32 bytes.
pub fn parse(text: &str) -> Result<String, String> {
    let t = text.trim();
    let t = t
        .strip_prefix("Public/")
        .or_else(|| t.strip_prefix("Private/"))
        .unwrap_or(t);
    if t.is_empty() {
        return Err("enter an account id".to_owned());
    }
    match decode(t) {
        None => Err(format!("`{t}` is not base58 (an account id as the wallet prints it)")),
        Some(b) if b.len() != 32 => Err(format!("`{t}` is {} bytes; an account id is 32 bytes", b.len())),
        Some(_) => Ok(t.to_owned()),
    }
}

/// `abcd…wxyz`: the first and last `n` characters, for narrow columns.
#[must_use]
pub fn short(id: &str, n: usize) -> String {
    if id.chars().count() <= 2 * n + 1 {
        return id.to_owned();
    }
    let head: String = id.chars().take(n).collect();
    let tail: String = id.chars().rev().take(n).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Base58 of 32 bytes, for tests: `n` repeated.
    pub(crate) fn id(n: u8) -> String {
        let bytes = [n; 32];
        // Encode by repeated division (test helper only).
        let mut num = bytes.to_vec();
        let mut out = Vec::new();
        while num.iter().any(|b| *b != 0) {
            let mut rem = 0u32;
            for b in &mut num {
                let cur = (rem << 8) | u32::from(*b);
                *b = (cur / 58) as u8;
                rem = cur % 58;
            }
            out.push(ALPHABET[rem as usize]);
        }
        out.extend(std::iter::repeat_n(b'1', bytes.iter().take_while(|b| **b == 0).count()));
        out.reverse();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn accepts_32_byte_ids_and_prefixes() {
        let a = id(7);
        assert_eq!(decode(&a).unwrap(), vec![7u8; 32]);
        assert_eq!(parse(&a), Ok(a.clone()));
        assert_eq!(parse(&format!("Public/{a}")), Ok(a.clone()));
        assert_eq!(parse(&format!(" Private/{a} ")), Ok(a));
        // All-zero id: 32 leading `1`s.
        assert_eq!(parse(&"1".repeat(32)), Ok("1".repeat(32)));
    }

    #[test]
    fn refuses_other_text() {
        assert!(parse("").is_err());
        assert!(parse("0OIl").unwrap_err().contains("not base58"));
        assert!(parse("abc").unwrap_err().contains("bytes"));
    }

    #[test]
    fn shortens_for_columns() {
        assert_eq!(short("abcdefghijkl", 3), "abc…jkl");
        assert_eq!(short("abcdefg", 3), "abcdefg");
    }
}
