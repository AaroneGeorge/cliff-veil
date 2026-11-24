//! Builds the guests listed in `[package.metadata.risc0] methods`.
//!
//! - Default: local rzup toolchain (`rust` 1.91.1), fast, for development.
//! - `CV_GUEST_DOCKER=1`: the pinned image `risczero/risc0-guest-builder:r0.1.91.1` (same tag as LEZ v0.4.0-rc3).
//!   The release set whose image ids are checked in CI is built by `scripts/guests_release.sh`.
//! - `RISC0_SKIP_BUILD=1`: no guest build (fmt, clippy, host-only tests).
//!
//! Build inputs of `vesting` (SPEC 1.1): `CV_FEES_PROGRAM_ID`, `CV_TOKEN_PROGRAM_ID`, `CV_ATA_PROGRAM_ID` (base58).
//! Unset, the guest uses its local test ids. They reach the local build through the environment and the docker build
//! through the options below; a change rebuilds the guests.

use std::collections::HashMap;

const GUESTS: [&str; 3] = ["vesting_program", "vesting_fees_program", "token_guest"];
const BUILD_INPUTS: [&str; 3] = ["CV_FEES_PROGRAM_ID", "CV_TOKEN_PROGRAM_ID", "CV_ATA_PROGRAM_ID"];
const DOCKER_TAG: &str = "r0.1.91.1";

fn main() {
    println!("cargo:rerun-if-env-changed=CV_GUEST_DOCKER");
    for name in BUILD_INPUTS {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let mut opts = HashMap::new();
    if std::env::var("CV_GUEST_DOCKER").is_ok_and(|v| v == "1") {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let inputs: Vec<(String, String)> = BUILD_INPUTS
            .iter()
            .filter_map(|name| std::env::var(name).ok().map(|value| ((*name).to_owned(), value)))
            .collect();
        let docker = risc0_build::DockerOptionsBuilder::default()
            .root_dir(root.canonicalize().expect("workspace root"))
            .docker_container_tag(DOCKER_TAG)
            .env(inputs)
            .build()
            .expect("docker options");
        for g in GUESTS {
            let o = risc0_build::GuestOptionsBuilder::default()
                .use_docker(docker.clone())
                .build()
                .expect("guest options");
            opts.insert(g, o);
        }
    }
    risc0_build::embed_methods_with_options(opts);
}
