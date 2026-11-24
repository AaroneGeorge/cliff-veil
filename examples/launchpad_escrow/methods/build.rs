//! Builds the `launchpad_escrow` example guest with the local rzup toolchain (`RISC0_SKIP_BUILD=1`: no build).
//!
//! Build input: `CV_VESTING_PROGRAM_ID` (base58), the `vesting` program the launches use; unset, the local test id
//! the executor tests deploy `vesting` at. A change rebuilds the guest.

fn main() {
    println!("cargo:rerun-if-env-changed=CV_VESTING_PROGRAM_ID");
    risc0_build::embed_methods();
}
