//! Private-claim mode: the fee of a private claim, read from the JSON a `cliff-veil --json claim --private` run
//! printed into a log.
//!
//! The log is the CLI lifecycle log (`evidence/cli/`): a section starting with a line `== private claim` holds the
//! command and, from a line `{` to the next line `}`, its JSON answer. The fee recorded is the fee the transaction
//! declared, and it is checked against the wallet's view after the claim: the fresh private account received the
//! claimed amount and paid the fee, so `amount - private balance after` must equal it.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use crate::{Row, Run};

/// The JSON answer of the first `== private claim` section of `log`.
fn claim_json(log: &str) -> Result<Value> {
    let mut lines = log.lines().skip_while(|l| !l.starts_with("== private claim ("));
    if lines.next().is_none() {
        bail!("no `== private claim (` section in the log");
    }
    let mut json = String::new();
    let mut inside = false;
    for line in lines {
        if line.starts_with("== ") {
            break;
        }
        if line == "{" {
            inside = true;
        }
        if inside {
            json.push_str(line);
            json.push('\n');
            if line == "}" {
                return serde_json::from_str(&json).context("the claim's JSON answer does not parse");
            }
        }
    }
    bail!("no JSON answer in the private claim section")
}

fn number(v: &Value, path: &[&str]) -> Result<u128> {
    let mut at = v;
    for key in path {
        at = match key.parse::<usize>() {
            Ok(i) => &at[i],
            Err(_) => &at[*key],
        };
    }
    match at {
        Value::String(s) => s.parse().with_context(|| format!("{} is not a number", path.join("."))),
        Value::Number(n) => n
            .as_u64()
            .map(u128::from)
            .with_context(|| format!("{} is not an integer", path.join("."))),
        _ => bail!("{} is missing", path.join(".")),
    }
}

/// The private claim row of the log at `log` (a path relative to `root`, or absolute).
pub fn run(root: &Path, log: &Path) -> Result<Run> {
    let full = if log.is_absolute() {
        log.to_path_buf()
    } else {
        root.join(log)
    };
    let text = std::fs::read_to_string(&full).with_context(|| format!("read {}", full.display()))?;
    let v = claim_json(&text)?;
    let tx = &v["transactions"][0];
    let amount = number(tx, &["amount"])?;
    let declared = number(&v, &["sent", "0", "declared_fee"])?;
    let after = number(&v, &["private_balance_after", "native"])?;
    let dev_mode = tx["dev_mode"].as_bool().unwrap_or(false);
    let block = number(&v, &["sent", "0", "block"])?;
    let hash = v["sent"][0]["hash"].as_str().unwrap_or("").to_owned();

    let mut row = Row::new("claim_private", "native", 1);
    row.fee = Some(declared);
    row.status = if amount.checked_sub(after) == Some(declared) {
        format!(
            "ok ({}; the fresh private account received {amount} and holds {after} after paying the fee)",
            if dev_mode {
                "dev mode: the proof is a dev-mode receipt, so no proving cost is shown; a real proof is in the section below"
            } else {
                "real proof"
            }
        )
    } else {
        format!("fee mismatch: declared {declared}, amount {amount}, private balance after {after}")
    };
    let rel = log.strip_prefix(root).unwrap_or(log);
    Ok(Run {
        mode: "private claim".into(),
        lez: "v0.4.0-rc3".into(),
        inputs: vec![
            ("source".into(), rel.display().to_string()),
            ("transaction".into(), format!("{hash} in block {block}")),
            (
                "RISC0_DEV_MODE".into(),
                if dev_mode { "1".into() } else { "unset".into() },
            ),
        ],
        rows: vec![row],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = r#"== private claim (dev mode), fresh private account
$ cliff-veil --json claim S --private --yes
Latest block is 856
{
  "ok": true,
  "private_balance_after": { "account": "A", "native": "94478251" },
  "sent": [ { "block": 856, "declared_fee": "5528416", "hash": "8ea2" } ],
  "transactions": [ { "amount": "100006667", "dev_mode": true } ]
}
[exit 0]
== next section
"#;

    #[test]
    fn reads_the_fee_and_checks_it_against_the_private_balance() {
        let dir = std::env::temp_dir().join(format!("cv-cost-private-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("log"), LOG).expect("write");
        let first = run(&dir, Path::new("log")).expect("parses");
        let row = &first.rows[0];
        assert_eq!(row.fee, Some(5_528_416));
        assert!(row.status.starts_with("ok (dev mode"), "{}", row.status);
        std::fs::write(dir.join("log"), LOG.replace("94478251", "94478250")).expect("write");
        let second = run(&dir, Path::new("log")).expect("parses");
        assert!(second.rows[0].status.starts_with("fee mismatch"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
