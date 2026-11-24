//! Entry point of the `vesting_fees` program. The handler and its rules are in the `vesting_fees` crate, so the host
//! tests run the same code.

lee_core::actor_entrypoint!(vesting_fees::handle_message);
