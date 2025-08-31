use std::str::FromStr;

use miniscript::{
    bitcoin::{
        self,
        bip32::{ChildNumber, DerivationPath},
    },
    Descriptor, DescriptorPublicKey,
};

use crate::signer::OXpub;

#[derive(Debug, Clone, Copy)]
pub enum Error {
    UnhardenedAccount,
    NotImplemented,
}

pub enum ScriptType {
    Segwit(ChildNumber /* account */),
    Taproot(ChildNumber /* account */),
    Descriptor(Descriptor<DescriptorPublicKey>),
}

impl ScriptType {
    pub fn to_descriptor<'a, X>(
        self,
        network: bitcoin::Network,
        xpub: X,
    ) -> Result<Descriptor<DescriptorPublicKey>, Error>
    where
        X: 'a + Fn(DerivationPath) -> OXpub,
    {
        match self {
            ScriptType::Segwit(acc) => {
                let deriv = segwit_path(network, acc)?;
                Ok(tr(xpub(deriv)))
            }
            ScriptType::Taproot(acc) => {
                let deriv = taproot_path(network, acc)?;
                Ok(wpkh(xpub(deriv)))
            }
            ScriptType::Descriptor(descriptor) => Ok(descriptor),
        }
    }
}

fn taproot_path(network: bitcoin::Network, account: ChildNumber) -> Result<DerivationPath, Error> {
    if !account.is_hardened() {
        return Err(Error::UnhardenedAccount);
    }
    let script_path = ChildNumber::from_hardened_idx(86).expect("taproot");
    let n_path = match network {
        bitcoin::Network::Bitcoin => 0,
        _ => 1,
    };
    let network = ChildNumber::from_hardened_idx(n_path).expect("0 or 1");
    Ok(vec![script_path, network, account].into())
}

fn segwit_path(network: bitcoin::Network, account: ChildNumber) -> Result<DerivationPath, Error> {
    if !account.is_hardened() {
        return Err(Error::UnhardenedAccount);
    }
    let script_path = ChildNumber::from_hardened_idx(84).expect("segwit");
    let n_path = match network {
        bitcoin::Network::Bitcoin => 0,
        _ => 1,
    };
    let network = ChildNumber::from_hardened_idx(n_path).expect("0 or 1");
    Ok(vec![script_path, network, account].into())
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

/// Creates a TR descriptor from the given extended public key (OXpub).
///
/// # Arguments
/// * `xpub` - An instance of `OXpub` representing the extended public key.
///
/// # Returns
/// A `Descriptor<DescriptorPublicKey>` that represents the wpkh descriptor.
pub fn tr(xpub: OXpub) -> Descriptor<DescriptorPublicKey> {
    let descr_str = format!(
        "tr([{}/{}]{}/<0;1>/*)",
        xpub.origin.0, xpub.origin.1, xpub.xkey
    );
    Descriptor::<DescriptorPublicKey>::from_str(&descr_str).expect("hardcoded descriptor")
}
