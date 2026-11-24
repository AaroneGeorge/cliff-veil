//! Parsing of command-line inputs that do not need the chain: times, amounts, schedule ids and batch files.

use anyhow::{Context as _, Result, anyhow, bail};

/// A time argument: milliseconds since the Unix epoch, `now`, or an offset from the chain clock such as `+10m`,
/// `now+90s`, `-1h` (units `ms`, `s`, `m`, `h`, `d`).
pub fn parse_time(text: &str, now: u64) -> Result<u64> {
    let t = text.trim();
    if t == "now" {
        return Ok(now);
    }
    let rel = t.strip_prefix("now").unwrap_or(t);
    if let Some(rest) = rel.strip_prefix('+') {
        return now.checked_add(parse_duration(rest)?).context("time overflows");
    }
    if let Some(rest) = rel.strip_prefix('-') {
        return now
            .checked_sub(parse_duration(rest)?)
            .context("time before the Unix epoch");
    }
    t.parse()
        .map_err(|_| anyhow!("`{text}` is not a time: use ms since the epoch, `now`, or an offset like `+10m`"))
}

/// A duration such as `250ms`, `30s`, `10m`, `2h`, `7d`; a bare number is milliseconds.
pub fn parse_duration(text: &str) -> Result<u64> {
    let t = text.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: u64 = num.parse().map_err(|_| anyhow!("`{text}` is not a duration"))?;
    let factor = match unit {
        "" | "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        other => bail!("unknown duration unit `{other}` in `{text}` (use ms, s, m, h, d)"),
    };
    n.checked_mul(factor).context("duration overflows")
}

/// A comma-separated list of amounts (`300,700`).
pub fn parse_amounts(text: &str) -> Result<Vec<u128>> {
    text.split(',')
        .map(|a| a.trim().parse().map_err(|_| anyhow!("`{a}` is not an amount")))
        .collect()
}

/// A comma-separated list of basis-point weights (`2500,7500`).
pub fn parse_bps(text: &str) -> Result<Vec<u16>> {
    text.split(',')
        .map(|a| {
            a.trim()
                .parse()
                .map_err(|_| anyhow!("`{a}` is not a basis-point weight"))
        })
        .collect()
}

/// A schedule id given as 64 hex characters.
pub fn parse_id_hex(text: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(text.trim()).context("schedule id: not hex")?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("schedule id: must be 32 bytes (64 hex characters)"))
}

/// One recipient of a batch file, before the beneficiary is resolved against the wallet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRow {
    /// Beneficiary: an account id or a wallet label.
    pub beneficiary: String,
    /// Amount for this recipient.
    pub total: u128,
    /// Label the schedule id is derived from; `None`: a random id.
    pub label: Option<String>,
}

/// Reads a batch file: CSV (`beneficiary,total[,label]`, optional header line, `#` comments) or JSON (an array of
/// `{"beneficiary": "...", "total": "1000" | 1000, "label": "..."}`). JSON is detected by a leading `[`.
pub fn parse_batch(text: &str) -> Result<Vec<BatchRow>> {
    let rows = if text.trim_start().starts_with('[') {
        parse_batch_json(text)?
    } else {
        parse_batch_csv(text)?
    };
    if rows.is_empty() {
        bail!("the batch file has no recipients");
    }
    Ok(rows)
}

fn parse_batch_csv(text: &str) -> Result<Vec<BatchRow>> {
    let mut rows = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        if n == 0 && cols.first().is_some_and(|c| c.eq_ignore_ascii_case("beneficiary")) {
            continue;
        }
        let [beneficiary, total, rest @ ..] = cols.as_slice() else {
            bail!("line {}: expected `beneficiary,total[,label]`", n + 1);
        };
        let total = total
            .parse()
            .map_err(|_| anyhow!("line {}: `{total}` is not an amount", n + 1))?;
        let label = rest.first().filter(|l| !l.is_empty()).map(|l| (*l).to_owned());
        rows.push(BatchRow {
            beneficiary: (*beneficiary).to_owned(),
            total,
            label,
        });
    }
    Ok(rows)
}

fn parse_batch_json(text: &str) -> Result<Vec<BatchRow>> {
    let value: serde_json::Value = serde_json::from_str(text).context("batch file: invalid JSON")?;
    let items = value.as_array().context("batch file: expected a JSON array")?;
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let field = |k: &str| item.get(k);
            let beneficiary = field("beneficiary")
                .and_then(serde_json::Value::as_str)
                .with_context(|| format!("item {i}: `beneficiary` (string) missing"))?
                .to_owned();
            let total = match field("total") {
                Some(serde_json::Value::String(s)) => s.parse().ok(),
                Some(serde_json::Value::Number(n)) => n.as_u64().map(u128::from),
                _ => None,
            }
            .with_context(|| format!("item {i}: `total` (a whole number or a string of one) missing"))?;
            let label = field("label").and_then(serde_json::Value::as_str).map(str::to_owned);
            Ok(BatchRow {
                beneficiary,
                total,
                label,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_absolute_now_and_relative() {
        assert_eq!(parse_time("1767225600000", 5).unwrap(), 1_767_225_600_000);
        assert_eq!(parse_time("now", 1_000).unwrap(), 1_000);
        assert_eq!(parse_time("+10m", 1_000).unwrap(), 601_000);
        assert_eq!(parse_time("now+2s", 1_000).unwrap(), 3_000);
        assert_eq!(parse_time("-1s", 1_500).unwrap(), 500);
        assert!(parse_time("tomorrow", 0).is_err());
        assert!(parse_time("+5y", 0).is_err());
    }

    #[test]
    fn u1_batch_csv_with_header_comments_and_labels() {
        let rows = parse_batch("beneficiary,total,label\n# team\nalice,1000,alice-2026\n  bob , 25 \n").unwrap();
        assert_eq!(
            rows,
            vec![
                BatchRow {
                    beneficiary: "alice".into(),
                    total: 1000,
                    label: Some("alice-2026".into())
                },
                BatchRow {
                    beneficiary: "bob".into(),
                    total: 25,
                    label: None
                },
            ]
        );
        assert!(parse_batch("alice\n").is_err());
        assert!(parse_batch("alice,ten\n").is_err());
        assert!(parse_batch("# nothing\n").is_err());
    }

    #[test]
    fn u1_batch_json_with_string_or_number_totals() {
        let rows = parse_batch(
            r#"[{"beneficiary":"alice","total":"340282366920938463463374607431768211455"},{"beneficiary":"bob","total":7,"label":"b"}]"#,
        )
        .unwrap();
        assert_eq!(rows[0].total, u128::MAX);
        assert_eq!((rows[1].total, rows[1].label.as_deref()), (7, Some("b")));
        assert!(parse_batch(r#"[{"total":1}]"#).is_err());
    }

    #[test]
    fn ids_amounts_and_weights() {
        assert_eq!(parse_id_hex(&"ab".repeat(32)).unwrap(), [0xab; 32]);
        assert!(parse_id_hex("abcd").is_err());
        assert_eq!(parse_amounts("300, 700").unwrap(), vec![300, 700]);
        assert_eq!(parse_bps("2500,7500").unwrap(), vec![2500, 7500]);
    }
}
