//! `cliff_veil_module`: the Logos core module behind the cliff-veil mini-app.
//!
//! The QML view calls the methods of [`glue::CliffVeilModule`]; each one hands its arguments to
//! [`cliff_veil_miniapp::app::App`], which holds the claim and operation flows and reaches the SDK through the
//! `cliff-veil` command line. The SDK (LEZ wallet, RISC Zero host) runs in that separate process, not in the plugin.

#[cfg(feature = "logos")]
mod glue;
