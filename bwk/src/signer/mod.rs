use std::str::FromStr;
pub mod error;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::mpsc,
};

use error::Error;
use serde::{Deserialize, Serialize};
use {
    bip39,
    miniscript::{
        bitcoin::{
            self,
            bip32::{self, DerivationPath},
            ecdsa,
            psbt::Input,
            secp256k1::{self, All, Message},
            sighash, EcdsaSighashType, NetworkKind, Psbt,
        },
        descriptor::DescriptorMultiXKey,
        Descriptor, DescriptorPublicKey, ForEachKey,
    },
};

use crate::account::AddrAccount;
use crate::{derivator::Derivator, signing_manager};

#[derive(Debug)]
pub enum SignerNotif {
    Info(bip32::Fingerprint, serde_json::Value),
    Xpub(bip32::Fingerprint, OXpub),
    Descriptor(bip32::Fingerprint, DescriptorMultiXKey<bip32::Xpub>),
    DescriptorRegistered(bip32::Fingerprint, Descriptor<DescriptorPublicKey>, bool),
    Signed(bip32::Fingerprint, Psbt),
    Error(bip32::Fingerprint, Error),
    Manager(signing_manager::Error),
}

/// This trait implement features that are available when the signer is connected.
pub trait Signer {
    /// Initialyse the signer with a new channel, in return the signer
    /// must return a [`SignerNotif::Info`] notification to the newly
    /// registered channel.
    fn init(&mut self, channel: mpsc::Sender<SignerNotif>);
    /// Request general informations from the signer.
    /// The signer must return a [`SignerNotif::Info`] notification.
    fn info(&self);
    /// Request descriptor to generate an Xpub using the given derivation
    /// path. `display` must be set to true for non standard derivation path,
    /// allow some signer to generate non-standard Xpub with user approval.
    /// The signer must return a [`SignerNotif::Xpub`] notification.
    fn get_xpub(&self, deriv: DerivationPath, display: bool);
    /// Request signer if the given descriptor is registered.
    /// The signer must return a [`SignerNotif::DescriptorRegistered`] notification.
    fn is_descriptor_registered(&self, descriptor: Descriptor<DescriptorPublicKey>);
    /// Prompt user to register given descriptor.
    /// The signer must return a [`SignerNotif::DescriptorRegistered`] notification.
    fn register_descriptor(&mut self, descriptor: Descriptor<DescriptorPublicKey>);
    /// Request the signer to sign the given psbt. A descriptor must be loaded
    /// prior to call this function.
    /// The signer must return a [`SignerNotif::DescriptorLoaded`] notification.
    fn sign(&self, psbt: Psbt, descriptor: Descriptor<DescriptorPublicKey>);
    /// Request the signer to display the address for verification.
    /// No notification is expected in return.
    fn display_address(&self, _deriv: (AddrAccount, u32)) {}
}

macro_rules! send {
    ($s:ident, $notif:ident($val1:expr)) => {
        if let Some(sender) = &$s.sender {
            if let Err(e) = sender.send(SignerNotif::$notif($s.fingerprint(), $val1)) {
                log::error!("Signer fail to send notification: {e:?}");
            }
        }
    };
    ($s:ident, $notif:ident($val1:expr, $val2:expr)) => {
        if let Some(sender) = &$s.sender {
            if let Err(e) = sender.send(SignerNotif::$notif($s.fingerprint(), $val1, $val2)) {
                log::error!("Signer fail to send notification: {e:?}");
            }
        }
    };
}

impl Signer for HotSigner {
    fn init(&mut self, channel: mpsc::Sender<SignerNotif>) {
        self.sender = Some(channel);
        self.info();
    }

    fn info(&self) {
        send!(self, Info(serde_json::Value::Null));
    }

    fn get_xpub(&self, deriv: DerivationPath, _display: bool) {
        let xpub = self.xpub(&deriv);
        if let Some(sender) = &self.sender {
            let _ = sender.send(SignerNotif::Xpub(self.fingerprint(), xpub));
        }
    }

    fn is_descriptor_registered(&self, descriptor: Descriptor<DescriptorPublicKey>) {
        let registered = self.descriptors.contains(&descriptor);
        if let Some(sender) = &self.sender {
            let _ = sender.send(SignerNotif::DescriptorRegistered(
                self.fingerprint(),
                descriptor,
                registered,
            ));
        }
    }

    fn register_descriptor(&mut self, descriptor: Descriptor<DescriptorPublicKey>) {
        let wrong_network = descriptor.for_any_key(|k| match k {
            DescriptorPublicKey::Single(_) => true,
            DescriptorPublicKey::XPub(key) => match (self.network, key.xkey.network) {
                (bitcoin::Network::Bitcoin, NetworkKind::Main) => false,
                (bitcoin::Network::Bitcoin, NetworkKind::Test) => true,
                (_, NetworkKind::Main) => true,
                _ => false,
            },
            DescriptorPublicKey::MultiXPub(key) => match (self.network, key.xkey.network) {
                (bitcoin::Network::Bitcoin, NetworkKind::Main) => false,
                (bitcoin::Network::Bitcoin, NetworkKind::Test) => true,
                (_, NetworkKind::Main) => true,
                _ => false,
            },
        });
        if !wrong_network {
            self.descriptors.insert(descriptor.clone());
        }
        if let Some(sender) = &self.sender {
            let response = if wrong_network {
                SignerNotif::Error(self.fingerprint(), Error::DescriptorNetwork)
            } else {
                SignerNotif::DescriptorRegistered(self.fingerprint(), descriptor, true)
            };
            let _ = sender.send(response);
        }
    }

    fn sign(&self, mut psbt: Psbt, descriptor: Descriptor<DescriptorPublicKey>) {
        let response = if self.descriptors.contains(&descriptor) {
            if let Err(e) = self.inner_sign(&mut psbt, &descriptor) {
                SignerNotif::Error(self.fingerprint(), e)
            } else {
                SignerNotif::Signed(self.fingerprint(), psbt)
            }
        } else {
            SignerNotif::Error(self.fingerprint(), Error::UnregisteredDescriptor)
        };
        if let Some(sender) = &self.sender {
            let _ = sender.send(response);
        }
    }
}

/// A struct that represents a hot signer for Bitcoin transactions.
///
/// This struct is responsible for managing the private keys and generating
/// addresses for receiving and change. It can create signatures for transactions
/// using the provided private keys.
#[derive(Debug, Clone)]
pub struct HotSigner {
    master_xpriv: bip32::Xpriv,
    fingerprint: bip32::Fingerprint,
    secp: secp256k1::Secp256k1<All>,
    mnemonic: Option<bip39::Mnemonic>,
    descriptors: BTreeSet<Descriptor<DescriptorPublicKey>>,
    network: bitcoin::Network,
    sender: Option<mpsc::Sender<SignerNotif>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonSigner {
    mnemonic: bip39::Mnemonic,
    descriptors: BTreeSet<String>,
    network: bitcoin::Network,
}

/// Creates a WPKH descriptor from the given extended public key (OXpub).
///
/// # Arguments
/// * `xpub` - An instance of `OXpub` representing the extended public key.
///
/// # Returns
/// A `Descriptor<DescriptorPublicKey>` that represents the wpkh descriptor.
pub fn wpkh(xpub: OXpub) -> Descriptor<DescriptorPublicKey> {
    let descr_str = format!(
        "wpkh([{}/{}]{}/<0;1>/*)",
        xpub.origin.0, xpub.origin.1, xpub.xkey
    );
    Descriptor::<DescriptorPublicKey>::from_str(&descr_str).expect("hardcoded descriptor")
}

/// A struct that represents an extended private key.
///
/// This struct contains the origin fingerprint and derivation path
/// associated with the extended private key, as well as the key itself.
///
/// # Fields
/// * `origin` - A tuple containing the fingerprint and derivation path.
/// * `xkey` - The extended private key.
pub struct OXpriv {
    pub origin: (bip32::Fingerprint, bip32::DerivationPath),
    pub xkey: bip32::Xpriv,
}

/// A struct that represents an extended public key.
///
/// This struct contains the origin fingerprint and derivation path
/// associated with the extended public key, as well as the key itself.
///
/// # Fields
/// * `origin` - A tuple containing the fingerprint and derivation path.
/// * `xkey` - The extended public key.
#[derive(Debug)]
pub struct OXpub {
    pub origin: (bip32::Fingerprint, bip32::DerivationPath),
    pub xkey: bip32::Xpub,
}

impl HotSigner {
    pub fn to_json(&self) -> Option<JsonSigner> {
        let descriptors = self.descriptors.iter().map(|d| d.to_string()).collect();
        self.mnemonic.as_ref().map(|mnemonic| JsonSigner {
            mnemonic: mnemonic.clone(),
            descriptors,
            network: self.network,
        })
    }
    pub fn from_json(json: JsonSigner) -> Self {
        let mut signer = HotSigner::new_from_mnemonics(json.network, &json.mnemonic.to_string())
            .expect("valid signer");
        #[allow(clippy::mutable_key_type)]
        let descriptors = json
            .descriptors
            .into_iter()
            .filter_map(|d| Descriptor::from_str(&d).ok())
            .collect();
        signer.descriptors = descriptors;
        signer
    }
    /// Create a new [`HotSigner`] instance from the provided Xpriv key.
    ///
    /// # Arguments
    /// * `network` - The Bitcoin network (e.g., Bitcoin, Testnet, Signet, Regtest).
    /// * `xpriv` - The extended private key that the signer will use.
    ///
    /// # Returns
    /// A new instance of [`HotSigner`].
    pub fn new_from_xpriv(network: bitcoin::Network, xpriv: bip32::Xpriv) -> Self {
        let secp = secp256k1::Secp256k1::new();
        let fingerprint = xpriv.fingerprint(&secp);
        let master_xpriv = xpriv;

        HotSigner {
            master_xpriv,
            fingerprint,
            secp,
            mnemonic: None,
            descriptors: BTreeSet::new(),
            network,
            sender: None,
        }
    }

    /// Create a new [`HotSigner`] instance from a mnemonic phrase.
    ///
    /// The mnemonic is stored in the [`HotSigner::mnemonic`] field.
    ///
    /// # Arguments
    /// * `network` - The Bitcoin network (e.g., Bitcoin, Testnet, Signet, Regtest).
    /// * `mnemonic` - A string representing the mnemonic phrase used to generate the keys.
    ///
    /// # Returns
    /// A result containing a new instance of [`HotSigner`] or an error if the mnemonic is invalid.
    pub fn new_from_mnemonics(network: bitcoin::Network, mnemonic: &str) -> Result<Self, Error> {
        let mnemonic = bip39::Mnemonic::from_str(mnemonic)?;
        let seed = mnemonic.to_seed("");
        let key = bip32::Xpriv::new_master(network, &seed).map_err(|_| Error::XPrivFromSeed)?;
        let mut signer = Self::new_from_xpriv(network, key);
        signer.mnemonic = Some(mnemonic);
        Ok(signer)
    }

    /// Generate a new signer and it's private key.
    /// The mnemonic is stored in [`HotSigner::mnemonic`] field.
    ///
    /// Note: generating a private key by this way is not safe enough
    ///   to use on mainnet, so we decide to forbid usage of this method on mainnet.
    ///   This method will panic if `network` have [`Network::Bitcoin`] value.
    pub fn new(network: bitcoin::Network) -> Result<Self, Error> {
        // Should not be used on mainnet
        assert_ne!(network, bitcoin::Network::Bitcoin);
        let mnemonic = bip39::Mnemonic::generate(12).expect("12 words must not fail");
        let mut signer = Self::new_from_mnemonics(network, &mnemonic.to_string())?;
        signer.mnemonic = Some(mnemonic);
        Ok(signer)
    }

    /// Registers a descriptor for the signer.
    ///
    /// This function adds the given descriptor to the signer's internal set of
    /// descriptors if it is not already registered.
    ///
    /// # Arguments
    /// * `descriptor` - The descriptor to be registered.
    pub fn inner_register_descriptor(&mut self, descriptor: Descriptor<DescriptorPublicKey>) {
        if !self.descriptors.contains(&descriptor) {
            self.descriptors.insert(descriptor);
        }
    }

    /// Retrieves the extended private key at the specified derivation path.
    ///
    /// # Arguments
    /// * `path` - The derivation path for which to retrieve the extended private key.
    ///
    /// # Returns
    /// An instance of `OXpriv` containing the origin fingerprint and the derived
    /// extended private key.
    pub fn xpriv(&self, path: &DerivationPath) -> OXpriv {
        let xkey = self
            .master_xpriv
            .derive_priv(&self.secp, path)
            .expect("cannot fail");

        OXpriv {
            origin: (self.fingerprint, path.clone()),
            xkey,
        }
    }

    /// Retrieves the extended public key at the specified derivation path.
    ///
    /// # Arguments
    /// * `path` - The derivation path for which to retrieve the extended public key.
    ///
    /// # Returns
    /// An instance of `OXpub` containing the origin fingerprint and the derived
    /// extended public key.
    pub fn xpub(&self, path: &DerivationPath) -> OXpub {
        let xpriv = self.xpriv(path);
        let xkey = bip32::Xpub::from_priv(&self.secp, &xpriv.xkey);

        OXpub {
            origin: xpriv.origin,
            xkey,
        }
    }

    /// Retrieves the private key at the specified derivation path from the master_xpriv.
    ///
    /// # Arguments
    /// * `path` - The derivation path for which to retrieve the private key.
    ///
    /// # Returns
    /// The private key as a [`secp256k1::SecretKey`].
    fn private_key_at(&self, path: &DerivationPath) -> secp256k1::SecretKey {
        self.master_xpriv
            .derive_priv(self.secp(), path)
            .expect("deriveable")
            .private_key
    }

    /// Retrieves the public key at the specified derivation path from the master_xpriv.
    ///
    /// # Arguments
    /// * `path` - The derivation path for which to retrieve the public key.
    ///
    /// # Returns
    /// The public key as a [`secp256k1::PublicKey`].
    pub fn public_key_at(&self, path: &DerivationPath) -> secp256k1::PublicKey {
        self.private_key_at(path).public_key(self.secp())
    }

    /// Sign the provided PSBT
    ///
    /// This function processes each input of the PSBT, checks for the necessary
    /// witness UTXO, and generates the required signatures using the private keys
    /// derived from the signer's extended private key. It only supports the
    /// SIGHASH_ALL signature type.
    ///
    /// # Arguments
    /// * `psbt` - A mutable reference to the PSBT that will be signed.
    ///
    /// # Returns
    /// A result indicating success or failure. Returns an error if:
    /// * Any input's witness UTXO is missing.
    /// * The expected script public key does not match.
    /// * The signature generation fails.
    pub fn inner_sign(
        &self,
        psbt: &mut Psbt,
        descriptor: &Descriptor<DescriptorPublicKey>,
    ) -> Result<(), Error> {
        let mut cache = sighash::SighashCache::new(psbt.unsigned_tx.clone());
        let derivator = Derivator::new(descriptor.clone(), self.network).unwrap();

        let mut inputs_to_sign = BTreeMap::new();
        for (index, input) in psbt.inputs.iter().enumerate() {
            let mut derivation_paths = vec![];
            input.bip32_derivation.iter().for_each(|(_, (fg, deriv))| {
                if *fg == self.fingerprint() {
                    derivation_paths.push(deriv.clone());
                }
            });
            if !derivation_paths.is_empty() {
                if input.witness_utxo.is_none() {
                    Err(Error::MissingWitnessUtxo)?
                }
                // FIXME: process sighash w/o psbt helper?
                let (hash, sighash_type) = psbt.sighash_ecdsa(0, &mut cache).map_err(|e| {
                    log::error!("Fail to generate sig hash: {e}");
                    Error::SighashFail
                })?;
                // NOTE: we only allow SigHash ALL for now
                if sighash_type != EcdsaSighashType::All {
                    return Err(Error::SighashFail);
                }
                inputs_to_sign.insert(index, (hash, psbt.inputs[index].clone(), derivation_paths));
            }
        }
        for (index, (hash, mut input, deriv)) in inputs_to_sign {
            self.sign_input(hash, &mut input, deriv, &derivator)?;
            psbt.inputs[index] = input;
        }
        Ok(())
    }

    /// Sign the input for a transaction.
    ///
    /// # Arguments
    /// * `hash` - The hash of the transaction to be signed.
    /// * `input` - A mutable reference to the input that will be signed.
    /// * `deriv` - A vector of derivation paths used to derive the signing keys.
    ///
    /// # Returns
    /// A result indicating success or failure. Returns an error if:
    /// * The input's witness UTXO is missing.
    /// * The expected script public key does not match.
    /// * The signature is invalid.
    ///
    /// This function iterates over the provided derivation paths, derives the signing key,
    /// and signs the transaction input with the corresponding private key.
    /// Note: only SIGHASH_ALL is supported for now
    pub fn sign_input(
        &self,
        hash: Message,
        input: &mut Input,
        deriv: Vec<DerivationPath>,
        derivator: &Derivator,
    ) -> Result<(), Error> {
        for d in &deriv {
            let signing_key = self.private_key_at(d);
            let pubkey = self.public_key_at(d);

            if !input.bip32_derivation.contains_key(&pubkey) {
                // NOTE: this can happen in case of fingerprint collision
                continue;
            }

            if let Some(wit) = &input.witness_utxo {
                let ap = account_path(d)?;
                let expected_spk = match ap.0 {
                    AddrAccount::Receive => derivator.receive_at(ap.1),
                    AddrAccount::Change => derivator.change_at(ap.1),
                }
                .script_pubkey();
                if wit.script_pubkey != expected_spk {
                    Err(Error::SpkNotMatch)
                } else {
                    Ok(())
                }
            } else {
                Err(Error::MissingWitnessUtxo)
            }?;

            let signature = self.secp.sign_ecdsa_low_r(&hash, &signing_key);

            self.secp()
                .verify_ecdsa(&hash, &signature, &pubkey)
                .map_err(|_| Error::InvalidSignature)?;

            let signature = ecdsa::Signature {
                signature,
                // NOTE: we only allow SigHash ALL for now
                sighash_type: EcdsaSighashType::All,
            };
            input.partial_sigs.insert(pubkey.into(), signature);
        }

        Ok(())
    }

    /// Returns the [`Fingerprint`] of this [`HotSigner`].
    pub fn fingerprint(&self) -> bip32::Fingerprint {
        self.fingerprint
    }

    /// Return the secp context of this signer
    fn secp(&self) -> &secp256k1::Secp256k1<All> {
        &self.secp
    }

    /// Returns a copy of the mnemonic if not None
    #[allow(unused)]
    fn mnemonic(&self) -> Option<bip39::Mnemonic> {
        self.mnemonic.clone()
    }
}

/// Converts a tuple containing an account type and an index into a derivation path.
///
/// # Arguments
/// * `path` - A tuple where the first element is an [`AddrAccount`] representing the account type,
///   and the second element is a `u32` representing the index.
///
/// # Returns
/// A result containing the derived [`DerivationPath`] or an error if the conversion fails.
pub fn deriv_path(path: &(AddrAccount, u32)) -> Result<DerivationPath, Error> {
    let account_u32: u32 = path.0.into();
    DerivationPath::from_str(&format!("m/{}/{}", account_u32, path.1))
        .map_err(|_| Error::DerivationPath)
}

/// Converts a derivation path into a tuple containing an account type and an index.
///
/// # Arguments
/// * `path` - A reference to a [`DerivationPath`] that contains the account type and index.
///
/// # Returns
/// A result containing a tuple of the account type as [`AddrAccount`] and the index as `u32`.
/// Returns an error if the derivation path does not have the expected length.
pub fn account_path(path: &DerivationPath) -> Result<(AddrAccount, u32), Error> {
    if path.len() != 2 {
        Err(Error::DerivationPath)?
    }
    let path = path.to_u32_vec();
    Ok((path[0].into(), path[1]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{random_output, setup_logger, txid};
    use bitcoin::Network;
    use miniscript::bitcoin::{absolute::Height, Amount, ScriptBuf, TxIn, Witness};
    use std::sync::mpsc;

    #[test]
    fn test_create_hot_signer_from_xpriv() {
        let network = Network::Testnet;
        let xpriv =
            bip32::Xpriv::new_master(network, &bip39::Mnemonic::generate(12).unwrap().to_seed(""))
                .unwrap();
        let signer = HotSigner::new_from_xpriv(network, xpriv);
        assert_eq!(signer.network, network);
    }

    #[test]
    fn test_create_hot_signer_from_mnemonic() {
        let network = Network::Testnet;
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let signer = HotSigner::new_from_mnemonics(network, mnemonic).unwrap();
        assert_eq!(signer.network, network);
    }

    #[test]
    fn test_sign_transaction() {
        setup_logger();
        let network = Network::Testnet;
        let xpriv =
            bip32::Xpriv::new_master(network, &bip39::Mnemonic::generate(12).unwrap().to_seed(""))
                .unwrap();
        let signer = HotSigner::new_from_xpriv(network, xpriv);
        let xpub = signer.xpub(&DerivationPath::from_str("m/84'/0'/0'/1'").unwrap());
        let descriptor = wpkh(xpub);

        let txin = TxIn {
            previous_output: bitcoin::OutPoint {
                txid: txid(0),
                vout: 1,
            },
            script_sig: ScriptBuf::new(),
            sequence: bitcoin::Sequence::ZERO,
            witness: Witness::new(),
        };

        let txout = random_output();

        let tx = bitcoin::Transaction {
            version: bitcoin::transaction::Version(2),
            lock_time: bitcoin::absolute::LockTime::Blocks(Height::ZERO),
            input: vec![txin],
            output: vec![txout],
        };

        let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();

        let deriv = &(AddrAccount::Receive, 0);
        let deriv_p = deriv_path(deriv).unwrap();
        let pubkey = signer.public_key_at(&deriv_p);

        // there is no signature
        assert!(psbt.inputs[0].partial_sigs.is_empty());

        // try to sign the tx
        signer.inner_sign(&mut psbt, &descriptor).unwrap();

        // there is no signature as bip32_derivation is missing
        assert!(psbt.inputs[0].partial_sigs.is_empty());

        // add a wrong derivation path
        let w_deriv = &(AddrAccount::Change, 0);
        let w_deriv_path = deriv_path(w_deriv).unwrap();
        psbt.inputs
            .get_mut(0)
            .unwrap()
            .bip32_derivation
            .insert(pubkey, (signer.fingerprint(), w_deriv_path));

        // try to sign the tx
        let res = signer.inner_sign(&mut psbt, &descriptor);

        // witness_utxo is missing
        assert_eq!(res, Err(Error::MissingWitnessUtxo));

        // there is no signature
        assert!(psbt.inputs[0].partial_sigs.is_empty());

        let derivator = Derivator::new(descriptor.clone(), bitcoin::Network::Regtest).unwrap();

        // add spent TxOut
        psbt.inputs.get_mut(0).unwrap().witness_utxo = Some(bitcoin::TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: derivator.receive_spk_at(deriv.1),
        });

        // try to sign the tx
        signer.inner_sign(&mut psbt, &descriptor).unwrap();

        // there is no signature as bip32_derivation is wrong and the public key
        // do not match only the fingerprint
        assert!(psbt.inputs[0].partial_sigs.is_empty());

        // cleanup deriv path map
        psbt.inputs[0].bip32_derivation.clear();

        // add the bip32 deriv
        psbt.inputs
            .get_mut(0)
            .unwrap()
            .bip32_derivation
            .insert(pubkey, (signer.fingerprint(), deriv_p));

        // sign the tx
        signer.inner_sign(&mut psbt, &descriptor).unwrap();

        // signature was added
        assert!(!psbt.inputs[0].partial_sigs.is_empty());
    }

    // Notification Signer tests

    struct MockSender {
        receiver: mpsc::Receiver<SignerNotif>,
    }

    impl MockSender {
        fn new() -> (mpsc::Sender<SignerNotif>, Self) {
            let (sender, receiver) = mpsc::channel();
            (sender, MockSender { receiver })
        }
    }

    #[test]
    fn test_signer_init() {
        let (sender, mock) = MockSender::new();
        let mut signer = HotSigner::new_from_xpriv(
            Network::Regtest,
            bip32::Xpriv::new_master(
                Network::Regtest,
                &bip39::Mnemonic::generate(12).unwrap().to_seed(""),
            )
            .unwrap(),
        );
        signer.init(sender);

        let notif = mock.receiver.recv().unwrap();
        match notif {
            SignerNotif::Info(fg, _) => {
                assert_eq!(signer.fingerprint(), fg);
            }
            _ => panic!("Expected Info notification"),
        }
    }

    #[test]
    fn test_signer_info() {
        let (sender, mock) = MockSender::new();
        let mut signer = HotSigner::new_from_xpriv(
            Network::Regtest,
            bip32::Xpriv::new_master(
                Network::Regtest,
                &bip39::Mnemonic::generate(12).unwrap().to_seed(""),
            )
            .unwrap(),
        );
        signer.init(sender);
        signer.info();

        let notif = mock.receiver.recv().unwrap();
        match notif {
            SignerNotif::Info(fg, _) => {
                assert_eq!(signer.fingerprint(), fg);
            }
            _ => panic!("Expected Info notification"),
        }
    }

    #[test]
    fn test_signer_get_xpub() {
        let (sender, mock) = MockSender::new();
        let mut signer = HotSigner::new_from_xpriv(
            Network::Regtest,
            bip32::Xpriv::new_master(
                Network::Regtest,
                &bip39::Mnemonic::generate(12).unwrap().to_seed(""),
            )
            .unwrap(),
        );
        signer.init(sender);
        let derivation_path = DerivationPath::from_str("m/84'/0'/0'/0").unwrap();
        signer.get_xpub(derivation_path, false);

        // first notif in info
        let _ = mock.receiver.recv().unwrap();

        // second is expected to be xpub
        let notif = mock.receiver.recv().unwrap();
        match notif {
            SignerNotif::Xpub(fg, _) => {
                assert_eq!(signer.fingerprint(), fg);
            }
            _ => panic!("Expected Xpub notification"),
        }
    }

    #[test]
    fn test_signer_is_descriptor_registered() {
        let (sender, mock) = MockSender::new();
        let mut signer = HotSigner::new_from_xpriv(
            Network::Regtest,
            bip32::Xpriv::new_master(
                Network::Regtest,
                &bip39::Mnemonic::generate(12).unwrap().to_seed(""),
            )
            .unwrap(),
        );
        signer.init(sender);
        // info notif
        let _ = mock.receiver.recv();
        let descriptor = wpkh(signer.xpub(&DerivationPath::from_str("m/84'/0'/0'/0").unwrap()));

        signer.is_descriptor_registered(descriptor.clone());
        let notif = mock.receiver.recv().unwrap();
        match notif {
            SignerNotif::DescriptorRegistered(fg, desc, false) => {
                assert_eq!(signer.fingerprint(), fg);
                assert_eq!(desc, descriptor);
            }
            _ => panic!("Expected DescriptorRegistered notification"),
        }

        signer.register_descriptor(descriptor.clone());
        let notif = mock.receiver.recv().unwrap();
        match notif {
            SignerNotif::DescriptorRegistered(fg, desc, true) => {
                assert_eq!(signer.fingerprint(), fg);
                assert_eq!(desc, descriptor);
            }
            _ => panic!("Expected DescriptorRegistered notification"),
        }

        signer.is_descriptor_registered(descriptor.clone());
        let notif = mock.receiver.recv().unwrap();
        match notif {
            SignerNotif::DescriptorRegistered(fg, desc, true) => {
                assert_eq!(signer.fingerprint(), fg);
                assert_eq!(desc, descriptor);
            }
            _ => panic!("Expected DescriptorRegistered notification"),
        }
    }

    #[test]
    fn test_signer_sign() {
        let (sender, mock) = MockSender::new();
        let mut signer = HotSigner::new_from_xpriv(
            Network::Regtest,
            bip32::Xpriv::new_master(
                Network::Regtest,
                &bip39::Mnemonic::generate(12).unwrap().to_seed(""),
            )
            .unwrap(),
        );
        let derivation_path = DerivationPath::from_str("m/84'/0'/0'/0").unwrap();
        let descriptor = wpkh(signer.xpub(&derivation_path));

        signer.init(sender);
        let notif = mock.receiver.recv().unwrap();
        match notif {
            SignerNotif::Info(fg, _) => {
                assert_eq!(signer.fingerprint(), fg);
            }
            _ => panic!("Expected DescriptorRegistered notification"),
        }

        signer.register_descriptor(descriptor.clone());
        let notif = mock.receiver.recv().unwrap();
        match notif {
            SignerNotif::DescriptorRegistered(fg, desc, true) => {
                assert_eq!(signer.fingerprint(), fg);
                assert_eq!(desc, descriptor);
            }
            _ => panic!("Expected DescriptorRegistered notification"),
        }

        let txin = TxIn {
            previous_output: bitcoin::OutPoint {
                txid: txid(0),
                vout: 1,
            },
            script_sig: ScriptBuf::new(),
            sequence: bitcoin::Sequence::ZERO,
            witness: Witness::new(),
        };

        let txout = random_output();

        let tx = bitcoin::Transaction {
            version: bitcoin::transaction::Version(2),
            lock_time: bitcoin::absolute::LockTime::Blocks(Height::ZERO),
            input: vec![txin],
            output: vec![txout],
        };

        let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();

        let deriv = &(AddrAccount::Receive, 0);
        let deriv_p = deriv_path(deriv).unwrap();
        let pubkey = signer.public_key_at(&deriv_p);

        let derivator = Derivator::new(descriptor.clone(), bitcoin::Network::Regtest).unwrap();

        // add spent TxOut
        psbt.inputs.get_mut(0).unwrap().witness_utxo = Some(bitcoin::TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: derivator.receive_spk_at(deriv.1),
        });

        // add the bip32 deriv
        psbt.inputs
            .get_mut(0)
            .unwrap()
            .bip32_derivation
            .insert(pubkey, (signer.fingerprint(), deriv_p));

        signer.sign(psbt, descriptor);
        let notif = mock.receiver.recv().unwrap();
        match notif {
            SignerNotif::Signed(fg, psbt) => {
                assert_eq!(signer.fingerprint(), fg);
                assert!(!psbt.inputs[0].partial_sigs.is_empty());
            }
            _ => panic!("Expected DescriptorRegistered notification"),
        }
    }
}
