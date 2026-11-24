//! How the mini-app reaches the SDK: the `cliff-veil` command line (`cli/`) with `--json`.
//!
//! The SDK links the LEZ wallet and the RISC Zero host crates. Those stay out of the Qt plugin process: the core
//! module runs the command line, which is the SDK's own front end, once per request, and reads the one JSON object it
//! prints ([`crate::cli`] reads it). Every request maps to one argument list in [`argv`].
//!
//! Every operation runs twice: first with `--dry-run` (checked against current state and priced, nothing sent), and,
//! after the person confirms in the mini-app, with `--yes`. For a claim to a private account the module passes
//! `--yes` only after the privacy disclosure of that claim was acknowledged ([`crate::claim::ClaimFlow`]); the
//! command line itself never asks, because the module is not a terminal.
//!
//! Wallet and program ids come from the command line's environment, set in the mini-app's Settings:
//! `LEE_WALLET_HOME_DIR`, `LEE_WALLET_PASSWORD`, and `CV_IDS` (ids file) or `CV_VESTING` (program id).

use std::process::Command;

use serde_json::Value;

use crate::claim::{ClaimPath, ClaimRequest};
use crate::form::CreateRequest;
use crate::model::{Asset, ErrorInfo, Kind};

/// Dry run (check and price, send nothing) or send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `--dry-run`.
    DryRun,
    /// `--yes`.
    Send,
}

/// Whose schedules to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Schedules a creator made.
    Creator,
    /// Schedules paying a beneficiary.
    Beneficiary,
}

/// A request to the SDK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// One schedule (`show`).
    Status {
        /// Schedule account.
        schedule: String,
    },
    /// Schedules of an account. The command line has no `list` yet: this request is refused until it does, and
    /// the views fall back to schedule accounts typed in.
    List {
        /// Creator or beneficiary.
        role: Role,
        /// The account.
        account: String,
    },
    /// A claim.
    Claim {
        /// The claim.
        request: ClaimRequest,
        /// Dry run or send.
        mode: Mode,
    },
    /// A creation.
    Create {
        /// The checked form.
        request: CreateRequest,
        /// Dry run or send.
        mode: Mode,
    },
    /// A cancel.
    Cancel {
        /// Schedule account.
        schedule: String,
        /// The cancel authority (signer).
        authority: String,
        /// Dry run or send.
        mode: Mode,
    },
    /// A milestone signal.
    Signal {
        /// Schedule account.
        schedule: String,
        /// The milestone authority (signer).
        authority: String,
        /// Milestone index.
        index: u8,
        /// Dry run or send.
        mode: Mode,
    },
    /// The privacy disclosure text.
    Disclosure,
}

fn mode_flag(mode: Mode) -> &'static str {
    match mode {
        Mode::DryRun => "--dry-run",
        Mode::Send => "--yes",
    }
}

/// The command-line arguments of `request` (after the program name). `--json` comes first; `--dry-run` / `--yes`
/// last (both are global flags of the command line).
#[must_use]
pub fn argv(request: &Request) -> Vec<String> {
    let mut a: Vec<String> = vec!["--json".into()];
    let mut push = |items: &[&str]| a.extend(items.iter().map(|s| (*s).to_owned()));
    match request {
        Request::Status { schedule } => push(&["show", schedule]),
        Request::Disclosure => push(&["disclosure"]),
        Request::List { role, account } => push(&[
            "list",
            match role {
                Role::Creator => "--creator",
                Role::Beneficiary => "--beneficiary",
            },
            account,
        ]),
        Request::Claim { request, mode } => {
            push(&["claim", &request.schedule, "--as", &request.beneficiary]);
            match &request.path {
                ClaimPath::Public { to } => {
                    if let Some(to) = to {
                        push(&["--to", to]);
                    }
                }
                ClaimPath::Private { account, fee_payer } => {
                    push(&["--private"]);
                    if let Some(p) = account {
                        push(&["--private-to", p]);
                    }
                    if let Some(p) = fee_payer {
                        push(&["--fee-payer", p]);
                    }
                }
            }
            push(&[mode_flag(*mode)]);
        }
        Request::Create { request: r, mode } => {
            push(&["create", "--from", &r.creator, "--beneficiary", &r.beneficiary]);
            if let Asset::Token { definition } = &r.asset {
                push(&["--token", definition]);
            }
            match &r.kind {
                Kind::Linear { start, cliff, end } => {
                    push(&["--total", &r.total.to_string()]);
                    push(&[
                        "--start",
                        &start.to_string(),
                        "--cliff",
                        &cliff.to_string(),
                        "--end",
                        &end.to_string(),
                    ]);
                }
                Kind::Milestones { amounts, .. } => {
                    let list: Vec<String> = amounts.iter().map(u128::to_string).collect();
                    push(&["--milestones", &list.join(",")]);
                }
            }
            if !r.cancelable {
                push(&["--non-cancelable"]);
            }
            if r.transferable {
                push(&["--transferable"]);
            }
            for (flag, v) in [
                ("--cancel-authority", &r.cancel_authority),
                ("--milestone-authority", &r.milestone_authority),
                ("--refund-to", &r.refund_to),
                ("--label", &r.label),
                ("--id", &r.id),
            ] {
                if let Some(v) = v {
                    push(&[flag, v]);
                }
            }
            push(&[mode_flag(*mode)]);
        }
        Request::Cancel {
            schedule,
            authority,
            mode,
        } => {
            push(&["cancel", schedule, "--as", authority, mode_flag(*mode)]);
        }
        Request::Signal {
            schedule,
            authority,
            index,
            mode,
        } => {
            push(&[
                "signal",
                schedule,
                "--index",
                &index.to_string(),
                "--as",
                authority,
                mode_flag(*mode),
            ]);
        }
    }
    a
}

/// Something that answers SDK requests with the command line's JSON object.
pub trait Backend {
    /// Runs `request`; the JSON answer or the refusal.
    ///
    /// # Errors
    /// The SDK's refusal, or `backend_unavailable` / `backend_bad_output`.
    fn call(&mut self, request: &Request) -> Result<Value, ErrorInfo>;
}

/// The JSON object a command printed. A private claim makes the LEZ wallet sync its private accounts, and the wallet
/// prints that progress (`Latest block is ...`, `Syncing to block ...`) on standard output before the object; so when
/// the whole output does not read, the object is the text from the first line that opens with `{` to the end.
fn document(stdout: &str) -> Option<Value> {
    let t = stdout.trim();
    serde_json::from_str(t).ok().or_else(|| {
        t.match_indices('\n')
            .map(|(i, _)| &t[i + 1..])
            .filter(|rest| rest.starts_with('{'))
            .find_map(|rest| serde_json::from_str(rest).ok())
    })
}

/// Reads a command's output: the object on success, else the refusal it printed ([`crate::cli::error`]).
///
/// # Errors
/// The printed refusal; `backend_bad_output` when the output is not the expected JSON (for example an argument the
/// command line does not know, which it reports as text).
pub fn read_output(success: bool, stdout: &str, stderr: &str) -> Result<Value, ErrorInfo> {
    let parsed = document(stdout);
    if let Some(e) = parsed.as_ref().and_then(crate::cli::error) {
        return Err(e);
    }
    let tail = |s: &str| {
        s.lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    };
    match (success, parsed) {
        (true, Some(v)) => Ok(v),
        (ok, _) => Err(ErrorInfo::new(
            "backend_bad_output",
            format!(
                "the cliff-veil command line {} without the expected JSON answer: {}",
                if ok { "succeeded" } else { "failed" },
                tail(if stderr.trim().is_empty() { stdout } else { stderr })
            ),
            Some("check the command line path and its environment (wallet home, program ids) in the Settings tab"),
        )),
    }
}

/// The `cliff-veil` command line as a backend.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliBackend {
    /// Path of the `cliff-veil` binary.
    pub program: String,
    /// Extra environment (`LEE_WALLET_HOME_DIR`, `CV_IDS` or `CV_VESTING`, `RISC0_DEV_MODE`, ...).
    pub env: Vec<(String, String)>,
}

impl Backend for CliBackend {
    fn call(&mut self, request: &Request) -> Result<Value, ErrorInfo> {
        if self.program.is_empty() {
            return Err(ErrorInfo::new(
                "backend_not_configured",
                "the path of the cliff-veil command line is not set",
                Some("set it in the Settings tab (or call configure)"),
            ));
        }
        let out = Command::new(&self.program)
            .args(argv(request))
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                ErrorInfo::new(
                    "backend_unavailable",
                    format!("cannot run {}: {e}", self.program),
                    Some("check the command line path in the Settings tab"),
                )
            })?;
        read_output(
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::id;
    use crate::claim::tests::{private_request, public_request};
    use crate::form::{tests::linear_form, validate};

    #[test]
    fn u5_claim_arguments_send_only_with_yes() {
        let args = |request, mode| argv(&Request::Claim { request, mode });
        assert_eq!(
            args(public_request(), Mode::Send),
            ["--json", "claim", &id(1), "--as", &id(3), "--yes"]
        );
        let private = ClaimRequest {
            path: ClaimPath::Private {
                account: Some(id(8)),
                fee_payer: Some(id(9)),
            },
            ..private_request()
        };
        assert_eq!(
            args(private.clone(), Mode::Send),
            [
                "--json",
                "claim",
                &id(1),
                "--as",
                &id(3),
                "--private",
                "--private-to",
                &id(8),
                "--fee-payer",
                &id(9),
                "--yes"
            ]
        );
        let dry = args(private, Mode::DryRun);
        assert_eq!(dry.last().unwrap(), "--dry-run");
        assert!(!dry.contains(&"--yes".to_owned()), "a dry run never carries --yes");
    }

    #[test]
    fn create_arguments_carry_every_parameter() {
        let mut r = validate(&linear_form()).unwrap();
        r.cancelable = false;
        r.transferable = true;
        r.refund_to = Some(id(9));
        r.label = Some("team".into());
        let a = argv(&Request::Create {
            request: r.clone(),
            mode: Mode::DryRun,
        })
        .join(" ");
        for part in [
            "create --from",
            "--total 1000",
            "--start 1767225600000 --cliff 1775001600000 --end 1798761600000",
            "--non-cancelable",
            "--transferable",
            &format!("--refund-to {}", id(9)),
            "--label team",
            "--dry-run",
        ] {
            assert!(a.contains(part), "missing `{part}` in `{a}`");
        }
        assert!(!a.contains("--cancel-authority") && !a.contains("--id"));
        r.label = None;
        r.id = Some("ab".repeat(32));
        let a = argv(&Request::Create {
            request: r,
            mode: Mode::Send,
        })
        .join(" ");
        assert!(a.contains(&format!("--id {}", "ab".repeat(32))) && a.ends_with("--yes"));
    }

    #[test]
    fn authority_and_read_arguments() {
        let s = |r: Request| argv(&r).join(" ");
        assert_eq!(s(Request::Status { schedule: "S".into() }), "--json show S");
        assert_eq!(s(Request::Disclosure), "--json disclosure");
        assert_eq!(
            s(Request::List {
                role: Role::Creator,
                account: "C".into()
            }),
            "--json list --creator C"
        );
        assert_eq!(
            s(Request::Cancel {
                schedule: "S".into(),
                authority: "A".into(),
                mode: Mode::Send
            }),
            "--json cancel S --as A --yes"
        );
        assert_eq!(
            s(Request::Signal {
                schedule: "S".into(),
                authority: "A".into(),
                index: 2,
                mode: Mode::DryRun
            }),
            "--json signal S --index 2 --as A --dry-run"
        );
    }

    #[test]
    fn reads_answers_and_refusals() {
        assert_eq!(
            read_output(true, "{\"ok\":true,\"a\":1}\n", ""),
            Ok(serde_json::json!({"ok": true, "a": 1}))
        );
        let e = read_output(
            false,
            r#"{"ok":false,"error":{"code":14,"name":"NothingClaimable","message":"nothing claimable yet","action":null}}"#,
            "",
        )
        .unwrap_err();
        assert_eq!((e.code.as_str(), e.program_code), ("nothing_claimable", Some(14)));
        // clap reports an unknown subcommand as text on stderr.
        let e = read_output(false, "", "error: unrecognized subcommand 'list'\n").unwrap_err();
        assert_eq!(e.code, "backend_bad_output");
        assert!(e.message.contains("unrecognized subcommand 'list'"));
    }

    #[test]
    fn reads_a_private_claim_after_the_wallet_sync_lines() {
        // What `cliff-veil --json claim ... --private --yes` prints on local-dev: the wallet's sync progress, then the
        // object (dev-mode warnings go to stderr).
        let stdout = "Latest block is 1951\nSyncing to block 1951. Blocks to sync: 3\nSynced to block 1951 in 151ms\n\
                      {\n  \"ok\": true,\n  \"sent\": [\n    {\"block\": 1952, \"hash\": \"ab\"}\n  ]\n}\n";
        let stderr = "WARNING: proving in dev mode. This will not generate valid, secure proofs.\n";
        let v = read_output(true, stdout, stderr).unwrap();
        assert_eq!(v["sent"][0]["block"], 1952);
        // A refusal after the sync lines is still the SDK's refusal, not unreadable output.
        let refused = "Latest block is 7\n{\"ok\":false,\"error\":{\"code\":14,\"name\":\"NothingClaimable\",\
                       \"message\":\"nothing claimable yet\",\"action\":null}}";
        assert_eq!(read_output(false, refused, "").unwrap_err().code, "nothing_claimable");
        // Text with no object in it stays unreadable.
        assert_eq!(
            read_output(true, "Latest block is 7\n", "").unwrap_err().code,
            "backend_bad_output"
        );
    }

    #[test]
    fn unconfigured_or_missing_program_is_reported() {
        let req = Request::Status { schedule: "S".into() };
        assert_eq!(
            CliBackend::default().call(&req).unwrap_err().code,
            "backend_not_configured"
        );
        let mut missing = CliBackend {
            program: "/nonexistent/cliff-veil".into(),
            env: vec![],
        };
        assert_eq!(missing.call(&req).unwrap_err().code, "backend_unavailable");
    }

    /// Every argument list [`argv`] builds is accepted by the real command line. Run with
    /// `CLIFF_VEIL_CLI=<path of cliff-veil> cargo test -- --ignored`. The ids file is pointed at a missing path, so
    /// every command except `disclosure` stops right after parsing its arguments, before any wallet or network: a
    /// JSON answer proves the arguments parsed, an argument error is printed as text instead.
    #[test]
    #[ignore = "needs CLIFF_VEIL_CLI: the path of a built cliff-veil binary"]
    fn u2_every_argument_list_is_accepted_by_the_command_line() {
        let program = std::env::var("CLIFF_VEIL_CLI").expect("CLIFF_VEIL_CLI");
        let run = |request: &Request| {
            let out = Command::new(&program)
                .args(argv(request))
                .env("CV_IDS", "/nonexistent/ids.toml")
                .env_remove("CV_VESTING")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        };
        let mut create = validate(&linear_form()).unwrap();
        create.cancelable = false;
        create.transferable = true;
        create.cancel_authority = Some(id(7));
        create.milestone_authority = Some(id(8));
        create.refund_to = Some(id(9));
        create.asset = Asset::Token { definition: id(6) };
        let mut by_id = create.clone();
        by_id.label = None;
        by_id.id = Some("ab".repeat(32));
        by_id.kind = Kind::Milestones {
            amounts: vec![300, 700],
            signalled: 0,
        };
        create.label = Some("team".into());
        let private = ClaimRequest {
            path: ClaimPath::Private {
                account: Some(id(8)),
                fee_payer: Some(id(9)),
            },
            ..private_request()
        };
        let mut requests = vec![
            Request::Status { schedule: id(1) },
            Request::Cancel {
                schedule: id(1),
                authority: id(2),
                mode: Mode::DryRun,
            },
            Request::Signal {
                schedule: id(1),
                authority: id(2),
                index: 3,
                mode: Mode::Send,
            },
        ];
        for mode in [Mode::DryRun, Mode::Send] {
            requests.push(Request::Claim {
                request: public_request(),
                mode,
            });
            requests.push(Request::Claim {
                request: private.clone(),
                mode,
            });
            requests.push(Request::Claim {
                request: private_request(),
                mode,
            });
            requests.push(Request::Create {
                request: create.clone(),
                mode,
            });
            requests.push(Request::Create {
                request: by_id.clone(),
                mode,
            });
        }
        for request in &requests {
            let (stdout, stderr) = run(request);
            let doc: Value = serde_json::from_str(stdout.trim())
                .unwrap_or_else(|_| panic!("{:?} was not accepted: {stderr}", argv(request)));
            let e = crate::cli::error(&doc).unwrap_or_else(|| panic!("stopped at the missing ids file: {doc}"));
            assert!(e.message.contains("program ids"), "{doc}");
        }
        let (stdout, _) = run(&Request::Disclosure);
        let doc: Value = serde_json::from_str(stdout.trim()).unwrap();
        crate::disclosure::check(&crate::cli::disclosure(&doc).unwrap()).unwrap();
        // Not there yet: listing by account (the command line reports a usage error).
        let (stdout, stderr) = run(&Request::List {
            role: Role::Beneficiary,
            account: id(3),
        });
        let refused = read_output(false, &stdout, &stderr).unwrap_err();
        assert!(refused.message.contains("unrecognized subcommand"), "{refused:?}");
    }
}
