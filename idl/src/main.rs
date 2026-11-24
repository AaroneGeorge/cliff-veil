//! Writes `idl/vesting.json` and `idl/vesting_fees.json` from `core/` with SPEL's generator.
//!
//! ```text
//! cargo run -p vesting_idl              write both files
//! cargo run -p vesting_idl -- --check   fail if a checked-in file differs from a fresh generation
//! ```

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let check = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("--check") => true,
        Some(other) => {
            eprintln!("unknown argument '{other}'; usage: vesting_idl [--check]");
            return ExitCode::FAILURE;
        }
    };
    let files = match vesting_idl::generate_all(&vesting_idl::core_dir()) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stale = false;
    for (name, json) in files {
        let path = dir.join(name);
        if check {
            if std::fs::read_to_string(&path).ok().as_deref() == Some(json.as_str()) {
                println!("idl/{name}: up to date");
            } else {
                eprintln!("idl/{name}: stale; run `cargo run -p vesting_idl`");
                stale = true;
            }
        } else if let Err(e) = std::fs::write(&path, json) {
            eprintln!("error: cannot write idl/{name}: {e}");
            return ExitCode::FAILURE;
        } else {
            println!("wrote idl/{name}");
        }
    }
    if stale { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
