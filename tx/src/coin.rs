use miniscript::{
    bitcoin::{
        self, absolute,
        key::rand,
        psbt::{self},
        Psbt, ScriptBuf, TxIn, Txid, Witness,
    },
    psbt::PsbtExt,
    Descriptor, DescriptorPublicKey,
};
use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyChain {
    Receive,
    Change,
    Custom(u32),
}

impl From<KeyChain> for u32 {
    fn from(value: KeyChain) -> Self {
        match value {
            KeyChain::Receive => 0,
            KeyChain::Change => 0,
            KeyChain::Custom(c) => c,
        }
    }
}

impl From<u32> for KeyChain {
    fn from(value: u32) -> Self {
        match value {
            0 => Self::Receive,
            1 => Self::Change,
            c => Self::Custom(c),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
pub enum CoinStatus {
    Unconfirmed,
    Confirmed,
    BeingSpend,
    Spent,
}

type Label = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Coin {
    pub txout: bitcoin::TxOut,
    pub outpoint: bitcoin::OutPoint,
    pub coin_path: (KeyChain, u32 /* index */),
    pub height: Option<u64>,
    pub sequence: bitcoin::Sequence,
    pub status: CoinStatus,
    pub label: Option<Label>,
    pub descriptor_fingerprint: crate::DescrFingerprint,
}

impl From<Coin> for bitcoin::TxIn {
    fn from(value: Coin) -> Self {
        TxIn {
            previous_output: value.outpoint,
            script_sig: ScriptBuf::new(),
            sequence: value.sequence,
            witness: Witness::new(),
        }
    }
}

impl Coin {
    pub fn spk(&self) -> ScriptBuf {
        self.txout.script_pubkey.clone()
    }

    pub fn to_psbt_input<D, T>(&self, descriptor: &D, tx: &T) -> Result<psbt::Input, Error>
    where
        D: Fn(crate::DescrFingerprint) -> Option<Descriptor<DescriptorPublicKey>>,
        T: Fn(Txid) -> Option<bitcoin::Transaction>,
    {
        let spk = self.spk();
        let is_segwit = spk.is_p2wsh() || spk.is_p2tr() || spk.is_p2wpkh();
        let txid = self.outpoint.txid;
        let funding_tx = tx(txid).ok_or(Error::NoFundingTx)?;
        let non_witness_utxo = funding_tx
            .output
            .get(self.outpoint.vout as usize)
            .cloned()
            .ok_or(Error::WrongVout)?;

        let inp = if is_segwit {
            psbt::Input {
                witness_utxo: Some(non_witness_utxo),
                ..Default::default()
            }
        } else {
            psbt::Input {
                non_witness_utxo: Some(funding_tx),
                ..Default::default()
            }
        };

        let fg = self.descriptor_fingerprint;
        let descr = descriptor(fg).ok_or(Error::NoDescriptor)?;
        let (kc, index) = self.coin_path;

        let descriptors = descr
            .into_single_descriptors()
            .map_err(|_| Error::MultiDescriptor)?;
        let recv = descriptors.first().ok_or(Error::MultiDescriptor)?;
        let change = descriptors.get(1).ok_or(Error::MultiDescriptor)?;

        let descr = match kc {
            KeyChain::Receive => recv
                .at_derivation_index(index)
                .map_err(|_| Error::Derivation)?,
            KeyChain::Change => change
                .at_derivation_index(index)
                .map_err(|_| Error::Derivation)?,

            KeyChain::Custom(_) => return Err(Error::KeyChain),
        };

        let dummy_tx = bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![self.clone().into()],
            output: vec![],
        };

        let mut dummy_psbt = Psbt {
            unsigned_tx: dummy_tx,
            version: 0,
            xpub: Default::default(),
            proprietary: Default::default(),
            unknown: Default::default(),
            inputs: vec![inp],
            outputs: vec![],
        };
        PsbtExt::update_input_with_descriptor(&mut dummy_psbt, 0, &descr)
            .map_err(|_| Error::Update)?;

        Ok(dummy_psbt.inputs[0].clone())
    }
}

pub fn shuffle_coins(mut vec: Vec<Coin>) -> Vec<Coin> {
    use rand::seq::SliceRandom;
    vec.shuffle(&mut rand::thread_rng());
    vec
}
