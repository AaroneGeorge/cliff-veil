//! The SDK (`cliff_veil_sdk`) against the test's sequencer, for the tests of the SDK's own calls (U1, Pr1, Pr3, F2
//! private claim): a client over a second wallet in the test's directory that holds every party's key.

use anyhow::Result;
use cliff_veil_sdk::{Client, Programs, SequencerBackend};

use crate::{Ctx, Ids, open_wallet};

impl Ctx {
    /// An SDK client over a wallet of its own holding the keys of every party of this context, with the programs of
    /// the ids file ([`Ids::path`]). Each call opens a new wallet (and a new set of private accounts).
    ///
    /// # Errors
    /// The ids file has no `vesting`, or the wallet does not open.
    pub fn sdk(&mut self) -> Result<Client<SequencerBackend>> {
        self.counter += 1;
        let dir = self.dir.join(format!("sdk-{}", self.counter));
        let mut wallet = open_wallet(&self.rpc, &dir)?;
        for (_, key) in &self.parties {
            wallet
                .storage_mut()
                .key_chain_mut()
                .add_imported_public_account(key.clone());
        }
        let programs = Programs::from_ids_file(&Ids::path())?;
        Ok(Client::new(SequencerBackend::new(wallet, programs)))
    }
}
