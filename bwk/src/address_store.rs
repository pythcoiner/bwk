use bwk_descriptor::derivator::SpkDerivator;
use bwk_tx::{coin::KeyChain, tx_builder::ChangeTip};
use miniscript::bitcoin::{self, address::NetworkUnchecked, Script, ScriptBuf};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{mpsc, Arc, Mutex},
};

use crate::{account::Notification, config::Config};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
pub enum AddressStatus {
    NotUsed,
    Used,
    Reused,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
/// Represents the current tip of address generation for receiving and change.
///
/// # Fields
/// - `recv`: Last generated receiving address index.
/// - `change`: Last generated change address index.
pub struct AddressTip {
    pub recv: u32,
    pub change: u32,
}

#[derive(Clone)]
pub struct ChangeTipUpdater(Arc<Mutex<AddressStore>>);

impl ChangeTipUpdater {
    pub fn new(store: Arc<Mutex<AddressStore>>) -> Self {
        ChangeTipUpdater(store)
    }
}

impl ChangeTip for ChangeTipUpdater {
    fn next_index(&mut self) -> u32 {
        let mut store = self.0.lock().expect("poisoned");
        store.change_generated_tip += 1;
        let tip = store.change_generated_tip;
        store.update_change(tip);
        tip
    }
}

/// Manages storage and generation of Bitcoin addresses.
///
/// Tracks generated receiving and change addresses, notifies listeners,
/// and populates addresses as needed.
///
/// # Fields
/// - `store`: Map of script public keys to address entries.
/// - `recv_generated_tip`: Last generated receiving address index.
/// - `change_generated_tip`: Last generated change address index.
/// - `signer`: Signer used to generate new addresses.
/// - `notification`: Channel for sending notifications about address
///   tips changes.
/// - `tx_listener`: Optional channel for sending address tip changes.
/// - `look_ahead`: Number of addresses to generate ahead of the current tip.
#[derive(Debug)]
pub struct AddressStore {
    store: BTreeMap<ScriptBuf, AddressEntry>,
    recv_generated_tip: u32,
    change_generated_tip: u32,
    derivator: SpkDerivator,
    notification: mpsc::Sender<Notification>,
    tx_listener: Option<mpsc::Sender<AddressTip>>,
    look_ahead: u32,
    config: Config,
}

impl AddressStore {
    /// Creates a new `AddressStore`.
    ///
    /// # Parameters
    /// - `signer`: The signer used to generate new addresses.
    /// - `notification`: A channel for sending notifications about address
    ///   tip changes.
    /// - `recv_tip`: The initial index for receiving address generation.
    /// - `change_tip`: The initial index for change address generation.
    /// - `look_ahead`: The number of addresses to generate ahead of the
    ///   current tip.
    ///
    /// # Returns
    /// A new instance of `AddressStore`.
    pub fn new(
        derivator: SpkDerivator,
        notification: mpsc::Sender<Notification>,
        recv_tip: u32,
        change_tip: u32,
        look_ahead: u32,
        config: Config,
    ) -> Self {
        let mut store = Self {
            derivator,
            store: BTreeMap::new(),
            recv_generated_tip: recv_tip,
            change_generated_tip: change_tip,
            notification,
            tx_listener: None,
            look_ahead,
            config,
        };
        store.populate_maybe();
        store.update_watch_tip();

        store
    }

    /// Notifies [`Account`] owner of address tip changes.
    ///
    /// This method sends a notification to the channel and updates the watch tip.
    fn notify(&self) {
        if let Err(e) = self.notification.send(Notification::AddressTipChanged) {
            log::error!("AddressStore::notify() fail to send notification: {e:?}");
        }
        self.update_watch_tip();
    }

    /// Updates the watch tip for receiving and change addresses.
    ///
    /// This method sends the current address tips to the transaction listener.
    fn update_watch_tip(&self) {
        if let Some(tx_listener) = &self.tx_listener {
            let recv = self.recv_watch_tip();
            let change = self.change_watch_tip();
            // NOTE: tx_listener thread must send notification itself if
            // fail to connect to electrum
            let _ = tx_listener.send(AddressTip { recv, change });
        }
        self.config
            .persist_tip(self.recv_generated_tip, self.change_generated_tip);
    }

    /// Processes a received coin at the specified script public key.
    ///
    /// # Parameters
    /// - `spk`: The script public key of the received coin.
    ///
    /// # Panics
    /// This function panics if the script public key is not found in the store.
    pub fn recv_coin_at(&mut self, spk: &ScriptBuf) {
        let AddressEntry { account, index, .. } = self.store.get(spk).expect("must be there");
        // AddrAccount::Receive => self.update_recv(*index),
        // AddrAccount::Change => self.update_change(*index),
        match *account {
            KeyChain::Receive => self.update_recv(*index),
            KeyChain::Change => self.update_change(*index),
            KeyChain::Custom(_) => unimplemented!(),
        }
    }

    /// Populates the address store with addresses up to the current watch tips.
    ///
    /// This method generates receiving and change addresses and adds them if not present.
    pub fn populate_maybe(&mut self) {
        for i in 0..self.recv_watch_tip() + 1 {
            let addr = self.derivator.receive_at(i);
            let script = addr.script_pubkey();
            self.store.entry(script).or_insert_with(|| {
                let address = addr.as_unchecked().clone();
                AddressEntry {
                    status: AddressStatus::NotUsed,
                    address,
                    account: KeyChain::Receive,
                    index: i,
                }
            });
        }
        for i in 0..self.change_watch_tip() + 1 {
            let addr = self.derivator.change_at(i);
            let script = addr.script_pubkey();
            self.store.entry(script).or_insert_with(|| {
                let address = addr.as_unchecked().clone();
                AddressEntry {
                    status: AddressStatus::NotUsed,
                    address,
                    account: KeyChain::Change,
                    index: i,
                }
            });
        }
    }

    /// Updates the receiving address tip and populates addresses if necessary.
    ///
    /// # Parameters
    /// - `index`: The new index for the receiving address.
    pub fn update_recv(&mut self, index: u32) {
        if index > self.recv_generated_tip {
            self.recv_generated_tip = index;
        }
        self.populate_maybe();
        self.notify();
    }
    /// Updates the change address tip and populates addresses if necessary.
    ///
    /// # Parameters
    /// - `index`: The new index for the change address.
    pub fn update_change(&mut self, index: u32) {
        if index > self.change_generated_tip {
            self.change_generated_tip = index;
        }
        self.populate_maybe();
        self.notify();
    }

    /// Generates a new receiving address and updates the receiving address tip.
    ///
    /// # Returns
    /// The newly generated receiving address.
    pub fn new_recv_addr(&mut self) -> bitcoin::Address {
        self.recv_generated_tip += 1;
        self.update_recv(self.recv_generated_tip);
        self.derivator.receive_at(self.recv_generated_tip)
    }

    /// Generates a new change address and updates the change address tip.
    ///
    /// # Returns
    /// The newly generated change address.
    pub fn new_change_addr(&mut self) -> bitcoin::Address {
        self.change_generated_tip += 1;
        self.update_change(self.change_generated_tip);
        self.derivator.change_at(self.change_generated_tip)
    }

    /// Returns the current change watch tip index.
    ///
    /// The watch tip is the index of the last generated change address plus
    /// the look-ahead.
    ///
    /// # Returns
    /// The current change watch tip index.
    pub fn change_watch_tip(&self) -> u32 {
        self.change_generated_tip + self.look_ahead + 1
    }

    /// Returns the current receiving watch tip index.
    ///
    /// The watch tip is the index of the last generated receiving address
    /// plus the look-ahead.
    ///
    /// # Returns
    /// The current receiving watch tip index.
    pub fn recv_watch_tip(&self) -> u32 {
        self.recv_generated_tip + self.look_ahead + 1
    }

    /// Returns the current receiving address tip index.
    ///
    /// # Returns
    /// The current receiving address tip index.
    pub fn recv_tip(&self) -> u32 {
        self.recv_generated_tip
    }

    /// Initializes the address store with a transaction poller.
    ///
    /// This method populates the address store and sets the transaction
    /// poller channel.
    ///
    /// # Parameters
    /// - `tx_listener`: The channel for sending address tips to the poller.
    pub fn init(&mut self, tx_listener: mpsc::Sender<AddressTip>) {
        self.populate_maybe();
        self.tx_listener = Some(tx_listener);
        self.notify();
    }

    /// Retrieves an address entry by its script public key.
    ///
    /// # Parameters
    /// - `spk`: The script public key of the address entry.
    ///
    /// # Returns
    /// An `Option<AddressEntry>` containing the address entry if found,
    /// or `None` if not.
    pub fn get_entry(&self, spk: &Script) -> Option<AddressEntry> {
        self.store.get(spk).cloned()
    }

    /// Retrieves a mutable reference to an address entry by its script
    /// public key.
    ///
    /// # Parameters
    /// - `spk`: The script public key of the address entry.
    ///
    /// # Returns
    /// An `Option<&mut AddressEntry>` containing a mutable reference to
    /// the address entry if found, or `None` if not.
    pub fn get_entry_mut(&mut self, spk: &Script) -> Option<&mut AddressEntry> {
        self.store.get_mut(spk)
    }

    /// Retrieves all unused receiving addresses.
    ///
    /// This method filters the address store for addresses that are not
    /// used and belong to the receiving account.
    ///
    /// # Returns
    /// An `Addresses` object containing all unused receiving addresses.
    pub fn get_unused(&self) -> Vec<AddressEntry> {
        let mut addrs = self
            .store
            .clone()
            .into_iter()
            .filter_map(|(_, entry)| {
                if entry.status == AddressStatus::NotUsed && entry.account == KeyChain::Receive {
                    Some(entry.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        addrs.sort_by(|a, b| {
            a.account
                .cmp(&b.account)
                .then_with(|| a.index.cmp(&b.index))
        });
        addrs.into_iter().collect()
    }

    /// Retrieves all addresses for a specific account type.
    ///
    /// # Parameters
    /// - `account`: The account type (receiving or change).
    ///
    /// # Returns
    /// An `Addresses` object containing all addresses for the specified
    /// account type.
    pub fn get(&self, account: KeyChain) -> Vec<AddressEntry> {
        let mut addrs = self
            .store
            .clone()
            .into_iter()
            .filter_map(|(_, entry)| {
                if entry.account == account {
                    Some(entry.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        addrs.sort_by(|a, b| {
            a.account
                .cmp(&b.account)
                .then_with(|| a.index.cmp(&b.index))
        });
        addrs.into_iter().collect()
    }

    /// Dumps the address store as a JSON value.
    ///
    /// # Returns
    /// A `Result` containing the serialized JSON value of the address store
    /// or an error if serialization fails.
    pub fn dump(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(&self.store)
    }

    /// Restores the address store from a JSON value.
    ///
    /// # Parameters
    /// - `value`: The JSON value to restore the address store from.
    ///
    /// # Returns
    /// A `Result` indicating success or failure of the restoration.
    pub fn restore(&mut self, value: serde_json::Value) -> Result<(), serde_json::Error> {
        self.store = serde_json::from_value(value)?;
        Ok(())
    }

    pub fn contains_spk(&self, spk: &ScriptBuf) -> bool {
        self.store.contains_key(spk)
    }

    pub fn get_generated_addresses(
        &self,
    ) -> (
        Vec<AddressEntry>, /* receive */
        Vec<AddressEntry>, /* change */
    ) {
        let mut recv = self
            .store
            .iter()
            .filter_map(|(_, entry)| {
                (entry.account == KeyChain::Receive && entry.index <= self.change_generated_tip)
                    .then_some(entry)
            })
            .cloned()
            .collect::<Vec<_>>();
        recv.sort_by(|a, b| a.index.cmp(&b.index));
        let mut change = self
            .store
            .iter()
            .filter_map(|(_, entry)| {
                (entry.account == KeyChain::Change && entry.index <= self.change_generated_tip)
                    .then_some(entry)
            })
            .cloned()
            .collect::<Vec<_>>();
        change.sort_by(|a, b| a.index.cmp(&b.index));
        (recv, change)
    }
}

/// Represents an entry in the address store.
///
/// The `AddressEntry` contains information about a specific address, including its
/// status, account type, and index.
///
/// # Fields
/// - `status`: The status of the address (used, unused, etc.).
/// - `address`: The Bitcoin address associated with this entry.
/// - `account`: The account type (receiving or change).
/// - `index`: The index of the address in the generation sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressEntry {
    pub status: AddressStatus,
    pub address: bitcoin::Address<NetworkUnchecked>,
    pub account: KeyChain,
    pub index: u32,
}

impl AddressEntry {
    /// Returns the script public key of the address.
    ///
    /// # Returns
    /// The script public key associated with this entry.
    pub fn script(&self) -> ScriptBuf {
        self.address.clone().assume_checked().script_pubkey()
    }
    /// Sets the status of the address entry.
    ///
    /// # Parameters
    /// - `status`: The new status to set for this entry.
    ///
    /// # Note
    /// The status is based on the Electrum protocol.
    /// https://electrumx.readthedocs.io/en/latest/protocol-basics.html#status
    pub fn set_status(&mut self, status: AddressStatus) {
        self.status = status;
    }
    /// Returns the status of the address entry.
    ///
    /// # Returns
    /// The current status of this entry.
    pub fn status(&self) -> AddressStatus {
        self.status
    }
    /// Returns the string representation of the address.
    ///
    /// # Returns
    /// The address as a string.
    pub fn value(&self) -> String {
        self.address.clone().assume_checked().to_string()
    }
    /// Returns the account type of the address entry.
    ///
    /// # Returns
    /// The account type (receiving or change) of this entry.
    pub fn account(&self) -> KeyChain {
        self.account
    }
    /// Returns the account type as a u32 value.
    ///
    /// # Returns
    /// `0` for receiving and `1` for change accounts.
    pub fn account_u32(&self) -> u32 {
        match self.account {
            KeyChain::Receive => 0,
            KeyChain::Change => 1,
            KeyChain::Custom(c) => c,
        }
    }
    /// Returns the index of the address entry.
    ///
    /// # Returns
    /// The derivation index of this address.
    pub fn index(&self) -> u32 {
        self.index
    }
    /// Returns the Bitcoin address associated with this entry.
    ///
    /// # Returns
    /// The Bitcoin address of this entry.
    pub fn address(&self) -> bitcoin::Address<NetworkUnchecked> {
        self.address.clone()
    }

    /// Clones the address entry and returns it as a boxed instance.
    ///
    /// # Returns
    /// A `Box<Self>` containing the cloned address entry.
    pub fn clone_boxed(&self) -> Box<Self> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bwk_descriptor::{derivator::SpkDerivator, descriptor::ScriptType, tr_path};
    use bwk_sign::HotSigner;
    use bwk_tx::{ChangeRecipientProvider, FinalizationContext, PsbtOutputInfo, RecipientProvider};
    use miniscript::bitcoin::{bip32::ChildNumber, Network};
    use miniscript::{Descriptor, DescriptorPublicKey};
    use temp_dir::TempDir;

    fn test_derivator() -> SpkDerivator {
        let nw = Network::Regtest;
        let signer = HotSigner::new(nw).unwrap();
        let path = tr_path(nw, ChildNumber::from_hardened_idx(0).unwrap()).unwrap();
        let xpub = signer.xpub(&path);
        SpkDerivator::new_tr(xpub, nw).unwrap()
    }

    fn test_config(descriptor: Descriptor<DescriptorPublicKey>) -> Config {
        let tmp = TempDir::new().expect("tempdir");
        Config::new(
            None, // no mnemonic needed with ScriptType::Descriptor
            "test".to_string(),
            Network::Regtest,
            ScriptType::Descriptor(Box::new(descriptor)),
            tmp.path().to_path_buf(),
            "test",
            false, // don't persist
        )
        .expect("valid config")
    }

    #[test]
    fn change_recipient_provider_tracks_correct_index() {
        let (tx, _rx) = mpsc::channel();
        let network = Network::Regtest;
        let derivator = test_derivator();
        let descriptor = derivator.descriptor();
        let config = test_config(descriptor.clone());

        // Create AddressStore with change_generated_tip = 5
        let store = AddressStore::new(
            derivator, tx, 0,  // recv_tip
            5,  // change_tip - start at index 5
            20, // look_ahead
            config,
        );
        let store = Arc::new(Mutex::new(store));

        let tip_updater = ChangeTipUpdater::new(store);
        let mut provider =
            ChangeRecipientProvider::new_with_updater(tip_updater, descriptor, network);

        // Create a dummy finalization context
        let ctx = FinalizationContext {
            inputs: &[],
            partial_secret: None,
            network,
        };

        // Call create_script - this should use index 6 (5 + 1)
        let _script = provider.create_script(&ctx);

        // Verify psbt_output_info returns the correct index
        let info = provider.psbt_output_info();
        match info {
            PsbtOutputInfo::Bip32 { origin, .. } => {
                assert_eq!(origin.0, KeyChain::Change);
                assert_eq!(origin.1, 6, "Expected change index 6, got {}", origin.1);
            }
            _ => panic!("Expected PsbtOutputInfo::Bip32"),
        }

        // Call create_script again - should use index 7
        let _script2 = provider.create_script(&ctx);
        let info2 = provider.psbt_output_info();
        match info2 {
            PsbtOutputInfo::Bip32 { origin, .. } => {
                assert_eq!(origin.1, 7, "Expected change index 7, got {}", origin.1);
            }
            _ => panic!("Expected PsbtOutputInfo::Bip32"),
        }
    }

    #[test]
    #[should_panic(expected = "psbt_output_info() called before create_script()")]
    fn change_recipient_provider_panics_without_create_script() {
        let (tx, _rx) = mpsc::channel();
        let network = Network::Regtest;
        let derivator = test_derivator();
        let descriptor = derivator.descriptor();
        let config = test_config(descriptor.clone());

        let store = AddressStore::new(derivator, tx, 0, 0, 20, config);
        let store = Arc::new(Mutex::new(store));

        let tip_updater = ChangeTipUpdater::new(store);
        let provider = ChangeRecipientProvider::new_with_updater(tip_updater, descriptor, network);

        // This should panic because create_script() was never called
        let _info = provider.psbt_output_info();
    }
}
