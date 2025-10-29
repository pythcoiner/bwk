use bip39::Mnemonic;
use bitcoin::{
    bip32::{DerivationPath, Fingerprint, Xpriv, Xpub},
    key::Secp256k1,
    secp256k1::{All, PublicKey, SecretKey},
    Network,
};
use bwk_utils::short_string;
use std::{
    fmt::{Debug, Display},
    str::FromStr,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Fail to create Xpriv from seed.")]
    XPrivFromSeed,
    #[error("Mnemonics words are invalid.")]
    InvalidMnemonicWords,
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
    pub origin: (Fingerprint, DerivationPath),
    pub xkey: Xpriv,
}

impl Debug for OXpriv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OXpriv")
            .field("origin", &self.origin)
            .field("xkey", &"[redacted]")
            .finish()
    }
}

/// A struct that represents an extended public key.
///
/// This struct contains the origin fingerprint and derivation path
/// associated with the extended public key, as well as the key itself.
///
/// # Fields
/// * `origin` - A tuple containing the fingerprint and derivation path.
/// * `xkey` - The extended public key.
pub struct OXpub {
    pub origin: (Fingerprint, DerivationPath),
    pub xkey: Xpub,
}

impl Debug for OXpub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OXpub")
            .field("origin", &self.origin)
            .field("xkey", &short_string(self.xkey.to_string(), 18))
            .finish()
    }
}

impl Display for OXpub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}/{}]{}", self.origin.0, self.origin.1, self.xkey)
    }
}

/// A utility struct for deriving extended private keys, extended public keys,
/// and ephemeral keys from a master seed or extended private key.
#[derive(Debug)]
pub struct KeyDerivator {
    secp: Secp256k1<All>,
    fingerprint: Fingerprint,
    master_xpriv: Xpriv,
}

impl KeyDerivator {
    /// Creates a new `KeyDerivator` from a mnemonic string without a passphrase.
    ///
    /// # Arguments
    /// * `network` - The Bitcoin network for which the key derivator is created.
    /// * `mnemonic` - A string representation of the BIP39 mnemonic.
    ///
    /// # Returns
    /// A `Result` containing the `KeyDerivator` instance or an `Error` if the creation fails.
    pub fn new_from_mnemonic_str(network: Network, mnemonic: &str) -> Result<Self, Error> {
        Self::new_from_mnemonic_str_with_passphrase(network, mnemonic, "")
    }

    /// Creates a new `KeyDerivator` from a mnemonic string with a passphrase.
    ///
    /// # Arguments
    /// * `network` - The Bitcoin network for which the key derivator is created.
    /// * `mnemonic` - A string representation of the BIP39 mnemonic.
    /// * `passphrase` - The passphrase to use for seed generation.
    ///
    /// # Returns
    /// A `Result` containing the `KeyDerivator` instance or an `Error` if the creation fails.
    pub fn new_from_mnemonic_str_with_passphrase(
        network: Network,
        mnemonic: &str,
        passphrase: &str,
    ) -> Result<Self, Error> {
        let mnemonic =
            bip39::Mnemonic::from_str(mnemonic).map_err(|_| Error::InvalidMnemonicWords)?;
        Self::new_from_mnemonic_with_passphrase(network, mnemonic, passphrase)
    }

    /// Creates a new `KeyDerivator` from a `Mnemonic` instance without a passphrase.
    ///
    /// # Arguments
    /// * `network` - The Bitcoin network for which the key derivator is created.
    /// * `mnemonic` - A `Mnemonic` instance representing the BIP39 mnemonic.
    ///
    /// # Returns
    /// A `Result` containing the `KeyDerivator` instance or an `Error` if the creation fails.
    pub fn new_from_mnemonic(network: Network, mnemonic: Mnemonic) -> Result<Self, Error> {
        Self::new_from_mnemonic_with_passphrase(network, mnemonic, "")
    }

    /// Creates a new `KeyDerivator` from a `Mnemonic` instance with a passphrase.
    ///
    /// # Arguments
    /// * `network` - The Bitcoin network for which the key derivator is created.
    /// * `mnemonic` - A `Mnemonic` instance representing the BIP39 mnemonic.
    /// * `passphrase` - The passphrase to use for seed generation.
    ///
    /// # Returns
    /// A `Result` containing the `KeyDerivator` instance or an `Error` if the creation fails.
    pub fn new_from_mnemonic_with_passphrase(
        network: Network,
        mnemonic: Mnemonic,
        passphrase: &str,
    ) -> Result<Self, Error> {
        let seed = mnemonic.to_seed(passphrase);
        let xpriv = Xpriv::new_master(network, &seed).map_err(|_| Error::XPrivFromSeed)?;
        Ok(Self::new_from_xpriv(xpriv))
    }

    /// Creates a new `KeyDerivator` from an extended private key.
    ///
    /// # Arguments
    /// * `xpriv` - The extended private key to initialize the derivator with.
    ///
    /// # Returns
    /// A `KeyDerivator` instance.
    pub fn new_from_xpriv(xpriv: Xpriv) -> Self {
        let secp = Secp256k1::new();
        let fingerprint = xpriv.fingerprint(&secp);
        Self {
            secp,
            fingerprint,
            master_xpriv: xpriv,
        }
    }

    /// Returns the fingerprint of the master extended private key.
    ///
    /// # Returns
    /// The `Fingerprint` of the master key.
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// Retrieves the extended private key at the specified derivation path.
    ///
    /// # Arguments
    /// * `path` - The derivation path for which to retrieve the extended private key.
    ///
    /// # Returns
    /// An instance of `OXpriv` containing the origin fingerprint and the derived
    /// extended private key.
    pub fn xpriv_at(&self, path: &DerivationPath) -> OXpriv {
        let xkey = self
            .master_xpriv
            .derive_priv(&self.secp, path)
            .expect("cannot fail");

        OXpriv {
            origin: (self.fingerprint, path.clone()),
            xkey,
        }
    }

    /// Retrieves the secret key at the specified derivation path.
    ///
    /// # Arguments
    /// * `path` - The derivation path for which to retrieve the secret key.
    ///
    /// # Returns
    /// The `SecretKey` derived at the given path.
    pub fn secret_key_at(&self, path: &DerivationPath) -> SecretKey {
        self.master_xpriv
            .derive_priv(&self.secp, path)
            .expect("cannot fail")
            .private_key
    }

    /// Retrieves the public key at the specified derivation path.
    ///
    /// # Arguments
    /// * `path` - The derivation path for which to retrieve the public key.
    ///
    /// # Returns
    /// The `PublicKey` derived at the given path.
    pub fn public_key_at(&self, path: &DerivationPath) -> PublicKey {
        self.master_xpriv
            .derive_priv(&self.secp, path)
            .expect("cannot fail")
            .private_key
            .public_key(&self.secp)
    }

    /// Retrieves the extended public key at the specified derivation path.
    ///
    /// # Arguments
    /// * `path` - The derivation path for which to retrieve the extended public key.
    ///
    /// # Returns
    /// An instance of `OXpub` containing the origin fingerprint and the derived
    /// extended public key.
    pub fn xpub_at(&self, path: &DerivationPath) -> OXpub {
        let xpriv = self.xpriv_at(path);
        let xkey = Xpub::from_priv(&self.secp, &xpriv.xkey);

        OXpub {
            origin: xpriv.origin,
            xkey,
        }
    }
}
