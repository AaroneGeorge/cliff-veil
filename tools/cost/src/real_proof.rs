//! The section "private claim with a real proof" of docs/COSTS.md, from the records `tools/real_proof/run.sh` wrote.
//!
//! Every directory under `evidence/real-proof/` is one run (named by its UTC start time) and must hold a
//! `summary.json`. The CPU figures come from `private-claim.time` and `token.time` (`/usr/bin/time -v`) when present.
//! When a record predates the `reason` field of the control, the reason is read from the sequencer's own log line for
//! that transaction (`control-sequencer-log.txt`, else `sequencer.log`). Runs are shown newest first.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

/// The records, relative to the repository root.
pub const DIR: &str = "evidence/real-proof";

/// The steps of the token part that are privacy-preserving, so proved (e2e/examples/real_proof_token.rs).
const PROVED_TOKEN_STEPS: [&str; 3] = ["fund-private-fee-payer", "init-private-holding", "private-token-claim"];

/// One run of `tools/real_proof/run.sh`.
#[derive(Debug, Clone)]
pub struct Record {
    /// The directory name (UTC start time), e.g. `20261124T155037Z`.
    pub name: String,
    /// `summary.json`.
    pub summary: Value,
    /// `private-claim.time`.
    pub claim_time: Option<GnuTime>,
    /// `token.time`.
    pub token_time: Option<GnuTime>,
    /// Why the sequencer refused the dev-mode control: from `summary.json`, else from its log.
    pub control_reason: Option<String>,
}

/// The CPU lines of a `/usr/bin/time -v` report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GnuTime {
    pub user_seconds: String,
    pub system_seconds: String,
    pub cpu_percent: String,
}

/// The CPU lines of a `/usr/bin/time -v` report; `None` when one is missing.
pub fn parse_gnu_time(text: &str) -> Option<GnuTime> {
    let field = |name: &str| {
        text.lines().find_map(|l| {
            l.trim()
                .strip_prefix(name)?
                .trim()
                .strip_prefix(':')
                .map(|v| v.trim().to_owned())
        })
    };
    Some(GnuTime {
        user_seconds: field("User time (seconds)")?,
        system_seconds: field("System time (seconds)")?,
        cpu_percent: field("Percent of CPU this job got")?,
    })
}

/// Every record under `dir`, newest first. A run directory without a readable `summary.json` is an error, so a record
/// cannot drop out of the document unnoticed.
pub fn load(dir: &Path) -> Result<Vec<Record>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.reverse();
    dirs.iter().map(|d| load_one(d)).collect()
}

fn load_one(dir: &Path) -> Result<Record> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("record directory name {}", dir.display()))?
        .to_owned();
    let path = dir.join("summary.json");
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let summary: Value = serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    if !summary["private_claim"].is_object() {
        bail!("{}: no private_claim", path.display());
    }
    let time = |file: &str| {
        std::fs::read_to_string(dir.join(file))
            .ok()
            .and_then(|t| parse_gnu_time(&t))
    };
    let control_reason = control_reason(dir, &summary);
    Ok(Record {
        name,
        claim_time: time("private-claim.time"),
        token_time: time("token.time"),
        control_reason,
        summary,
    })
}

/// The control's refusal reason: `control_dev_mode_receipt.reason`, else the text after `failed settlement: ` on the
/// sequencer's log line that names the control's transaction hash.
fn control_reason(dir: &Path, summary: &Value) -> Option<String> {
    let control = &summary["control_dev_mode_receipt"];
    if let Some(r) = control["reason"].as_str() {
        return Some(r.to_owned());
    }
    let hash = control["tx_hash"].as_str().map(str::to_owned).or_else(|| {
        let message = control["answer"]["error"]["message"].as_str()?;
        message
            .split(|c: char| !c.is_ascii_hexdigit())
            .find(|w| w.len() == 64)
            .map(str::to_owned)
    })?;
    ["control-sequencer-log.txt", "sequencer.log"].iter().find_map(|file| {
        let log = std::fs::read_to_string(dir.join(file)).ok()?;
        log.lines()
            .filter(|l| l.contains(&hash))
            .find_map(|l| l.split_once("failed settlement: ").map(|(_, r)| r.trim().to_owned()))
    })
}

/// `1234567` as `1,234,567`; a decimal keeps its fraction (`4541.95` as `4,541.95`). Anything else is returned as is.
pub fn thousands(s: &str) -> String {
    let (int, frac) = s.split_once('.').map_or((s, None), |(i, f)| (i, Some(f)));
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return s.to_owned();
    }
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out
}

/// A JSON number or numeric string as text, `-` when absent.
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => "-".to_owned(),
    }
}

fn num(v: &Value) -> String {
    thousands(&text(v))
}

fn short(hash: &Value) -> String {
    let h = text(hash);
    if h.len() > 8 { format!("{}...", &h[..8]) } else { h }
}

/// `GitHub-hosted runner` when the run was a GitHub Actions run, else `machine`.
fn host(s: &Value) -> &'static str {
    if s["run_url"]
        .as_str()
        .is_some_and(|u| u.starts_with("https://github.com/"))
    {
        "GitHub-hosted runner"
    } else {
        "machine"
    }
}

fn token_ok(r: &Record) -> bool {
    r.summary["token_private_claim"]["status"].as_str() == Some("ok")
}

/// The whole section: an introduction, one subsection per record (newest first), and what the runs do not show.
pub fn render(records: &[Record]) -> String {
    if records.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "\n## private claim with a real proof (LEZ v0.4.0-rc3)\n\n\
         Every record under `{DIR}/`, newest first: {}. The figures come from each record's `summary.json` and \
         `private-claim.time` (and `token.time` for the token part); the memory samples are in `memory.tsv`, the \
         privacy scan in `scan.txt`, the sequencer's log in `sequencer.log`. The method and its limits are in \
         [tools/real_proof/README.md](../tools/real_proof/README.md).\n",
        if records.len() == 1 {
            "one run".to_owned()
        } else {
            format!("{} runs", records.len())
        }
    );
    for r in records {
        render_record(&mut out, r);
    }
    let mut not_shown = vec!["how the time scales on other machines (a different runner, a laptop)".to_owned()];
    if records.len() == 1 {
        not_shown.push("a second run on the same kind of machine".to_owned());
    }
    out.push_str(&format!(
        "\nWhat {} not show: {}.",
        if records.len() == 1 {
            "the run does"
        } else {
            "the runs do"
        },
        not_shown.join(", or ")
    ));
    if !records.iter().any(token_ok) {
        out.push_str(
            " Nor a token claim with real proofs (which needs three more proofs): a token private claim is so far \
             measured in dev mode on a sequencer only.",
        );
    }
    out.push('\n');
    out
}

fn render_record(out: &mut String, r: &Record) {
    let s = &r.summary;
    let p = &s["private_claim"];
    let m = &s["machine"];
    let seq = &s["sequencer"];
    let label = if host(s) == "machine" {
        "one run"
    } else {
        "one run on a hosted runner"
    };
    out.push_str(&format!("\n### {}: {label}\n\n", r.name));
    out.push_str(&format!(
        "- source: `{DIR}/{}/`, repository commit `{}`",
        r.name,
        text(&s["repo_commit"])
    ));
    if let Some(url) = s["run_url"].as_str().filter(|u| !u.is_empty()) {
        let id = url.rsplit('/').next().unwrap_or(url);
        out.push_str(&format!(", workflow run [{id}]({url})"));
    }
    out.push('\n');
    out.push_str(&format!(
        "- transaction: `{}` in block {}\n",
        text(&p["hash"]),
        text(&p["block"])
    ));
    let client = if p["dev_mode"] == Value::Bool(false) {
        "unset in the client"
    } else {
        "SET in the client"
    };
    out.push_str(&format!(
        "- RISC0_DEV_MODE: {client} and in the sequencer's environment (`risc0_dev_mode_in_its_environment: {}`)\n",
        text(&seq["risc0_dev_mode_in_its_environment"])
    ));
    out.push_str(&format!(
        "- sequencer: standalone LEZ {} that verifies proofs, {}\n",
        text(&s["lez"]),
        seq["ephemeral"].as_str().unwrap_or("started for this run")
    ));
    out.push_str(&format!(
        "- machine: {}, {} CPUs ({}), {} MiB of memory, {} MiB of swap in total (including any swap file the \
         workflow added as a margin), image `{}`, prover `{}`\n",
        host(s),
        text(&m["cpus"]),
        text(&m["cpu"]),
        num(&m["mem_total_mib"]),
        num(&m["swap_total_mib"]),
        text(&m["image"]),
        text(&m["prover"])
    ));

    let sampled = &p["sampled"];
    out.push_str("\n| Measure | Value |\n|---|---|\n");
    out.push_str(&format!(
        "| Claimed | {} (the private balance after the claim: {}) |\n",
        num(&p["amount"]),
        num(&p["private_balance_after"])
    ));
    out.push_str(&format!(
        "| Network fee estimate | {} |\n",
        num(&p["network_fee_estimate"])
    ));
    out.push_str(&format!(
        "| Cycles (the CLI's estimate before sending) | {} |\n",
        num(&p["cycles"])
    ));
    out.push_str(&format!(
        "| Wall time of the whole CLI command | {} s ({} by `/usr/bin/time -v`) |\n",
        num(&p["wall_seconds"]),
        text(&p["elapsed_gnu_time"])
    ));
    out.push_str(&format!(
        "| Time `r0vm` was seen running | about {} s (sampled every {} s) |\n",
        num(&sampled["r0vm_seen_seconds"]),
        text(&sampled["interval_seconds"])
    ));
    if let Some(t) = &r.claim_time {
        out.push_str(&format!(
            "| CPU use | {} (user {} s, system {} s) |\n",
            t.cpu_percent,
            thousands(&t.user_seconds),
            thousands(&t.system_seconds)
        ));
    }
    out.push_str(&format!(
        "| Peak memory of the command (maximum resident set size) | {} KiB ({} GiB) |\n",
        num(&p["max_rss_kib_gnu_time"]),
        text(&p["max_rss_gib_gnu_time"])
    ));
    out.push_str(&format!(
        "| Peak sampled memory of `r0vm` / of the CLI | {} KiB / {} KiB |\n",
        num(&sampled["r0vm_peak_rss_kib"]),
        num(&sampled["cli_peak_rss_kib"])
    ));
    out.push_str(&format!(
        "| Peak swap in use (sampled) | {} KiB |\n",
        num(&sampled["swap_used_peak_kib"])
    ));
    out.push_str(&format!(
        "| Lowest available memory (sampled) | {} KiB |\n",
        num(&sampled["mem_available_min_kib"])
    ));
    out.push_str(&format!(
        "| Size of the transaction (`getTransaction`) | {} bytes |\n",
        num(&p["transaction_bytes"])
    ));
    let scan = &p["rpc_scan"];
    out.push_str(&format!(
        "| Privacy scan of `getBlock` and `getTransaction` | canonical private account found {} times, one-time \
         address {} times |\n",
        text(&scan["canonical_account_total"]),
        text(&scan["one_time_address_total"])
    ));

    out.push_str(
        "\nThe wall time covers the whole command: local checks, the dry run, the proof, sending, and waiting for the \
         transaction to be included.",
    );
    let (public, cancel) = (&s["public_claim"], &s["cancel"]);
    if public.is_object() && cancel.is_object() {
        out.push_str(&format!(
            " Public transactions carry no proof on LEZ, so the public claim (`{}`, block {}, {}) and the cancel \
             (`{}`, block {}) of the same run were not proved.",
            short(&public["hash"]),
            text(&public["block"]),
            num(&public["amount"]),
            short(&cancel["hash"]),
            text(&cancel["block"])
        ));
    }
    let control = &s["control_dev_mode_receipt"];
    match (control["refused"].as_bool(), &r.control_reason) {
        (Some(true), Some(reason)) => out.push_str(&format!(
            " The same claim made with a dev-mode receipt was not included, and the sequencer's log names the \
             reason: `{reason}`."
        )),
        (Some(true), None) => out.push_str(&format!(
            " The same claim made with a dev-mode receipt was not included (CLI exit {}).",
            text(&control["cli_exit"])
        )),
        _ => out.push_str(" The record shows no refused dev-mode control."),
    }
    out.push_str(" The privacy scan of this transaction is in [PRIVACY.md](PRIVACY.md).\n");
    render_token(out, r);
}

fn render_token(out: &mut String, r: &Record) {
    let t = &r.summary["token_private_claim"];
    if !t.is_object() {
        out.push_str("\nThis run predates the token part of `run.sh`.\n");
        return;
    }
    let status = t["status"].as_str().unwrap_or("unknown");
    out.push_str(&format!(
        "\n**Token claim to a private holding** (status `{status}`, exit {}): a private fee payer funded, the \
         destination's holding initialised, and the claim, each proved.",
        text(&t["exit"])
    ));
    match status {
        "skipped" => {
            out.push_str(" The run skipped it.\n");
            return;
        }
        "ok" => {}
        _ => out.push_str(&format!(
            " It failed: `{}`.",
            t["error"].as_str().unwrap_or("no error recorded; see run.log")
        )),
    }
    out.push('\n');
    if let Some(steps) = t["steps"].as_array().filter(|s| !s.is_empty()) {
        out.push_str(
            "\n| Step | Proved | Transaction | Block | Seconds from send to inclusion |\n|---|---|---|---|---|\n",
        );
        for st in steps {
            let name = text(&st["step"]);
            out.push_str(&format!(
                "| {name} | {} | `{}` | {} | {} |\n",
                if PROVED_TOKEN_STEPS.contains(&name.as_str()) {
                    "yes"
                } else {
                    "no (public)"
                },
                short(&st["hash"]),
                text(&st["block"]),
                num(&st["seconds"])
            ));
        }
    }
    let v = &t["values"];
    let sampled = &t["sampled"];
    let mut rows: Vec<(String, String)> = Vec::new();
    if !v["claim_amount"].is_null() {
        rows.push((
            "Claimed (the private holding after the claim)".into(),
            format!("{} ({})", num(&v["claim_amount"]), num(&v["private_holding_balance"])),
        ));
    }
    if !v["fee_paid_by_private_payer"].is_null() {
        rows.push((
            "Paid by the private fee payer, for initialising the holding and the claim together".into(),
            num(&v["fee_paid_by_private_payer"]),
        ));
    }
    if !t["wall_seconds_all_steps"].is_null() {
        rows.push((
            "Wall time of the whole program (five transactions, three proofs)".into(),
            format!("{} s", num(&t["wall_seconds_all_steps"])),
        ));
    }
    if let Some(tt) = &r.token_time {
        rows.push((
            "CPU use".into(),
            format!(
                "{} (user {} s, system {} s)",
                tt.cpu_percent,
                thousands(&tt.user_seconds),
                thousands(&tt.system_seconds)
            ),
        ));
    }
    if !t["max_rss_kib_gnu_time"].is_null() {
        rows.push((
            "Peak memory of the program (maximum resident set size)".into(),
            format!(
                "{} KiB ({} GiB)",
                num(&t["max_rss_kib_gnu_time"]),
                text(&t["max_rss_gib_gnu_time"])
            ),
        ));
    }
    if sampled.is_object() {
        rows.push((
            "Peak sampled memory of `r0vm` / of the program".into(),
            format!(
                "{} KiB / {} KiB",
                num(&sampled["r0vm_peak_rss_kib"]),
                num(&sampled["client_peak_rss_kib"])
            ),
        ));
        rows.push((
            "Time `r0vm` was seen running".into(),
            format!(
                "about {} s (sampled every {} s)",
                num(&sampled["r0vm_seen_seconds"]),
                text(&sampled["interval_seconds"])
            ),
        ));
    }
    if !t["claim_transaction_bytes"].is_null() {
        rows.push((
            "Size of the claim transaction (`getTransaction`)".into(),
            format!("{} bytes", num(&t["claim_transaction_bytes"])),
        ));
    }
    let scan = &t["rpc_scan"];
    if scan.is_object() {
        rows.push((
            "Privacy scan of the claim's `getBlock` and `getTransaction`".into(),
            format!(
                "destination found {} times, fee payer {} times, one-time address {} times",
                text(&scan["destination_total"]),
                text(&scan["fee_payer_total"]),
                text(&scan["one_time_address_total"])
            ),
        ));
    }
    if !rows.is_empty() {
        out.push_str("\n| Measure | Value |\n|---|---|\n");
        for (k, val) in rows {
            out.push_str(&format!("| {k} | {} |\n", val.replace('|', "\\|")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn native(name: &str) -> Record {
        Record {
            name: name.into(),
            summary: json!({
                "repo_commit": "abc", "lez": "v0.4.0-rc3",
                "run_url": "https://github.com/o/r/actions/runs/1",
                "machine": {"cpus": 4, "cpu": "CPU", "mem_total_mib": 15983, "swap_total_mib": 11263,
                            "image": "ubuntu24 1", "prover": "risc0-r0vm 3.0.5"},
                "sequencer": {"risc0_dev_mode_in_its_environment": false, "ephemeral": "discarded"},
                "control_dev_mode_receipt": {"refused": true, "cli_exit": 1},
                "private_claim": {"hash": "66d6e06d", "block": 193, "amount": "305555555", "dev_mode": false,
                    "private_balance_after": "300026771", "network_fee_estimate": "5528784", "cycles": 57271,
                    "wall_seconds": 1229, "elapsed_gnu_time": "20:29.49", "max_rss_kib_gnu_time": 9612576,
                    "max_rss_gib_gnu_time": 9.17,
                    "sampled": {"r0vm_peak_rss_kib": 9445464, "cli_peak_rss_kib": 37352, "swap_used_peak_kib": 1,
                                "mem_available_min_kib": 2, "r0vm_seen_seconds": 1200, "interval_seconds": 5},
                    "transaction_bytes": 251270,
                    "rpc_scan": {"canonical_account_total": 0, "one_time_address_total": 8}},
                "public_claim": {"hash": "ee6ac7676b9f", "block": 194, "amount": "1138893519"},
                "cancel": {"hash": "7246a352074d", "block": 195}
            }),
            claim_time: parse_gnu_time(
                "\tUser time (seconds): 4541.95\n\tSystem time (seconds): 15.95\n\tPercent of CPU this job got: 370%\n",
            ),
            token_time: None,
            control_reason: Some("Invalid proof".into()),
        }
    }

    #[test]
    fn thousands_groups_the_integer_part_only() {
        assert_eq!(thousands("305555555"), "305,555,555");
        assert_eq!(thousands("4541.95"), "4,541.95");
        assert_eq!(thousands("999"), "999");
        assert_eq!(thousands("20:29.49"), "20:29.49");
    }

    #[test]
    fn one_native_record_is_rendered_from_its_figures() {
        let doc = render(&[native("20261124T155037Z")]);
        assert!(doc.contains("one run. The figures"));
        assert!(doc.contains("### 20261124T155037Z: one run on a hosted runner"));
        assert!(doc.contains("- machine: GitHub-hosted runner, 4 CPUs (CPU), 15,983 MiB of memory"));
        assert!(doc.contains("| Claimed | 305,555,555 (the private balance after the claim: 300,026,771) |"));
        assert!(doc.contains("| CPU use | 370% (user 4,541.95 s, system 15.95 s) |"));
        assert!(doc.contains("| Peak memory of the command (maximum resident set size) | 9,612,576 KiB (9.17 GiB) |"));
        assert!(doc.contains("the public claim (`ee6ac767...`, block 194, 1,138,893,519)"));
        assert!(doc.contains("names the reason: `Invalid proof`"));
        assert!(doc.contains("This run predates the token part"));
        assert!(doc.contains("a second run on the same kind of machine"));
        assert!(doc.contains("Nor a token claim with real proofs"));
    }

    #[test]
    fn token_records_are_shown_newest_first_with_their_status() {
        let mut ok = native("20261124T180000Z");
        ok.summary["token_private_claim"] = json!({
            "status": "ok", "exit": 0, "wall_seconds_all_steps": 3700,
            "steps": [{"step": "create", "hash": "aaaaaaaaaa", "block": 3, "seconds": 15},
                      {"step": "private-token-claim", "hash": "bbbbbbbbbb", "block": 9, "seconds": 1300}],
            "values": {"claim_amount": "1000000", "private_holding_balance": "1000000",
                       "fee_paid_by_private_payer": "22272688"},
            "max_rss_kib_gnu_time": 9700000, "max_rss_gib_gnu_time": 9.25,
            "claim_transaction_bytes": 400000,
            "rpc_scan": {"destination_total": 0, "fee_payer_total": 0, "one_time_address_total": 4}
        });
        let mut failed = native("20261124T170000Z");
        failed.summary["token_private_claim"] = json!({"status": "failed", "exit": 1, "error": "out of memory"});
        let doc = render(&[ok, failed]);
        assert!(doc.contains("2 runs"));
        let newer = doc.find("### 20261124T180000Z").expect("newer run");
        let older = doc.find("### 20261124T170000Z").expect("older run");
        assert!(newer < older);
        assert!(doc.contains("| private-token-claim | yes | `bbbbbbbb...` | 9 | 1,300 |"));
        assert!(doc.contains("| create | no (public) |"));
        assert!(doc.contains("for initialising the holding and the claim together | 22,272,688 |"));
        assert!(doc.contains("destination found 0 times, fee payer 0 times"));
        assert!(doc.contains("It failed: `out of memory`."));
        assert!(!doc.contains("Nor a token claim"));
        assert!(!doc.contains("a second run"));
    }

    #[test]
    fn the_control_reason_is_read_from_the_sequencer_log_when_the_summary_has_none() {
        let dir = std::env::temp_dir().join(format!("cv-cost-real-proof-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let hash = "b6".repeat(32);
        std::fs::write(
            dir.join("sequencer.log"),
            format!("other line\nTransaction with hash {hash} failed settlement: Invalid proof, skipping it\n"),
        )
        .expect("write");
        let summary = json!({"control_dev_mode_receipt": {"answer": {"error": {
            "message": format!("transaction {hash} was not included within 300s")}}}});
        assert_eq!(
            control_reason(&dir, &summary).as_deref(),
            Some("Invalid proof, skipping it")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
