//! Requirement map: each RFP requirement and the tests that show it.
//!
//! ```text
//! reqmap [--root DIR]            write docs/REQUIREMENTS.md from docs/requirements.toml and the workspace's tests
//! reqmap --check [--strict]      fail if a hard requirement has no test, or docs/REQUIREMENTS.md is stale;
//!                                --strict also fails when a requirement has only pending tests
//! reqmap --pending CRATE_DIR     print the names of the pending tests of one crate, one per line
//! ```
//!
//! A test is a function carrying `#[test]` or `#[tokio::test]` (proptest's `#[test]` included) in any `.rs` file of
//! the workspace. It is *pending* when it carries `#[ignore = "pending: ..."]`: written, compiled, not yet run.
//! Tests in `tests/executor.rs` and `tests/batch_limit.rs` of the program crates are labelled *executor*: they run the
//! built guests through LEZ's public execution, without a sequencer.
//!
//! A row with a `gap` is *partial*: its tests run, and the gap states what they do not yet show. Every path in a
//! row's `files` must exist, or `--check` fails. Mapping rules are in docs/requirements.toml.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

const TABLE: &str = "docs/requirements.toml";
const OUTPUT: &str = "docs/REQUIREMENTS.md";
/// Directories never scanned for tests.
const SKIP_DIRS: [&str; 5] = ["target", ".git", "node_modules", "build", "vendor"];
/// Groups whose hard requirements must have a test (RFP Supportability 3, and the Privacy section).
const TESTED_GROUPS: [&str; 5] = ["Functionality", "Usability", "Reliability", "Performance", "Privacy"];

#[derive(Debug, Deserialize)]
struct Table {
    requirement: Vec<Requirement>,
}

#[derive(Debug, Clone, Deserialize)]
struct Requirement {
    id: String,
    group: String,
    kind: String,
    text: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    prefixes: Vec<String>,
    #[serde(default)]
    tests: Vec<String>,
    #[serde(default)]
    evidence: Option<String>,
    /// Files (relative to the workspace root) the evidence names; each must exist.
    #[serde(default)]
    files: Vec<String>,
    /// What the tests and evidence do not yet show. A row with a gap is `partial`, never `tested`.
    #[serde(default)]
    gap: Option<String>,
}

impl Requirement {
    fn prefixes(&self) -> Vec<String> {
        if self.prefixes.is_empty() {
            vec![format!("{}_", self.id.to_lowercase())]
        } else {
            self.prefixes.clone()
        }
    }

    fn matches(&self, test: &Test) -> bool {
        self.tests.iter().any(|t| *t == test.qualified() || *t == test.name)
            || self.prefixes().iter().any(|p| test.name.starts_with(p.as_str()))
    }

    fn needs_test(&self) -> bool {
        self.kind == "hard" && TESTED_GROUPS.contains(&self.group.as_str())
    }
}

/// How a test runs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Run {
    /// Runs in every `cargo test`.
    Unit,
    /// Ignored by default; run against a sequencer (or by hand) with `--ignored`.
    Ignored(String),
    /// Written and compiled, waiting for a program message or SDK call.
    Pending(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Test {
    /// Crate directory relative to the workspace root, e.g. `e2e` or `tools/reqmap`.
    krate: String,
    name: String,
    file: String,
    line: usize,
    run: Run,
}

impl Test {
    fn qualified(&self) -> String {
        format!("{}::{}", self.krate, self.name)
    }
}

/// Status of one requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Tested,
    /// Tested or met by evidence, with a stated gap.
    Partial,
    Pending,
    Evidence,
    Missing,
}

impl Status {
    const fn label(self) -> &'static str {
        match self {
            Self::Tested => "tested",
            Self::Partial => "partial",
            Self::Pending => "pending",
            Self::Evidence => "evidence",
            Self::Missing => "MISSING",
        }
    }
}

fn status(req: &Requirement, tests: &[&Test]) -> Status {
    let met = if tests.iter().any(|t| !matches!(t.run, Run::Pending(_))) {
        Status::Tested
    } else if !tests.is_empty() {
        Status::Pending
    } else if req.evidence.is_some() && !req.needs_test() {
        Status::Evidence
    } else {
        Status::Missing
    };
    match met {
        Status::Tested | Status::Evidence if req.gap.is_some() => Status::Partial,
        other => other,
    }
}

// ---------------------------------------------------------------- scanning

/// Finds the tests in one Rust source file.
fn scan_source(krate: &str, file: &str, source: &str) -> Vec<Test> {
    let mut tests = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    // Inside a raw string literal (`r#"` .. `"#`), such as a test fixture: nothing there is code.
    let mut in_raw = false;
    for (index, raw) in source.lines().enumerate() {
        let line = raw.trim();
        if in_raw {
            in_raw = !line.contains("\"#");
            continue;
        }
        if let Some(start) = line.find("r#\"") {
            in_raw = !line[start + 3..].contains("\"#");
            attrs.clear();
            continue;
        }
        if line.starts_with("#[") {
            attrs.push(line.to_owned());
            continue;
        }
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if let Some(name) = fn_name(line)
            && attrs.iter().any(|a| is_test_attr(a))
        {
            tests.push(Test {
                krate: krate.to_owned(),
                name: name.to_owned(),
                file: file.to_owned(),
                line: index + 1,
                run: run_of(&attrs),
            });
        }
        attrs.clear();
    }
    tests
}

fn is_test_attr(attr: &str) -> bool {
    attr == "#[test]" || attr.starts_with("#[tokio::test")
}

fn run_of(attrs: &[String]) -> Run {
    for attr in attrs {
        if attr == "#[ignore]" {
            return Run::Ignored(String::new());
        }
        if let Some(rest) = attr.strip_prefix("#[ignore") {
            let reason = rest
                .split_once('"')
                .and_then(|(_, r)| r.rsplit_once('"'))
                .map_or("", |(r, _)| r)
                .to_owned();
            return if reason.starts_with("pending") {
                Run::Pending(reason)
            } else {
                Run::Ignored(reason)
            };
        }
    }
    Run::Unit
}

/// The function name of a `fn` item line, if it is one.
fn fn_name(line: &str) -> Option<&str> {
    let mut rest = line;
    for qualifier in ["pub(crate) ", "pub ", "async ", "unsafe "] {
        rest = rest.strip_prefix(qualifier).unwrap_or(rest);
    }
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("fn ")?;
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    (end > 0).then(|| &rest[..end])
}

fn scan_workspace(root: &Path) -> Result<Vec<Test>> {
    let mut files = Vec::new();
    collect_rs(root, &mut files)?;
    files.sort();
    let mut tests = Vec::new();
    for file in files {
        let source = std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        let rel = file.strip_prefix(root).unwrap_or(&file);
        let krate = crate_dir(root, &file);
        tests.extend(scan_source(&krate, &rel.display().to_string(), &source));
    }
    Ok(tests)
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if !SKIP_DIRS.contains(&name.as_ref()) && !name.starts_with('.') {
                collect_rs(&path, out)?;
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// The directory of the nearest `Cargo.toml` above `file`, relative to `root` (`.` for the root itself).
fn crate_dir(root: &Path, file: &Path) -> String {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() {
            let rel = d.strip_prefix(root).unwrap_or(d).display().to_string();
            return if rel.is_empty() { ".".to_owned() } else { rel };
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }
    ".".to_owned()
}

// ---------------------------------------------------------------- report

struct Report<'a> {
    rows: Vec<(&'a Requirement, Vec<&'a Test>, Status)>,
    unmapped: Vec<&'a Test>,
}

fn build<'a>(reqs: &'a [Requirement], tests: &'a [Test]) -> Report<'a> {
    let rows = reqs
        .iter()
        .map(|r| {
            let mapped: Vec<&Test> = tests.iter().filter(|t| r.matches(t)).collect();
            let s = status(r, &mapped);
            (r, mapped, s)
        })
        .collect();
    let unmapped = tests.iter().filter(|t| !reqs.iter().any(|r| r.matches(t))).collect();
    Report { rows, unmapped }
}

/// Whether `test` runs the built guests through LEZ's executor (the program crates' executor test files).
fn is_executor(test: &Test) -> bool {
    test.krate.starts_with("program")
        && (test.file.ends_with("tests/executor.rs") || test.file.ends_with("tests/batch_limit.rs"))
}

fn run_label(test: &Test) -> String {
    match &test.run {
        Run::Unit if is_executor(test) => "executor".to_owned(),
        Run::Unit => "unit".to_owned(),
        Run::Ignored(_) if test.krate == "e2e" => "sequencer".to_owned(),
        Run::Ignored(reason) if is_executor(test) => format!("executor, ignored ({reason})"),
        Run::Ignored(reason) => format!("ignored ({reason})"),
        Run::Pending(reason) => reason.clone(),
    }
}

fn render(report: &Report<'_>) -> String {
    let mut out = String::new();
    let count = |s: Status, kind: &str| {
        report
            .rows
            .iter()
            .filter(|(r, _, st)| *st == s && r.kind == kind)
            .count()
    };
    let hard = report.rows.iter().filter(|(r, _, _)| r.kind == "hard").count();
    let _ = writeln!(out, "# Requirements and the tests that show them\n");
    let _ = writeln!(
        out,
        "Written by `cargo run -p reqmap` from `{TABLE}` and the tests of this workspace. Do not edit by hand: CI \
         runs `cargo run -p reqmap -- --check`, which fails when a hard requirement has no test or when this file is \
         out of date.\n"
    );
    let _ = writeln!(
        out,
        "Status: **tested** = at least one test that runs (in `cargo test`, or against a standalone sequencer in the \
         `e2e` CI job); **partial** = tests or evidence exist, and the row states what they do not yet show; \
         **pending** = tests written and compiled that wait for a program message or SDK call; **evidence** = met by \
         the file or CI job named; **MISSING** = nothing yet.\n\n\
         How a test runs: (unit) in `cargo test`; (executor) the built guests run through LEZ v0.4.0-rc3's public \
         execution, the code a sequencer runs, without a sequencer; (sequencer) against a standalone LEZ sequencer.\n"
    );
    let _ = writeln!(
        out,
        "Hard requirements ({hard} rows, numbered requirements and their parts): {} tested, {} partial, {} pending, \
         {} evidence, {} missing.\n",
        count(Status::Tested, "hard"),
        count(Status::Partial, "hard"),
        count(Status::Pending, "hard"),
        count(Status::Evidence, "hard"),
        count(Status::Missing, "hard"),
    );
    let mut group = "";
    for (req, tests, st) in &report.rows {
        if req.group != group {
            group = &req.group;
            let _ = writeln!(out, "\n## {group}\n");
            let _ = writeln!(out, "| ID | Requirement (RFP text) | Tests | Status |");
            let _ = writeln!(out, "|---|---|---|---|");
        }
        let id = req
            .parent
            .as_ref()
            .map_or_else(|| req.id.clone(), |p| format!("{} (part of {p})", req.id));
        let mut cell = tests
            .iter()
            .map(|t| format!("`{}` ({})", t.qualified(), run_label(t)))
            .collect::<Vec<_>>()
            .join("<br>");
        if let Some(evidence) = &req.evidence {
            if !cell.is_empty() {
                cell.push_str("<br>");
            }
            let _ = write!(cell, "evidence: {evidence}");
        }
        if !req.files.is_empty() {
            if !cell.is_empty() {
                cell.push_str("<br>");
            }
            let files: Vec<String> = req.files.iter().map(|f| format!("`{f}`")).collect();
            let _ = write!(cell, "files: {}", files.join(", "));
        }
        if cell.is_empty() {
            cell.push('-');
        }
        let status = match (&req.gap, st) {
            (Some(gap), Status::Partial) => format!("partial: {}", escape(gap)),
            _ => st.label().to_owned(),
        };
        let _ = writeln!(out, "| {id} | {} | {cell} | {status} |", escape(&req.text));
    }
    if !report.unmapped.is_empty() {
        let _ = writeln!(out, "\n## Other tests\n");
        let _ = writeln!(
            out,
            "Tests that map to no requirement row (internal properties, helpers):\n"
        );
        let mut by_crate: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for t in &report.unmapped {
            by_crate.entry(&t.krate).or_default().push(&t.name);
        }
        for (krate, names) in by_crate {
            let _ = writeln!(out, "- `{krate}`: {} tests", names.len());
        }
    }
    out
}

fn escape(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Problems that make `--check` fail. `root` is where the rows' `files` are looked up.
fn problems(report: &Report<'_>, strict: bool, root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for (req, _, _) in &report.rows {
        for file in &req.files {
            if !root.join(file).exists() {
                found.push(format!("{}: evidence file {file} is missing", req.id));
            }
        }
    }
    for (req, _, st) in &report.rows {
        match st {
            Status::Missing if req.kind == "hard" => found.push(format!("{}: hard requirement with no test", req.id)),
            Status::Pending if strict && req.kind == "hard" => {
                found.push(format!("{}: only pending tests", req.id));
            }
            _ => {}
        }
    }
    found
}

// ---------------------------------------------------------------- main

struct Args {
    root: PathBuf,
    check: bool,
    strict: bool,
    pending: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        root: workspace_root(),
        check: false,
        strict: false,
        pending: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--root" => args.root = PathBuf::from(it.next().context("--root needs a directory")?),
            "--check" => args.check = true,
            "--strict" => args.strict = true,
            "--pending" => args.pending = Some(it.next().context("--pending needs a crate directory")?),
            "-h" | "--help" => {
                println!("usage: reqmap [--root DIR] [--check [--strict]] [--pending CRATE_DIR]");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(args)
}

/// The workspace root: two levels above this crate's manifest (`tools/reqmap`).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_table(root: &Path) -> Result<Vec<Requirement>> {
    let path = root.join(TABLE);
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let table: Table = toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let mut seen = std::collections::BTreeSet::new();
    for r in &table.requirement {
        if !seen.insert(r.id.clone()) {
            bail!("{TABLE}: duplicate id {}", r.id);
        }
        if !["hard", "soft", "fee"].contains(&r.kind.as_str()) {
            bail!("{TABLE}: {} has unknown kind {}", r.id, r.kind);
        }
    }
    Ok(table.requirement)
}

fn run() -> Result<ExitCode> {
    let args = parse_args()?;
    let root = args
        .root
        .canonicalize()
        .with_context(|| format!("root {}", args.root.display()))?;
    let tests = scan_workspace(&root)?;
    if let Some(krate) = &args.pending {
        for t in tests
            .iter()
            .filter(|t| &t.krate == krate && matches!(t.run, Run::Pending(_)))
        {
            println!("{}", t.name);
        }
        return Ok(ExitCode::SUCCESS);
    }
    let reqs = load_table(&root)?;
    let report = build(&reqs, &tests);
    let text = render(&report);
    let out = root.join(OUTPUT);
    if !args.check {
        std::fs::write(&out, &text).with_context(|| format!("write {}", out.display()))?;
        println!("wrote {OUTPUT}: {} requirements, {} tests", reqs.len(), tests.len());
        return Ok(ExitCode::SUCCESS);
    }
    let mut found = problems(&report, args.strict, &root);
    let current = std::fs::read_to_string(&out).unwrap_or_default();
    if current != text {
        found.push(format!(
            "{OUTPUT} is out of date: run `cargo run -p reqmap` and commit it"
        ));
    }
    for (req, tests, st) in &report.rows {
        println!("{:<6} {:<9} {} tests", req.id, st.label(), tests.len());
    }
    if found.is_empty() {
        println!("reqmap: OK ({} requirements, {} tests)", reqs.len(), tests.len());
        Ok(ExitCode::SUCCESS)
    } else {
        for f in &found {
            eprintln!("reqmap: {f}");
        }
        Ok(ExitCode::FAILURE)
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("reqmap: {e:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
#[test]
fn f1a_cliff_then_linear() {}

#[tokio::test]
#[ignore = "needs a sequencer"]
async fn r3_concurrent_claims() {}

/// Doc comment between attribute and fn.
#[test]
#[ignore = "pending: program message Cancel"]
fn f3_cancel_refund_exact() {}

fn helper() {}

#[test]
fn prop_vested_monotone() {}
"#;

    fn req(id: &str, group: &str, kind: &str) -> Requirement {
        Requirement {
            id: id.into(),
            group: group.into(),
            kind: kind.into(),
            text: "text".into(),
            parent: None,
            prefixes: Vec::new(),
            tests: Vec::new(),
            evidence: None,
            files: Vec::new(),
            gap: None,
        }
    }

    #[test]
    fn finds_tests_and_how_they_run() {
        let tests = scan_source("e2e", "e2e/tests/x.rs", SOURCE);
        let names: Vec<_> = tests.iter().map(|t| (t.name.as_str(), t.run.clone())).collect();
        assert_eq!(
            names,
            vec![
                ("f1a_cliff_then_linear", Run::Unit),
                ("r3_concurrent_claims", Run::Ignored("needs a sequencer".into())),
                (
                    "f3_cancel_refund_exact",
                    Run::Pending("pending: program message Cancel".into())
                ),
                ("prop_vested_monotone", Run::Unit),
            ]
        );
        assert_eq!(tests[0].line, 3);
    }

    #[test]
    fn maps_by_prefix_and_by_table() {
        let tests = scan_source("core", "core/tests/p.rs", SOURCE);
        let mut f1 = req("F1", "Functionality", "hard");
        f1.tests = vec!["core::prop_vested_monotone".into()];
        let f1a = req("F1a", "Functionality", "hard");
        assert!(f1.matches(&tests[3]));
        assert!(!f1.matches(&tests[0]), "f1a_ does not start with f1_");
        assert!(f1a.matches(&tests[0]));
    }

    #[test]
    fn s3_every_hard_requirement_needs_a_test() {
        let tests = scan_source("e2e", "e2e/tests/x.rs", SOURCE);
        let reqs = vec![
            req("F1a", "Functionality", "hard"),
            req("F3", "Functionality", "hard"),
            req("R4", "Reliability", "hard"),
            req("Soft1", "Soft", "soft"),
        ];
        let report = build(&reqs, &tests);
        let st: Vec<_> = report.rows.iter().map(|(r, _, s)| (r.id.as_str(), *s)).collect();
        assert_eq!(
            st,
            vec![
                ("F1a", Status::Tested),
                ("F3", Status::Pending),
                ("R4", Status::Missing),
                ("Soft1", Status::Missing)
            ]
        );
        let root = workspace_root();
        assert_eq!(
            problems(&report, false, &root),
            vec!["R4: hard requirement with no test".to_owned()]
        );
        assert_eq!(
            problems(&report, true, &root).len(),
            2,
            "strict also refuses pending-only rows"
        );
    }

    #[test]
    fn supportability_rows_may_be_met_by_evidence() {
        let mut s2 = req("S2", "Supportability", "hard");
        s2.evidence = Some("the e2e CI job".into());
        assert_eq!(status(&s2, &[]), Status::Evidence);
        let mut u1 = req("U1", "Usability", "hard");
        u1.evidence = Some("not enough".into());
        assert_eq!(status(&u1, &[]), Status::Missing, "a tested group needs a test");
    }

    #[test]
    fn a_row_with_a_gap_is_partial_and_counted_as_such() {
        let tests = scan_source("e2e", "e2e/tests/x.rs", SOURCE);
        let mut f1a = req("F1a", "Functionality", "hard");
        f1a.gap = Some("not shown in the GUI".into());
        let mut s1 = req("S1", "Supportability", "hard");
        s1.evidence = Some("a local run".into());
        s1.gap = Some("testnet not live".into());
        let mut f3 = req("F3", "Functionality", "hard");
        f3.gap = Some("gap on a pending-only row".into());
        let reqs = vec![f1a, s1, f3];
        let report = build(&reqs, &tests);
        let st: Vec<_> = report.rows.iter().map(|(_, _, s)| *s).collect();
        assert_eq!(st, vec![Status::Partial, Status::Partial, Status::Pending]);
        let text = render(&report);
        assert!(text.contains("| partial: not shown in the GUI |"), "{text}");
        assert!(text.contains("2 partial"), "{text}");
    }

    #[test]
    fn a_missing_evidence_file_fails_the_check() {
        let mut s4 = req("S4", "Supportability", "hard");
        s4.evidence = Some("README".into());
        s4.files = vec!["README.md".into(), "docs/no-such-file.md".into()];
        let reqs = vec![s4];
        let report = build(&reqs, &[]);
        assert_eq!(
            problems(&report, false, &workspace_root()),
            vec!["S4: evidence file docs/no-such-file.md is missing".to_owned()]
        );
    }

    #[test]
    fn executor_tests_are_labelled_executor() {
        let tests = scan_source("program", "program/tests/executor.rs", "#[test]\nfn f6_events() {}\n");
        assert_eq!(run_label(&tests[0]), "executor");
        let tests = scan_source("program/fees", "program/fees/src/lib.rs", "#[test]\nfn fee_x() {}\n");
        assert_eq!(run_label(&tests[0]), "unit");
    }

    #[test]
    fn every_evidence_file_named_in_the_table_exists() {
        let root = workspace_root();
        let reqs = load_table(&root).expect("table");
        for r in &reqs {
            for f in &r.files {
                assert!(root.join(f).exists(), "{}: {f} is missing", r.id);
            }
        }
    }

    #[test]
    fn text_inside_a_raw_string_is_not_scanned() {
        let source = format!("const X: &str = r#\"\n{SOURCE}\n\"#;\n\n#[test]\nfn after() {{}}\n");
        let names: Vec<_> = scan_source("x", "x.rs", &source).into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["after".to_owned()]);
    }

    #[test]
    fn fn_names() {
        assert_eq!(fn_name("pub async fn r3_x() {"), Some("r3_x"));
        assert_eq!(fn_name("fn prop_a(x in 0..3u64) {"), Some("prop_a"));
        assert_eq!(fn_name("let f = 3;"), None);
    }

    #[test]
    fn the_requirements_table_parses_and_lists_all_26_hard_requirements() {
        let reqs = load_table(&workspace_root()).expect("table");
        let numbered = reqs.iter().filter(|r| r.kind == "hard" && r.parent.is_none()).count();
        assert_eq!(numbered, 26);
        assert_eq!(reqs.iter().filter(|r| r.kind == "soft").count(), 2);
        assert_eq!(reqs.iter().filter(|r| r.kind == "fee").count(), 4);
    }
}
