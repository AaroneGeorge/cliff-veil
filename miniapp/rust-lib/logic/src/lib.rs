//! The cliff-veil Basecamp mini-app, minus the toolkit.
//!
//! Everything the screens show and every rule they enforce lives here, in plain Rust:
//! - [`views`]: the recipient view (position, total locked, claimable now, next unlock) and the creator view
//!   (active schedules, which of cancel and signal the caller may do);
//! - [`form`]: the create form with every schedule parameter, checked with the program's error codes;
//! - [`claim`]: the pre-claim summary (claimable amount, estimated fee, gas check) and the claim flow, which does not
//!   send a claim to a private account until the privacy disclosure shown for that claim is acknowledged;
//! - [`disclosure`]: what the disclosure must name (checked on the text the SDK supplies);
//! - [`backend`]: how the module reaches the SDK (the `cliff-veil` command line, `--json`), and [`cli`]: reading
//!   what it prints;
//! - [`app`]: the JSON API the QML view calls through the core module.
//!
//! The core module (`miniapp/rust-lib`) is a thin layer that hands [`app::App`] to the Logos module runtime.

pub mod account;
pub mod amount;
pub mod app;
pub mod backend;
pub mod claim;
pub mod cli;
pub mod disclosure;
pub mod form;
pub mod model;
pub mod time;
pub mod views;
