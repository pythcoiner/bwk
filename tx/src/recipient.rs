use miniscript::{
    bitcoin::{self, absolute, address::NetworkUnchecked, key::rand, Address, TxOut},
    psbt::PsbtExt,
    Descriptor, DescriptorPublicKey,
};
use serde::{Deserialize, Serialize};

use crate::{coin::KeyChain, transaction::Amount, Error};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Recipient {
    pub address: Address<NetworkUnchecked>,
    pub amount: Amount,
    pub label: Option<String>,
    pub origin: Option<(KeyChain, u32 /* index */)>,
    pub descriptor: Option<Descriptor<DescriptorPublicKey>>,
}

impl From<&Recipient> for TxOut {
    fn from(recip: &Recipient) -> Self {
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
            script_pubkey: recip.address.clone().assume_checked().script_pubkey(),
        }
    }
}

impl From<Recipient> for TxOut {
    fn from(value: Recipient) -> Self {
        (&value).into()
    }
}

impl TryFrom<Recipient> for bitcoin::psbt::Output {
    type Error = Error;

    fn try_from(recip: Recipient) -> Result<Self, Self::Error> {
        recip.to_psbt_output()
    }
}

impl Recipient {
    pub fn to_psbt_output(&self) -> Result<bitcoin::psbt::Output, Error> {
        if let (Some(descr), Some((kc, index))) = (&self.descriptor, &self.origin) {
            let descriptors = descr
                .clone()
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

pub fn shuffle_recipients(mut vec: Vec<Recipient>) -> Vec<Recipient> {
    use rand::seq::SliceRandom;
    vec.shuffle(&mut rand::thread_rng());
    vec
}
