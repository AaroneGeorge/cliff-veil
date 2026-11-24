//! Embedded ELF and image id of the `launchpad_escrow` example: `LAUNCHPAD_ESCROW_ELF`, `LAUNCHPAD_ESCROW_ID`,
//! `LAUNCHPAD_ESCROW_PATH`. `risc0-build` writes them at build time (empty with `RISC0_SKIP_BUILD=1`).

include!(concat!(env!("OUT_DIR"), "/methods.rs"));
