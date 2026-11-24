//! Embedded guest ELFs and image ids: `VESTING_ELF`, `VESTING_ID`, `VESTING_PATH` for the `vesting` program and
//! `VESTING_FEES_ELF`, `VESTING_FEES_ID`, `VESTING_FEES_PATH` for `vesting_fees`.
//! `risc0-build` writes them at build time (empty with `RISC0_SKIP_BUILD=1`).

include!(concat!(env!("OUT_DIR"), "/methods.rs"));
