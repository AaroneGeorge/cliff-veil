//! Entry point of the `launchpad_escrow` example program. The handler is in the `launchpad_escrow` crate, so host
//! tests run the same code. Raw entrypoint: messages are decoded by sender (seat or launch actor).

lee_core::actor_entrypoint!(raw launchpad_escrow::handle);
