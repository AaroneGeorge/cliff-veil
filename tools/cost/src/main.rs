//! Cost of each `vesting` operation.
//!
//! ```text
//! vesting-cost [--bins DIR] [--json FILE] [--doc FILE]            executor: cycles of each operation
//! vesting-cost --rpc URL [--json FILE] [--doc FILE]               sequencer: gas and fee charged per operation
//! ```
//!
//! **Executor mode** runs every operation through LEZ's own public execution (`ValidatedStateDiff`, the code the
//! sequencer runs) on a local `V03State` holding the deployed guests, and records the cycles each transaction used
//! under the chain's 10,000,000-cycle cap. It needs no sequencer and is deterministic. Guests are read from `--bins`
//! (default `$CARGO_TARGET_DIR/guests`): `vesting.bin` (`make guests`), and `token.bin` for token rows.
//!
//! **Sequencer mode** sends the same operations to a running sequencer (start it with dev mode unset,
//! `scripts/seq.sh pub start`, for real execution) through the end-to-end harness, and records the fee each one
//! was charged (the payer's balance change less what the operation itself moved).
//!
//! An operation whose program or message does not exist yet is recorded with its status (`not built`, or the
//! refusal text) instead of a number, so the table always shows what was and was not measured. The fee switch's own
//! operations are behind the `fees` feature until `vesting_fees` is built.
//!
//! Output: one JSON file per mode under `evidence/cost/` and `docs/COSTS.md`, rendered from every JSON file there.

mod executor;
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
    /// Native fee charged by the sequencer (sequencer mode).
    pub fee: Option<u128>,
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
        println!("usage: vesting-cost [--bins DIR | --rpc URL] [--json FILE] [--doc FILE] [--render-only]");
        return Ok(());
    }
    let root = repo_root();
    let doc = arg_value(&args, "--doc").map_or_else(|| root.join("docs/COSTS.md"), PathBuf::from);
    let evidence = root.join("evidence/cost");
    if !args.iter().any(|a| a == "--render-only") {
        let run = if let Some(rpc) = arg_value(&args, "--rpc") {
            sequencer::run(&rpc)?
        } else {
            let bins = arg_value(&args, "--bins").map_or_else(default_bins, PathBuf::from);
            executor::run(&bins)?
        };
        let json =
            arg_value(&args, "--json").map_or_else(|| evidence.join(format!("{}.json", run.mode)), PathBuf::from);
        if let Some(dir) = json.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&json, serde_json::to_string_pretty(&run)? + "\n")
            .with_context(|| format!("write {}", json.display()))?;
        for r in &run.rows {
            println!(
                "{:<22} {:<7} n={:<4} cycles={:<10} fee={:<12} {}",
                r.op,
                r.asset,
                r.n,
                fmt_opt(r.cycles),
                fmt_opt(r.fee),
                r.status
            );
        }
        println!("wrote {}", json.display());
    }
    let runs = load_runs(&evidence)?;
    std::fs::create_dir_all(doc.parent().unwrap_or(&root))?;
    std::fs::write(&doc, render(&runs)).with_context(|| format!("write {}", doc.display()))?;
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

/// `docs/COSTS.md` from every recorded run. Numbers come only from the JSON files.
fn render(runs: &[Run]) -> String {
    let mut out = String::from(
        "# Cost of each operation\n\nWritten by `cargo run -p vesting_cost` from the files in `evidence/cost/`; \
         every number below is copied from those files. Do not edit by hand.\n\n\
         - **Executor cycles**: the transaction run through LEZ v0.4.0-rc3's public execution (`ValidatedStateDiff`, \
         the code a sequencer runs), whole transaction including the clock read where the operation has one. The \
         chain's cap is 10,000,000 cycles per transaction and per block.\n\
         - **Sequencer fee**: native units charged by a standalone sequencer, from the payer's balance change.\n\
         - A row without a number says why: `not built` (the program or message does not exist yet) or the refusal.\n",
    );
    if runs.is_empty() {
        out.push_str("\nNo run recorded yet.\n");
    }
    for run in runs {
        out.push_str(&format!("\n## {} run (LEZ {})\n\n", run.mode, run.lez));
        for (k, v) in &run.inputs {
            out.push_str(&format!("- {k}: `{v}`\n"));
        }
        out.push_str("\n| Operation | Asset | Schedules | Cycles | Fee | Status |\n|---|---|---|---|---|---|\n");
        for r in &run.rows {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                r.op,
                r.asset,
                r.n,
                fmt_opt(r.cycles),
                fmt_opt(r.fee),
                r.status.replace('|', "\\|")
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
        let doc = render(&[run]);
        assert!(doc.contains("| claim | native | 1 | 57332 | - | ok |"));
        assert!(doc.contains("| cancel | native | 1 | - | - | not built |"));
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
