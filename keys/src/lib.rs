use bip39::Mnemonic;
use bitcoin::{
    bip32::{DerivationPath, Fingerprint, Xpriv, Xpub},
    key::Secp256k1,
    secp256k1::All,
    Network,
};
use std::str::FromStr;

pub enum Error {
    XPrivFromSeed,
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
    pub origin: (Fingerprint, DerivationPath),
    pub xkey: Xpub,
}

#[derive(Debug)]
pub struct KeyDerivator {
    network: Network,
    secp: Secp256k1<All>,
    fingerprint: Fingerprint,
    master_xpriv: Xpriv,
}

impl KeyDerivator {
    pub fn new_from_mnemonic_str(network: Network, mnemonic: &str) -> Result<Self, Error> {
        Self::new_from_mnemonic_str_with_passphrase(network, mnemonic, "")
    }
    pub fn new_from_mnemonic_str_with_passphrase(
        network: Network,
        mnemonic: &str,
        passphrase: &str,
    ) -> Result<Self, Error> {
        let mnemonic =
            bip39::Mnemonic::from_str(mnemonic).map_err(|_| Error::InvalidMnemonicWords)?;
        Self::new_from_mnemonic_with_passphrase(network, mnemonic, passphrase)
    }

    pub fn new_from_mnemonic(network: Network, mnemonic: Mnemonic) -> Result<Self, Error> {
        Self::new_from_mnemonic_with_passphrase(network, mnemonic, "")
    }

    pub fn new_from_mnemonic_with_passphrase(
        network: Network,
        mnemonic: Mnemonic,
        passphrase: &str,
    ) -> Result<Self, Error> {
        let seed = mnemonic.to_seed(passphrase);
        let xpriv = Xpriv::new_master(network, &seed).map_err(|_| Error::XPrivFromSeed)?;
        Ok(Self::new_from_xpriv(network, xpriv))
    }

    pub fn new_from_xpriv(network: Network, xpriv: Xpriv) -> Self {
        let secp = Secp256k1::new();
        let fingerprint = xpriv.fingerprint(&secp);
        Self {
            network,
            secp,
            fingerprint,
            master_xpriv: xpriv,
        }
    }
}
