//! Amounts. On chain an amount is a `u128` in base units. QML numbers are doubles and lose precision above 2^53, so
//! every amount crosses into the view as a decimal string, formatted here.

use std::fmt;

/// Why an amount field was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmountError {
    /// Empty field.
    Empty,
    /// Not a whole number of base units.
    NotANumber,
    /// Larger than `u128::MAX`.
    TooLarge,
}

impl fmt::Display for AmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "enter an amount",
            Self::NotANumber => "enter a whole number of base units (digits only; `_` and `,` separators allowed)",
            Self::TooLarge => "the amount is larger than the largest amount the program accepts (2^128 - 1)",
        })
    }
}

/// Parses an amount typed by a person: digits, with optional `_`, `,` or space separators.
///
/// # Errors
/// [`AmountError`] when the text is empty, not a whole number, or above `u128::MAX`.
pub fn parse(text: &str) -> Result<u128, AmountError> {
    let digits: String = text.trim().chars().filter(|c| !matches!(c, '_' | ',' | ' ')).collect();
    if digits.is_empty() {
        return Err(AmountError::Empty);
    }
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AmountError::NotANumber);
    }
    digits.parse::<u128>().map_err(|_| AmountError::TooLarge)
}

/// An amount with thousands separators: `1234567` -> `1,234,567`.
#[must_use]
pub fn display(amount: u128) -> String {
    let digits = amount.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `part / whole` in basis points (0..=10_000), rounded down; 0 when `whole` is 0.
#[must_use]
pub fn bps(part: u128, whole: u128) -> u32 {
    if whole == 0 {
        return 0;
    }
    let part = part.min(whole);
    // part * 10_000 overflows only for part > u128::MAX / 10_000; scale both down first in that case.
    let (p, w) = if part > u128::MAX / 10_000 {
        (part / 10_000, whole / 10_000)
    } else {
        (part, whole)
    };
    let w = w.max(1);
    u32::try_from((p * 10_000 / w).min(10_000)).unwrap_or(10_000)
}

/// Basis points as a percentage with two decimals: `1234` -> `12.34%`.
#[must_use]
pub fn percent(bps: u32) -> String {
    format!("{}.{:02}%", bps / 100, bps % 100)
}

/// (De)serialises a `u128` as a decimal string, and accepts a JSON number too (small values).
pub mod dec {
    use serde::de::{self, Visitor};
    use serde::{Deserializer, Serializer};
    use std::fmt;

    /// Writes `v` as a decimal string.
    ///
    /// # Errors
    /// Those of the serializer.
    pub fn serialize<S: Serializer>(v: &u128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    /// Reads a decimal string or a non-negative JSON integer.
    ///
    /// # Errors
    /// Anything else.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = u128;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a u128 as a decimal string")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<u128, E> {
                v.parse().map_err(|_| E::custom(format!("not a u128: {v:?}")))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<u128, E> {
                Ok(u128::from(v))
            }
        }
        d.deserialize_any(V)
    }

    /// The same for a list of amounts.
    pub mod vec {
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        #[derive(Serialize, Deserialize)]
        struct W(#[serde(with = "super")] u128);

        /// Writes each amount as a decimal string.
        ///
        /// # Errors
        /// Those of the serializer.
        pub fn serialize<S: Serializer>(v: &[u128], s: S) -> Result<S::Ok, S::Error> {
            v.iter().map(|a| W(*a)).collect::<Vec<_>>().serialize(s)
        }

        /// Reads a list of decimal strings.
        ///
        /// # Errors
        /// A non-amount item.
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u128>, D::Error> {
            Ok(Vec::<W>::deserialize(d)?.into_iter().map(|w| w.0).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_amounts() {
        assert_eq!(parse("1000"), Ok(1000));
        assert_eq!(parse(" 1_000_000 "), Ok(1_000_000));
        assert_eq!(parse("1,000"), Ok(1000));
        assert_eq!(parse("340282366920938463463374607431768211455"), Ok(u128::MAX));
        assert_eq!(
            parse("340282366920938463463374607431768211456"),
            Err(AmountError::TooLarge)
        );
        assert_eq!(parse(""), Err(AmountError::Empty));
        assert_eq!(parse("1.5"), Err(AmountError::NotANumber));
        assert_eq!(parse("-3"), Err(AmountError::NotANumber));
    }

    #[test]
    fn displays_with_separators() {
        assert_eq!(display(0), "0");
        assert_eq!(display(999), "999");
        assert_eq!(display(1000), "1,000");
        assert_eq!(display(1_234_567), "1,234,567");
        assert_eq!(
            display(u128::MAX),
            "340,282,366,920,938,463,463,374,607,431,768,211,455"
        );
    }

    #[test]
    fn basis_points_never_exceed_the_whole() {
        assert_eq!(bps(0, 0), 0);
        assert_eq!(bps(1, 3), 3333);
        assert_eq!(bps(3, 3), 10_000);
        assert_eq!(bps(5, 3), 10_000);
        assert_eq!(bps(u128::MAX / 2, u128::MAX), 4999);
        assert_eq!(percent(1234), "12.34%");
        assert_eq!(percent(10_000), "100.00%");
    }

    #[test]
    fn decimal_strings_round_trip() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct T {
            #[serde(with = "dec")]
            a: u128,
            #[serde(with = "dec::vec")]
            v: Vec<u128>,
        }
        let t = T {
            a: u128::MAX,
            v: vec![1, 2],
        };
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, r#"{"a":"340282366920938463463374607431768211455","v":["1","2"]}"#);
        assert_eq!(serde_json::from_str::<T>(&json).unwrap(), t);
        assert_eq!(serde_json::from_str::<T>(r#"{"a":7,"v":[]}"#).unwrap().a, 7);
    }
}
