use miniscript::{
    bitcoin::{
        self, absolute,
        key::rand,
        psbt::{self},
        Psbt, ScriptBuf, TxIn, Witness,
    },
    psbt::PsbtExt,
    Descriptor, DescriptorPublicKey,
};
use serde::{Deserialize, Serialize};

use bitcoin::bip32::DerivationPath;

use crate::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// Signing-specific information for a coin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CoinSpendInfo {
    /// Descriptor-based coin (standard wallets)
    Bip32 {
        coin_path: (KeyChain, u32),
        descriptor: Descriptor<DescriptorPublicKey>,
        /// Ephemeral secret key for SP partial secret computation.
        /// Never persisted — only populated at tx-building time.
        #[serde(skip)]
        secret_key: Option<bitcoin::secp256k1::SecretKey>,
    },
    /// Silent Payment coin (BIP352)
    Sp {
        derivation: DerivationPath,
        tweak: [u8; 32],
    },
}

/// A spendable coin (UTXO) with all information needed for transaction building and signing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Coin {
    pub txout: bitcoin::TxOut,
    pub outpoint: bitcoin::OutPoint,
    pub height: Option<u64>,
    pub sequence: bitcoin::Sequence,
    pub status: CoinStatus,
    pub label: Option<Label>,
    pub satisfaction_size: u64,
    pub spend_info: CoinSpendInfo,
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

impl TryFrom<Coin> for bitcoin::psbt::Input {
    type Error = Error;

    fn try_from(coin: Coin) -> Result<Self, Self::Error> {
        coin.to_psbt_input()
    }
}

impl Coin {
    pub fn spk(&self) -> ScriptBuf {
        self.txout.script_pubkey.clone()
    }

    pub fn value(&self) -> bitcoin::Amount {
        self.txout.value
    }

    pub fn is_bip32(&self) -> bool {
        matches!(self.spend_info, CoinSpendInfo::Bip32 { .. })
    }

    pub fn is_sp(&self) -> bool {
        matches!(self.spend_info, CoinSpendInfo::Sp { .. })
    }

    pub fn to_psbt_input(&self) -> Result<psbt::Input, Error> {
        match &self.spend_info {
            CoinSpendInfo::Bip32 {
                coin_path,
                descriptor,
                ..
            } => self.spk_to_psbt_input(*coin_path, descriptor),
            CoinSpendInfo::Sp { .. } => self.sp_to_psbt_input(),
        }
    }

    fn spk_to_psbt_input(
        &self,
        coin_path: (KeyChain, u32),
        descriptor: &Descriptor<DescriptorPublicKey>,
    ) -> Result<psbt::Input, Error> {
        let inp = psbt::Input {
            witness_utxo: Some(self.txout.clone()),
            ..Default::default()
        };

        let (kc, index) = coin_path;

        let mut descriptors = descriptor
            .clone()
            .into_single_descriptors()
            .map_err(|_| Error::MultiDescriptor)?
            .into_iter();
        let recv = descriptors.next().ok_or(Error::MultiDescriptor)?;
        let change = descriptors.next().ok_or(Error::MultiDescriptor)?;

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
            sp_dleqs: Default::default(),
            sp_ecdh_shares: Default::default(),
            inputs: vec![inp],
            outputs: vec![],
        };
        PsbtExt::update_input_with_descriptor(&mut dummy_psbt, 0, &descr)
            .map_err(|_| Error::Update)?;

        Ok(dummy_psbt.inputs[0].clone())
    }

    fn sp_to_psbt_input(&self) -> Result<psbt::Input, Error> {
        // For SP coins, we create a basic PSBT input with witness_utxo
        // The actual signing will be handled by the SP signer which uses the tweak
        Ok(psbt::Input {
            witness_utxo: Some(self.txout.clone()),
            ..Default::default()
        })
    }
}

pub fn shuffle_coins(mut vec: Vec<Coin>) -> Vec<Coin> {
    use rand::seq::SliceRandom;
    vec.shuffle(&mut rand::thread_rng());
    vec
}
