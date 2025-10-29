use bip39::Mnemonic;
use miniscript::bitcoin::{
    bip32::{Fingerprint, Xpriv},
    key::Secp256k1,
    secp256k1::All,
    Network,
};
use std::str::FromStr;

pub enum Error {
    XPrivFromSeed,
    InvalidMnemonicWords,
}

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
