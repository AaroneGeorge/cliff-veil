//! The token program of `logos-blockchain/lez-programs` (pinned commit), unchanged: the same entry point as its own
//! guest. Built here so the executor tests and a local network run the token program `vesting` is built against.

lee_core::actor_entrypoint!(raw token_program::handle_message);
