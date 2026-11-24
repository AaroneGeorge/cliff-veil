//! Entry point of the `vesting` program. The handler is in the `vesting_program` library, so host tests run the same
//! code. Raw entrypoint: the program decodes each message by its sender, the clock's reply included (SPEC 1.4).

lee_core::actor_entrypoint!(raw vesting_program::handle);
