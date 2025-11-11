use miniscript::{
    bitcoin::{self, absolute, address::NetworkUnchecked, Address, TxOut},
    psbt::PsbtExt,
    Descriptor, DescriptorPublicKey,
};
use serde::{Deserialize, Serialize};

use crate::{coin::KeyChain, transaction::Amount, DescrFingerprint, Error};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipient {
    pub address: Address<NetworkUnchecked>,
    pub amount: Amount,
    pub label: Option<String>,
    pub origin: Option<(KeyChain, u32 /* index */)>,
    pub descriptor_fingerprint: Option<DescrFingerprint>,
}

impl From<Recipient> for TxOut {
    fn from(recip: Recipient) -> Self {
        let value = match recip.amount {
            Amount::Value(v) => bitcoin::Amount::from_sat(v),
            Amount::Max(value) => {
                match value {
                    Some(v) => bitcoin::Amount::from_sat(v),
                    // NOTE: vif value is None, that's mean simulation do
                    // NOT success, thus we put a dummy MAX_MONEY amount
                    // in order to be sure the transaction is not realistically
                    // spendable, but can be used for weight estimation
                    None => bitcoin::Amount::MAX_MONEY,
                }
            }
            Amount::Anchor => bitcoin::Amount::ZERO,
        };
        TxOut {
            value,
            script_pubkey: recip.address.assume_checked().script_pubkey(),
        }
    }
}

impl Recipient {
    pub fn to_psbt_output(
        &self,
        descriptor: &'static fn(DescrFingerprint) -> Option<Descriptor<DescriptorPublicKey>>,
    ) -> Result<bitcoin::psbt::Output, Error> {
        if let (Some(fg), Some((kc, index))) = (&self.descriptor_fingerprint, &self.origin) {
            let descr = descriptor(*fg).ok_or(Error::NoDescriptor)?;

            let descriptors = descr
                .into_single_descriptors()
                .map_err(|_| Error::MultiDescriptor)?;
            let recv = descriptors.first().ok_or(Error::MultiDescriptor)?;
            let change = descriptors.get(1).ok_or(Error::MultiDescriptor)?;

            let descr = match kc {
                KeyChain::Receive => recv
                    .at_derivation_index(*index)
                    .map_err(|_| Error::Derivation)?,
                KeyChain::Change => change
                    .at_derivation_index(*index)
                    .map_err(|_| Error::Derivation)?,

                KeyChain::Custom(_) => return Err(Error::KeyChain),
            };

            let dummy_tx = bitcoin::Transaction {
                version: bitcoin::transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![],
                output: vec![self.clone().into()],
            };

            let mut dummy_psbt = bitcoin::Psbt {
                unsigned_tx: dummy_tx,
                version: 0,
                xpub: Default::default(),
                proprietary: Default::default(),
                unknown: Default::default(),
                inputs: Default::default(),
                outputs: vec![bitcoin::psbt::Output::default()],
            };

            PsbtExt::update_output_with_descriptor(&mut dummy_psbt, 0, &descr)
                .map_err(|_| Error::Update)?;

            Ok(dummy_psbt.outputs[0].clone())
        } else {
            Ok(bitcoin::psbt::Output::default())
        }
    }
}
