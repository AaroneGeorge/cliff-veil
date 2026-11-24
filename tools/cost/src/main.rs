//! Cost of each `vesting` operation.
//!
//! ```text
//! vesting-cost [--bins DIR] [--json FILE] [--doc FILE]            executor: cycles and bytes of each operation
//! vesting-cost --rpc URL [--json FILE] [--doc FILE]               sequencer: fee charged per operation
//! vesting-cost --private-claim-log FILE [--json FILE] [--doc FILE] private claim: fee read from a CLI run's log
//! vesting-cost --render-only [--doc FILE]                         docs/COSTS.md from the recorded runs
//! ```
//!
//! **Executor mode** runs every operation through LEZ's own public execution (`ValidatedStateDiff`, the code the
//! sequencer runs) on a local `V03State` holding the deployed guests, and records the cycles each transaction used
//! under the chain's 10,000,000-cycle cap. It needs no sequencer and is deterministic. Guests are read from `--bins`
//! (default `$CARGO_TARGET_DIR/guests`): `vesting.bin` (`make guests`), `token.bin` for token rows and
//! `vesting_fees.bin` for the fee rows, with the program ids of the ids file (`CV_IDS`, default
//! `deploy/ids-local-dev.toml`), which are the ids `vesting` was built with. Each row also records the size of the
//! transaction as a sequencer receives it (`LeeTransaction::Public`, Borsh).
//!
//! **Sequencer mode** sends the same operations to a running sequencer (start it with dev mode unset,
//! `scripts/seq.sh pub start`, for real execution) through the end-to-end harness, and records the fee each one
//! was charged (the payer's balance change less what the operation itself moved).
//!
//! **Private-claim mode** reads the JSON a `cliff-veil --json claim --private` run printed into a log (the CLI
//! lifecycle log in `evidence/cli/`) and records its fee, checked against the private balance the wallet reports.
//!
//! An operation whose guest is missing is recorded with its status (`not built`, or the refusal text) instead of a
//! number, so the table always shows what was and was not measured; the recorded evidence must have a number in
//! every row (test `p2_every_rfp_operation_has_a_measured_cost`).
//!
//! Output: one JSON file per mode under `evidence/cost/` and `docs/COSTS.md`, rendered from every JSON file there
//! and from the batch limits in `evidence/batch/max.txt`.

mod executor;
mod private;
mod sequencer;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// One measured (or attempted) operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Row {
    /// Operation name, e.g. `create`, `claim`, `create_batch`.
    pub op: String,
    /// `native` or `token` (or `-` for protocol baselines).
    pub asset: String,
    /// Schedules in the transaction (batch size; 1 otherwise).
    pub n: u64,
    /// Executor cycles of the whole transaction (executor mode), or gas limit declared (sequencer mode).
    pub cycles: Option<u64>,
    /// Native fee charged by the sequencer (sequencer and private-claim modes).
    pub fee: Option<u128>,
    /// Size of the transaction in bytes as a sequencer receives it (executor mode).
    #[serde(default)]
    pub bytes: Option<u64>,
    /// `ok`, `not built`, or why it failed.
    pub status: String,
}

impl Row {
    fn new(op: &str, asset: &str, n: u64) -> Self {
        Self {
            op: op.to_owned(),
            asset: asset.to_owned(),
            n,
            cycles: None,
            fee: None,
            bytes: None,
            status: String::new(),
        }
    }
}

/// One run's results, written as `evidence/cost/<mode>.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Run {
    /// `executor` or `sequencer`.
    pub mode: String,
    /// LEZ version the run used.
    pub lez: String,
    /// What was executed: guest hashes (executor) or the RPC endpoint and whether dev mode was set (sequencer).
    pub inputs: Vec<(String, String)>,
    /// The operations.
    pub rows: Vec<Row>,
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "usage: vesting-cost [--bins DIR | --rpc URL | --private-claim-log FILE] [--json FILE] [--doc FILE] \
             [--render-only]"
        );
        return Ok(());
    }
    let root = repo_root();
    let doc = arg_value(&args, "--doc").map_or_else(|| root.join("docs/COSTS.md"), PathBuf::from);
    let evidence = root.join("evidence/cost");
    if !args.iter().any(|a| a == "--render-only") {
        let run = if let Some(rpc) = arg_value(&args, "--rpc") {
            sequencer::run(&rpc)?
        } else if let Some(log) = arg_value(&args, "--private-claim-log") {
            private::run(&root, Path::new(&log))?
        } else {
            let bins = arg_value(&args, "--bins").map_or_else(default_bins, PathBuf::from);
            executor::run(&bins)?
        };
        let json = arg_value(&args, "--json").map_or_else(
            || evidence.join(format!("{}.json", run.mode.replace(' ', "-"))),
            PathBuf::from,
        );
        if let Some(dir) = json.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&json, serde_json::to_string_pretty(&run)? + "\n")
            .with_context(|| format!("write {}", json.display()))?;
        for r in &run.rows {
            println!(
                "{:<40} {:<7} n={:<4} cycles={:<10} fee={:<10} bytes={:<7} {}",
                r.op,
                r.asset,
                r.n,
                fmt_opt(r.cycles),
                fmt_opt(r.fee),
                fmt_opt(r.bytes),
                r.status
            );
        }
        println!("wrote {}", json.display());
    }
    let runs = load_runs(&evidence)?;
    let maxima = load_batch_maxima(&root.join(BATCH_MAX))?;
    std::fs::create_dir_all(doc.parent().unwrap_or(&root))?;
    std::fs::write(&doc, render(&runs, &maxima)).with_context(|| format!("write {}", doc.display()))?;
    println!("wrote {}", doc.display());
    Ok(())
}

fn default_bins() -> PathBuf {
    std::env::var("CARGO_TARGET_DIR")
        .map_or_else(|_| repo_root().join("../build/target"), PathBuf::from)
        .join("guests")
}

fn fmt_opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| v.to_string())
}

fn load_runs(dir: &Path) -> Result<Vec<Run>> {
    let mut runs = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(runs);
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for p in paths.into_iter().filter(|p| p.extension().is_some_and(|e| e == "json")) {
        let text = std::fs::read_to_string(&p)?;
        runs.push(serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?);
    }
    Ok(runs)
}

/// The batch-limit measurements (SPEC 8), relative to the repository root.
const BATCH_MAX: &str = "evidence/batch/max.txt";

/// One measured batch limit: the largest batch of a shape the chain accepted under its cycle budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchMax {
    /// The item shape, e.g. `native`, `token-64-milestones-with-fee`.
    pub shape: String,
    pub accepted: u64,
    pub refused: u64,
    pub budget: u64,
}

/// The `max <shape> accepted=N refused=M budget=B` lines of `evidence/batch/max.txt`.
fn parse_batch_maxima(text: &str) -> Vec<BatchMax> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            if words.next()? != "max" {
                return None;
            }
            let shape = words.next()?.to_owned();
            let mut field = |name: &str| words.next()?.strip_prefix(name)?.strip_prefix('=')?.parse::<u64>().ok();
            Some(BatchMax {
                shape,
                accepted: field("accepted")?,
                refused: field("refused")?,
                budget: field("budget")?,
            })
        })
        .collect()
}

fn load_batch_maxima(path: &Path) -> Result<Vec<BatchMax>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse_batch_maxima(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// `docs/COSTS.md` from every recorded run and the batch limits. Numbers come only from those files.
fn render(runs: &[Run], maxima: &[BatchMax]) -> String {
    let mut out = String::from(
        "# Cost of each operation\n\nWritten by `cargo run -p vesting_cost` from the files in `evidence/cost/` and \
         `evidence/batch/max.txt`; every number below is copied from those files. Do not edit by hand.\n\n\
         All runs are on LEZ v0.4.0-rc3 (a local standalone sequencer and its executor). LEZ testnet 0.4 has no \
         public endpoint yet; the same commands will be run there when it has.\n\n\
         - **Cycles**: the transaction run through LEZ v0.4.0-rc3's public execution (`ValidatedStateDiff`, \
         the code a sequencer runs), whole transaction: every program it calls (`vesting`, `vesting_fees`, the token \
         or native program, the clock read where the operation has one). The chain's cap is 10,000,000 cycles per \
         transaction and per block.\n\
         - **Bytes**: the size of the transaction as a sequencer receives it (`LeeTransaction::Public`, Borsh). A \
         sequencer accepts a transaction up to its block size less 2,048 bytes (1,048,576 - 2,048 by default).\n\
         - **Fee**: native units charged by a standalone sequencer, from the payer's balance change.\n\
         - `init`, `set_fee`, `check_fee`: messages of `vesting_fees` sent alone, to a deployment of the same binary \
         whose admin key the run holds. Inside a creation `vesting` sends `CheckFee` itself; that cost is part of \
         every `create` row.\n\
         - A row without a number says why: `not built` (a guest was missing from the run) or the refusal.\n",
    );
    if runs.is_empty() {
        out.push_str("\nNo run recorded yet.\n");
    }
    for run in runs {
        out.push_str(&format!("\n## {} run (LEZ {})\n\n", run.mode, run.lez));
        for (k, v) in &run.inputs {
            out.push_str(&format!("- {k}: `{v}`\n"));
        }
        out.push_str(
            "\n| Operation | Asset | Schedules | Cycles | Bytes | Fee | Status |\n|---|---|---|---|---|---|---|\n",
        );
        for r in &run.rows {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                r.op,
                r.asset,
                r.n,
                fmt_opt(r.cycles),
                fmt_opt(r.bytes),
                fmt_opt(r.fee),
                r.status.replace('|', "\\|")
            ));
        }
    }
    if !maxima.is_empty() {
        out.push_str(&format!(
            "\n## Largest batch the chain accepts\n\nFrom `{BATCH_MAX}` (executor, budget of one transaction). A \
             batch over the budget is refused whole (out of gas) and changes nothing. The program caps a batch at \
             85% of the linear maximum: {} native and {} token schedules (`vesting_core::MAX_BATCH_NATIVE`, \
             `MAX_BATCH_TOKEN`); the SDK splits a longer list into several transactions, each filled to an \
             estimated 85% of the cycle budget and never above the cap. The byte size of a batch at the cap is in the executor table above.\n\n\
             | Item shape | Largest accepted | Smallest refused | Cycle budget |\n|---|---|---|---|\n",
            vesting_core::MAX_BATCH_NATIVE,
            vesting_core::MAX_BATCH_TOKEN
        ));
        for m in maxima {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                m.shape, m.accepted, m.refused, m.budget
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendering_copies_numbers_from_the_runs_only() {
        let mut row = Row::new("claim", "native", 1);
        row.cycles = Some(57_332);
        row.status = "ok".into();
        let mut missing = Row::new("cancel", "native", 1);
        missing.status = "not built".into();
        let run = Run {
            mode: "executor".into(),
            lez: "v0.4.0-rc3".into(),
            inputs: vec![("vesting.bin".into(), "ab".into())],
            rows: vec![row, missing],
        };
        let doc = render(&[run], &[]);
        assert!(doc.contains("| claim | native | 1 | 57332 | - | - | ok |"));
        assert!(doc.contains("| cancel | native | 1 | - | - | - | not built |"));
    }

    #[test]
    fn batch_maxima_are_read_from_the_max_lines() {
        let text = "batch native n=193 cycles=9995075 accepted\nmax native accepted=193 refused=194 budget=10000000\n\
                    # max is a comment word\nmax token-64-milestones-with-fee accepted=38 refused=39 budget=10000000\n";
        let maxima = parse_batch_maxima(text);
        assert_eq!(maxima.len(), 2);
        assert_eq!(
            maxima[1],
            BatchMax {
                shape: "token-64-milestones-with-fee".into(),
                accepted: 38,
                refused: 39,
                budget: 10_000_000
            }
        );
        assert!(render(&[], &maxima).contains("| native | 193 | 194 | 10000000 |"));
    }

    /// Operations (and assets) that docs/COSTS.md must show with a measured number: the five the RFP names
    /// (Performance 2), the rest of the program's messages, token and fee paths, and a private claim.
    const MEASURED: [(&str, &str); 20] = [
        ("create", "native"),
        ("claim", "native"),
        ("claim_as_of", "native"),
        ("cancel", "native"),
        ("signal_milestone", "native"),
        ("transfer_beneficiary", "native"),
        ("make_non_cancelable", "native"),
        ("create_batch", "native"),
        ("create_batch (program cap)", "native"),
        ("create", "token"),
        ("claim", "token"),
        ("cancel", "token"),
        ("create_batch", "token"),
        ("create_batch (program cap)", "token"),
        ("create_with_fee", "native"),
        ("create_with_fee", "token"),
        ("init", "-"),
        ("set_fee", "-"),
        ("check_fee", "-"),
        ("claim_private", "native"),
    ];

    /// Performance 2 on the recorded evidence: every operation has a row with a number, and no row of any run is
    /// left without one (a row may only be a refusal the run expected).
    #[test]
    fn p2_every_rfp_operation_has_a_measured_cost() {
        let root = repo_root();
        let runs = load_runs(&root.join("evidence/cost")).expect("evidence/cost parses");
        let rows: Vec<&Row> = runs.iter().flat_map(|r| &r.rows).collect();
        let measured = |r: &Row| r.status.starts_with("ok") && (r.cycles.is_some() || r.fee.is_some());
        for (op, asset) in MEASURED {
            assert!(
                rows.iter()
                    .any(|r| (r.op == op || r.op.starts_with(&format!("{op} ("))) && r.asset == asset && measured(r)),
                "evidence/cost has no measured row for {op} ({asset})"
            );
        }
        for run in &runs {
            for r in &run.rows {
                let expected_refusal = r.op.contains("must be refused") && r.status.starts_with("refused");
                assert!(
                    measured(r) || expected_refusal,
                    "{} run: row without a number: {r:?}",
                    run.mode
                );
            }
        }
        let maxima = load_batch_maxima(&root.join(BATCH_MAX)).expect("batch maxima");
        for shape in [
            "native",
            "token",
            "native-64-milestones-with-fee",
            "token-64-milestones-with-fee",
        ] {
            assert!(
                maxima.iter().any(|m| m.shape == shape && m.accepted < m.refused),
                "{BATCH_MAX}: no maximum for {shape}"
            );
        }
        let doc = std::fs::read_to_string(root.join("docs/COSTS.md")).expect("docs/COSTS.md");
        assert_eq!(
            doc,
            render(&runs, &maxima),
            "docs/COSTS.md is out of date: run `cargo run -p vesting_cost -- --render-only`"
        );
    }

    #[test]
    fn json_roundtrip() {
        let run = Run {
            mode: "sequencer".into(),
            lez: "v0.4.0-rc3".into(),
            inputs: vec![],
            rows: vec![Row::new("create", "native", 1)],
        };
        let back: Run = serde_json::from_str(&serde_json::to_string(&run).expect("json")).expect("parse");
        assert_eq!(back.rows, run.rows);
    }
}
