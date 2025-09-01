use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    str::FromStr,
    sync::mpsc::{self},
};

// use joinstr::{
//     bip39::{self},
//     miniscript::bitcoin::{
//         bip32::{self, DerivationPath},
//         Psbt,
//     },
// };

use miniscript::bitcoin::{self, bip32};

use crate::{
    descriptor::wpkh,
    signer::{HotSigner, JsonSigner, Signer, SignerNotif},
};

#[derive(Debug, Clone)]
pub enum Error {
    ParsePsbt,
}

/// A manager for handling hot signers and their notifications.
#[derive(Debug)]
pub struct SigningManager {
    data_dir: PathBuf,
    dir_name: &'static str,
    receiver: mpsc::Receiver<SignerNotif>,
    sender: mpsc::Sender<SignerNotif>,
    hot_signers: BTreeMap<bip32::Fingerprint, HotSigner>,
    #[allow(unused)]
    signers: BTreeMap<bip32::Fingerprint, ()>,
    persist: bool,
}

impl SigningManager {
    pub fn new(data_dir: PathBuf, dir_name: &'static str) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            data_dir,
            dir_name,
            receiver,
            sender,
            hot_signers: Default::default(),
            signers: Default::default(),
            persist: true,
        }
    }
    /// Returns the path to the signers' data directory.
    pub fn path(data_dir: PathBuf, dir_name: &'static str) -> PathBuf {
        let mut path = data_dir;
        path.push(dir_name);
        path.push(".signers");
        path
    }
    /// Creates a `SigningManager` instance from a file.
    pub fn from_file(data_dir: PathBuf, dir_name: &'static str) -> Self {
        if let Ok(mut file) = File::open(Self::path(data_dir.clone(), dir_name)) {
            let mut content = String::new();
            let _ = file.read_to_string(&mut content);
            let json_signers: Result<Vec<JsonSigner>, _> = serde_json::from_str(&content);
            if let Ok(signers) = json_signers {
                let hot_signers = signers
                    .into_iter()
                    .map(|s| {
                        let signer = HotSigner::from_json(s);
                        (signer.fingerprint(), signer)
                    })
                    .collect();
                let mut manager = SigningManager::new(data_dir, dir_name);
                manager.hot_signers = hot_signers;
                let sender = manager.sender.clone();
                for signer in manager.hot_signers.values_mut() {
                    signer.init(sender.clone());
                }
                manager
            } else {
                SigningManager::new(data_dir, dir_name)
            }
        } else {
            SigningManager::new(data_dir, dir_name)
        }
    }

    /// Allow to disable persistance of data, useful for tests
    pub fn enable_persist(mut self, persist: bool) -> Self {
        self.persist = persist;
        self
    }

    /// Persists the current state of the signers to a file.
    pub fn persist(&self) {
        if !self.persist {
            return;
        }
        match File::create(Self::path(self.data_dir.clone(), self.dir_name)) {
            Ok(mut file) => {
                let content: Vec<_> = self
                    .hot_signers
                    .clone()
                    .into_values()
                    .map(|s| s.to_json())
                    .collect();
                let str_content = serde_json::to_string_pretty(&content).expect("cannot_fail");
                let _ = file.write(str_content.as_bytes());
            }
            Err(e) => {
                log::error!("SigningManager::persist() fail to open file: {e}");
            }
        }
    }

    /// Polls for a new signer notification.
    ///
    /// # Returns
    /// An `Option<SignerNotif>` which is `Some` if a notification is available,
    /// or `None` if there are no new notifications.
    pub fn poll(&self) -> Option<SignerNotif> {
        self.receiver.try_recv().ok()
    }

    /// Creates a new hot signer with a generated mnemonic.
    ///
    /// # Parameters
    /// - `network`: The network for which the hot signer is created.
    pub fn new_hot_signer(&mut self, network: bitcoin::Network) {
        let mnemomic = bip39::Mnemonic::generate(12).unwrap();
        self.new_hot_signer_from_mnemonic(network, mnemomic.to_string());
    }

    /// Creates a new hot signer from a given mnemonic.
    ///
    /// # Parameters
    /// - `network`: The network for which the hot signer is created.
    /// - `mnemonic`: The mnemonic used to create the hot signer.
    pub fn new_hot_signer_from_mnemonic(&mut self, network: bitcoin::Network, mnemonic: String) {
        let mut signer = HotSigner::new_from_mnemonics(network, &mnemonic).unwrap();
        signer.init(self.sender.clone());
        self.hot_signers.insert(signer.fingerprint(), signer);
    }

    pub fn sign(&self, network: bitcoin::Network, psbt: String) {
        let psbt = match bitcoin::Psbt::from_str(&psbt) {
            Ok(p) => p,
            Err(_) => {
                if self
                    .sender
                    .send(SignerNotif::Manager(Error::ParsePsbt))
                    .is_err()
                {
                    log::error!("SigningManager::sign() fails to send notif")
                }
                return;
            }
        };

        let signer = self
            .hot_signers
            .iter()
            .next()
            .expect("at least one signer")
            .1;

        let n_path = match network {
            bitcoin::Network::Bitcoin => 0,
            _ => 1,
        };
        let deriv_path = bip32::DerivationPath::from_str(&format!("m/84'/{}'/0'", n_path)).unwrap();
        let xpub = signer.xpub(&deriv_path);
        let descriptor = wpkh(xpub);

        signer.sign(psbt, descriptor);
    }
}

#[cfg(test)]
mod tests {
    use bip32::Fingerprint;

    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_manager_hot_signer() {
        let mut manager = SigningManager::new(PathBuf::new(), ".bwk");
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".to_string();
        manager.new_hot_signer_from_mnemonic(bitcoin::Network::Regtest, mnemonic);
        if let SignerNotif::Info(fg, _info) = manager.poll().unwrap() {
            assert_eq!(fg, Fingerprint::from_str("73c5da0a").unwrap());
        } else {
            panic!("expect info");
        }
    }
}
