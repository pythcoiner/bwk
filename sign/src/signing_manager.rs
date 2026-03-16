use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    str::FromStr,
};

use crossbeam::channel;

use miniscript::{
    bitcoin::{self, bip32},
    Descriptor, DescriptorPublicKey, ForEachKey,
};

use crate::{
    hot_signer::{HotSigner, JsonSigner},
    signer::{Signer, SignerNotif},
};

#[derive(Debug, Clone)]
pub enum Error {
    ParsePsbt,
    #[cfg(feature = "hwi")]
    Hw(String),
}

pub enum SignerKind {
    Hot,
    #[cfg(feature = "hwi")]
    External(bwk_hwi::DeviceKind),
}

#[cfg(feature = "hwi")]
impl Clone for SignerKind {
    fn clone(&self) -> Self {
        match self {
            SignerKind::Hot => SignerKind::Hot,
            SignerKind::External(k) => SignerKind::External(*k),
        }
    }
}

#[allow(clippy::mutable_key_type)]
struct ExternalSigner {
    signer: Box<dyn Signer>,
    #[cfg(feature = "hwi")]
    kind: SignerKind,
    id: String,
    descriptors: BTreeSet<Descriptor<DescriptorPublicKey>>,
}

/// A manager for handling hot signers and their notifications.
pub struct SigningManager {
    data_dir: PathBuf,
    dir_name: &'static str,
    receiver: channel::Receiver<SignerNotif>,
    sender: channel::Sender<SignerNotif>,
    bip32_signers: BTreeMap<bip32::Fingerprint, HotSigner>,
    signers: BTreeMap<bip32::Fingerprint, ExternalSigner>,
    persist: bool,
    #[cfg(feature = "hwi")]
    hw_service: Option<bwk_hwi::service::HwiService<crate::hwi::HwMessage>>,
    #[cfg(feature = "hwi")]
    hw_receiver: Option<channel::Receiver<crate::hwi::HwMessage>>,
}

impl std::fmt::Debug for SigningManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningManager")
            .field("data_dir", &self.data_dir)
            .field("dir_name", &self.dir_name)
            .field("bip32_signers", &self.bip32_signers)
            .field("signers_count", &self.signers.len())
            .field("persist", &self.persist)
            .finish()
    }
}

impl SigningManager {
    pub fn new(data_dir: PathBuf, dir_name: &'static str) -> Self {
        let (sender, receiver) = channel::unbounded();
        Self {
            data_dir,
            dir_name,
            receiver,
            sender,
            bip32_signers: Default::default(),
            signers: Default::default(),
            persist: true,
            #[cfg(feature = "hwi")]
            hw_service: None,
            #[cfg(feature = "hwi")]
            hw_receiver: None,
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
                let bip32_signers = signers
                    .into_iter()
                    .map(|s| {
                        let signer = HotSigner::from_json(s);
                        (signer.fingerprint(), signer)
                    })
                    .collect();
                let mut manager = SigningManager::new(data_dir, dir_name);
                manager.bip32_signers = bip32_signers;
                let sender = manager.sender.clone();
                for signer in manager.bip32_signers.values_mut() {
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
                    .bip32_signers
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
        if let Ok(notif) = self.receiver.try_recv() {
            return Some(notif);
        }
        #[cfg(feature = "hwi")]
        if let Some(ref hw_rx) = self.hw_receiver {
            if let Ok(hw_msg) = hw_rx.try_recv() {
                return self.convert_hw_message(hw_msg);
            }
        }
        None
    }

    #[cfg(feature = "hwi")]
    fn convert_hw_message(&self, msg: crate::hwi::HwMessage) -> Option<SignerNotif> {
        use bwk_hwi::service::SigningDeviceMsg;
        use bwk_keys::OXpub;
        match msg {
            crate::hwi::HwMessage::Device(device_msg) => match device_msg {
                SigningDeviceMsg::Update => Some(SignerNotif::DeviceUpdate),
                SigningDeviceMsg::TransactionSigned(_, fg, psbt) => {
                    Some(SignerNotif::Signed(fg, psbt))
                }
                SigningDeviceMsg::XPub(_, fg, path, xpub) => {
                    let oxpub = OXpub {
                        origin: (fg, path),
                        xkey: xpub,
                    };
                    Some(SignerNotif::Xpub(fg, oxpub))
                }
                SigningDeviceMsg::Error(_, msg) => Some(SignerNotif::Manager(Error::Hw(msg))),
                _ => None,
            },
        }
    }

    /// Creates a new hot signer with a generated mnemonic.
    ///
    /// # Parameters
    /// - `network`: The network for which the hot signer is created.
    pub fn new_bip32_signer(&mut self, network: bitcoin::Network) {
        let mnemomic = bip39::Mnemonic::generate(12).unwrap();
        self.new_bip32_signer_from_mnemonic(network, mnemomic.to_string());
    }

    /// Creates a new hot signer from a given mnemonic.
    ///
    /// # Parameters
    /// - `network`: The network for which the hot signer is created.
    /// - `mnemonic`: The mnemonic used to create the hot signer.
    pub fn new_bip32_signer_from_mnemonic(&mut self, network: bitcoin::Network, mnemonic: String) {
        let mut signer = HotSigner::new_from_mnemonics(network, &mnemonic).unwrap();
        signer.init(self.sender.clone());
        self.bip32_signers.insert(signer.fingerprint(), signer);
    }

    pub fn register_bip32_descriptor(&mut self, descriptor: Descriptor<DescriptorPublicKey>) {
        for signer in self.bip32_signers.values_mut() {
            signer.inner_register_descriptor(descriptor.clone());
        }
    }

    pub fn sign(&self, psbt: String) {
        let mut psbt = match bitcoin::Psbt::from_str(&psbt) {
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

        self.sign_psbt(&mut psbt);

        let fg = self
            .bip32_signers
            .keys()
            .next()
            .copied()
            .unwrap_or_default();
        if self.sender.send(SignerNotif::Signed(fg, psbt)).is_err() {
            log::error!("SigningManager::sign() fails to send notif")
        }
    }

    pub fn sign_psbt(&self, psbt: &mut bitcoin::Psbt) {
        for signer in self.bip32_signers.values() {
            signer.sign(psbt);
        }
    }

    /// Returns master xprivs from all BIP32 hot signers, keyed by fingerprint.
    pub fn master_xprivs(&self) -> BTreeMap<bip32::Fingerprint, bip32::Xpriv> {
        self.bip32_signers
            .iter()
            .map(|(fg, signer)| (*fg, signer.master_xpriv()))
            .collect()
    }

    pub fn list_signers(&self) -> Vec<(bip32::Fingerprint, SignerKind)> {
        #[cfg_attr(not(feature = "hwi"), allow(unused_mut))]
        let mut result: Vec<_> = self
            .bip32_signers
            .keys()
            .map(|fg| (*fg, SignerKind::Hot))
            .collect();
        #[cfg(feature = "hwi")]
        for (fg, ext) in &self.signers {
            result.push((*fg, ext.kind.clone()));
        }
        result
    }

    pub fn sign_with(
        &self,
        fingerprint: bip32::Fingerprint,
        id: Option<&str>,
        psbt: &mut bitcoin::Psbt,
    ) {
        // Hot signers: sign in-place synchronously
        if let Some(signer) = self.bip32_signers.get(&fingerprint) {
            signer.sign(psbt);
            return;
        }
        // External signers: fire-and-forget, result via poll()
        if let Some(ext) = self.signers.get(&fingerprint) {
            if let Some(req_id) = id {
                if ext.id != req_id {
                    return;
                }
            }
            // Find first registered descriptor and trigger async signing
            if let Some(descriptor) = psbt_matching_descriptor(psbt, fingerprint, &ext.descriptors)
            {
                ext.signer.sign_with_descriptor(psbt.clone(), descriptor);
            } else {
                log::warn!("sign_with: no matching descriptor found for fingerprint {fingerprint}");
            }
        }
    }

    #[cfg(feature = "hwi")]
    pub fn start_hw_service(&mut self, network: bitcoin::Network) {
        let (hw_sender, hw_receiver) = channel::unbounded();
        let service = bwk_hwi::service::HwiService::new(network);
        service.start(hw_sender);
        self.hw_service = Some(service);
        self.hw_receiver = Some(hw_receiver);
    }

    #[cfg(feature = "hwi")]
    pub fn stop_hw_service(&mut self) {
        if let Some(service) = self.hw_service.as_ref() {
            service.stop();
        }
        self.hw_service = None;
        self.hw_receiver = None;
    }

    #[cfg(feature = "hwi")]
    pub fn hw_devices(
        &self,
    ) -> BTreeMap<String, bwk_hwi::service::SigningDevice<crate::hwi::HwMessage>> {
        if let Some(ref service) = self.hw_service {
            service.list()
        } else {
            BTreeMap::new()
        }
    }

    #[cfg(feature = "hwi")]
    pub fn add_hw_signer(&mut self, device_id: &str) -> Option<bip32::Fingerprint> {
        let service = self.hw_service.as_ref()?;
        let devices = service.list();
        let device = devices.get(device_id)?;
        if let bwk_hwi::service::SigningDevice::Supported(supported) = device {
            let fg = *supported.fingerprint();
            let kind = *supported.kind();
            let mut signer = crate::hwi::HwSigner::new(supported.clone(), device_id.to_string());
            signer.init(self.sender.clone());
            let ext = ExternalSigner {
                signer: Box::new(signer),
                kind: SignerKind::External(kind),
                id: device_id.to_string(),
                descriptors: BTreeSet::new(),
            };
            self.signers.insert(fg, ext);
            Some(fg)
        } else {
            None
        }
    }

    #[cfg(feature = "hwi")]
    pub fn remove_hw_signer(&mut self, fingerprint: &bip32::Fingerprint) {
        self.signers.remove(fingerprint);
    }

    /// Register a descriptor for a hardware signer identified by fingerprint.
    ///
    /// This stores the descriptor in the ExternalSigner for use during signing,
    /// and delegates to the underlying signer (which calls device.register_wallet()).
    #[cfg(feature = "hwi")]
    pub fn register_hw_descriptor(
        &mut self,
        fingerprint: &bip32::Fingerprint,
        descriptor: Descriptor<DescriptorPublicKey>,
    ) {
        if let Some(ext) = self.signers.get_mut(fingerprint) {
            ext.descriptors.insert(descriptor.clone());
            ext.signer.register_descriptor(descriptor);
        }
    }
}

/// Find a registered descriptor that references the given fingerprint, confirmed by the PSBT.
/// Iterates the signer's registered descriptors and returns the first one whose key origins
/// include the given fingerprint.
#[allow(clippy::mutable_key_type)]
fn psbt_matching_descriptor(
    psbt: &bitcoin::Psbt,
    fingerprint: bip32::Fingerprint,
    descriptors: &BTreeSet<Descriptor<DescriptorPublicKey>>,
) -> Option<Descriptor<DescriptorPublicKey>> {
    // Collect all fingerprints referenced in this PSBT for quick lookup
    let mut psbt_fingerprints = BTreeSet::new();
    for input in &psbt.inputs {
        for (fg, _) in input.bip32_derivation.values() {
            psbt_fingerprints.insert(*fg);
        }
        for (_, (fg, _)) in input.tap_key_origins.values() {
            psbt_fingerprints.insert(*fg);
        }
    }

    if !psbt_fingerprints.contains(&fingerprint) {
        return None;
    }

    // Find the first registered descriptor that references this fingerprint
    for descriptor in descriptors {
        let matches = descriptor.for_any_key(|k| match k {
            DescriptorPublicKey::XPub(key) => key
                .origin
                .as_ref()
                .is_some_and(|(fg, _)| *fg == fingerprint),
            DescriptorPublicKey::MultiXPub(key) => key
                .origin
                .as_ref()
                .is_some_and(|(fg, _)| *fg == fingerprint),
            DescriptorPublicKey::Single(_) => false,
        });
        if matches {
            return Some(descriptor.clone());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use bip32::Fingerprint;

    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_manager_bip32_signer() {
        let mut manager = SigningManager::new(PathBuf::new(), ".bwk");
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".to_string();
        manager.new_bip32_signer_from_mnemonic(bitcoin::Network::Regtest, mnemonic);
        if let SignerNotif::Info(fg, _info) = manager.poll().unwrap() {
            assert_eq!(fg, Fingerprint::from_str("73c5da0a").unwrap());
        } else {
            panic!("expect info");
        }
    }
}
