use crate::{
    transaction::{finalize_transaction, process_transaction, Error},
    Coin, Fees, Recipient, TransactionResult, TxTemplate,
};
use bitcoin::Psbt;
use bwk_descriptor::SpkDerivator;
use miniscript::{Descriptor, DescriptorPublicKey};

#[cfg(test)]
use {
    bitcoin::{bip32::ChildNumber, Network},
    bwk_descriptor::tr_path,
    bwk_sign::HotSigner,
    bwk_utils::test::{random_input, random_output, txid},
    miniscript::bitcoin::{
        self,
        key::rand::{self, random, Rng},
        Amount, OutPoint, ScriptBuf, Sequence, TxOut,
    },
    std::collections::BTreeMap,
};

pub trait ChangeTip {
    fn next_index(&mut self) -> u32;
}

pub trait CoinSource {
    fn spendable_coins(&self) -> Vec<Coin>;
    #[cfg(test)]
    fn add_coin(&mut self, coin: Coin);
    #[cfg(test)]
    fn remove_coin(&mut self, coin: &Coin);
}

#[cfg(test)]
impl CoinSource for BTreeMap<OutPoint, Coin> {
    fn spendable_coins(&self) -> Vec<Coin> {
        self.values().cloned().collect()
    }

    fn add_coin(&mut self, coin: Coin) {
        self.insert(coin.outpoint, coin);
    }

    fn remove_coin(&mut self, coin: &Coin) {
        self.remove(&coin.outpoint);
    }
}

pub enum ChangeTipHandle {
    Internal(u32),
    External(Box<dyn ChangeTip>),
    None,
}

pub struct TxBuilder {
    derivator: SpkDerivator,
    change_tip: ChangeTipHandle,
    tx_template: TxTemplate,
    coin_source: Option<Box<dyn CoinSource>>,
}

impl TxBuilder {
    pub fn new(
        descriptor: Descriptor<DescriptorPublicKey>,
        tip_handle: Box<dyn ChangeTip>,
        network: bitcoin::Network,
    ) -> Result<Self, Error> {
        Ok(TxBuilder {
            derivator: SpkDerivator::new(descriptor.clone(), network)
                .map_err(|_| Error::Derivator)?,
            change_tip: ChangeTipHandle::External(tip_handle),
            tx_template: TxTemplate {
                inputs: vec![],
                outputs: vec![],
                fees: crate::Fees::MilliSatsVb(1_000),
                change_descriptor: descriptor.clone(),
            },
            #[cfg(not(test))]
            coin_source: None,
            #[cfg(test)]
            coin_source: Some(Box::new(BTreeMap::new())),
        })
    }
    pub fn new_standalone(
        descriptor: Descriptor<DescriptorPublicKey>,
        network: bitcoin::Network,
    ) -> Result<Self, Error> {
        Self::new_standalone_with_tip(descriptor, network, 0)
    }
    pub fn new_standalone_with_tip(
        descriptor: Descriptor<DescriptorPublicKey>,
        network: bitcoin::Network,
        tip: u32,
    ) -> Result<Self, Error> {
        Ok(TxBuilder {
            derivator: SpkDerivator::new(descriptor.clone(), network)
                .map_err(|_| Error::Descriptor)?,
            change_tip: ChangeTipHandle::Internal(tip),
            tx_template: TxTemplate {
                inputs: vec![],
                outputs: vec![],
                fees: crate::Fees::MilliSatsVb(1_000),
                change_descriptor: descriptor.clone(),
            },
            #[cfg(not(test))]
            coin_source: None,
            #[cfg(test)]
            coin_source: Some(Box::new(BTreeMap::new())),
        })
    }
    pub fn coin_source(mut self, coin_source: Box<dyn CoinSource>) -> Self {
        self.coin_source = Some(coin_source);
        self
    }
    /// Set an absolute fee value
    pub fn fee(mut self, fee: u64) -> Self {
        self.tx_template.fees = Fees::Sats(fee);
        self
    }
    /// Set a feerate in millisats/vb
    pub fn feerate(mut self, feerate: u64) -> Self {
        self.tx_template.fees = Fees::MilliSatsVb(feerate);
        self
    }
    /// Replace current template inputs by new set of inputs
    pub fn inputs(mut self, inputs: Vec<Coin>) -> Self {
        self.tx_template.inputs = inputs;
        self
    }
    /// Add input to the set of inputs
    pub fn input(mut self, coin: Coin) -> Self {
        if !self.tx_template.inputs.contains(&coin) {
            self.tx_template.inputs.push(coin);
        }
        self
    }
    /// Replace current template outputs by new set of outputs
    pub fn outputs(mut self, recipients: Vec<Recipient>) -> Self {
        self.tx_template.outputs = recipients;
        self
    }
    /// Add output to the set of outputs
    pub fn output(mut self, recipient: Recipient) -> Self {
        if !self.tx_template.outputs.contains(&recipient) {
            self.tx_template.outputs.push(recipient);
        }
        self
    }
    /// Initialise a new template
    pub fn new_template(&mut self) {
        self.tx_template = TxTemplate {
            inputs: vec![],
            outputs: vec![],
            fees: crate::Fees::MilliSatsVb(1_000),
            change_descriptor: self.tx_template.change_descriptor.clone(),
        };
    }
    /// Send <amount> to <address>
    pub fn send_to(&mut self, address: bitcoin::Address, amount: u64) {
        let recipient = Recipient {
            address: address.as_unchecked().clone(),
            amount: crate::Amount::Value(amount),
            label: None,
            origin: None,
            descriptor: None,
        };
        self.tx_template.outputs.push(recipient);
    }
    /// Send <amount> to <address> w/ <label>
    pub fn send_to_with_label<T>(&mut self, address: bitcoin::Address, amount: u64, label: T)
    where
        T: Into<String>,
    {
        let label = Some(label.into());
        let recipient = Recipient {
            address: address.as_unchecked().clone(),
            amount: crate::Amount::Value(amount),
            label,
            origin: None,
            descriptor: None,
        };
        self.tx_template.outputs.push(recipient);
    }
    /// Try to craft a transaction from the current TxTemplate and output a TransactionResult
    pub fn simulate(&self) -> TransactionResult {
        process_transaction(self.tx_template.clone(), &self.derivator.descriptor())
    }
    /// Increment and return a new change index
    pub fn new_change_index(&mut self) -> u32 {
        match &mut self.change_tip {
            ChangeTipHandle::Internal(v) => {
                *v += 1;
                *v
            }
            ChangeTipHandle::External(tip) => tip.next_index(),
            ChangeTipHandle::None => unreachable!(),
        }
    }
    /// Generate a signable PSBT from the current TxTemplate
    pub fn generate(&mut self) -> Result<Psbt, Error> {
        let descriptor = self.derivator.descriptor();
        let res = process_transaction(self.tx_template.clone(), &descriptor);
        finalize_transaction(res, &mut (|| self.new_change_index()), descriptor, true)
    }
    #[cfg(test)]
    pub fn mark_tx_mined(&mut self) {
        use crate::transaction::max_input_satisfaction_size;

        if let Some(source) = self.coin_source.as_mut() {
            for coin in &self.tx_template.inputs {
                source.remove_coin(coin);
            }
        }
        if let Some(source) = self.coin_source.as_mut() {
            let txid = self.tx_template.tx().compute_txid();
            for (pos, recipient) in self.tx_template.outputs.iter().enumerate() {
                if let (Some(origin), Some(descriptor)) = (&recipient.origin, &recipient.descriptor)
                {
                    let this_descriptor = self.derivator.descriptor();
                    if descriptor == &this_descriptor {
                        let coin = Coin {
                            txout: recipient.into(),
                            outpoint: OutPoint {
                                txid,
                                vout: pos as u32,
                            },
                            coin_path: *origin,
                            height: Some(0),
                            sequence: Sequence::ZERO,
                            status: crate::CoinStatus::Confirmed,
                            label: recipient.label.clone(),
                            descriptor: descriptor.clone(),
                            satisfaction_size: max_input_satisfaction_size(descriptor) as u64,
                        };
                        source.add_coin(coin);
                    }
                }
            }
        }
    }
    #[cfg(test)]
    pub fn receive_coin(&mut self, coin: Coin) {
        if coin.descriptor != self.derivator.descriptor() {
            return;
        }
        if let Some(source) = self.coin_source.as_mut() {
            source.add_coin(coin);
        }
    }
    #[cfg(test)]
    pub fn funding_input(&mut self, amount: u64) {
        let coin = receive_coin(amount, &self.derivator, random());
        self.tx_template.inputs.push(coin);
    }
    #[cfg(test)]
    pub fn dummy_external_output(&mut self, amount: u64) {
        let recipient = external_recipient(amount);
        self.tx_template.outputs.push(recipient);
    }
    #[cfg(test)]
    pub fn self_output(&mut self, amount: u64) {
        use crate::coin::KeyChain;
        let index = random();
        let addr = self.derivator.receive_at(index);
        let recipient = Recipient {
            address: addr.as_unchecked().clone(),
            amount: crate::Amount::Value(amount),
            label: None,
            origin: Some((KeyChain::Receive, index)),
            descriptor: Some(self.derivator.descriptor()),
        };
        self.tx_template.outputs.push(recipient);
    }
}

#[cfg(test)]
fn receive_coin(amount: u64, derivator: &SpkDerivator, index: u32) -> Coin {
    use crate::{coin::KeyChain, transaction::max_input_satisfaction_size, CoinStatus};

    let spk = derivator.receive_at(index).script_pubkey();
    let txout = TxOut {
        value: Amount::from_sat(amount),
        script_pubkey: spk.clone(),
    };
    let vout: u8 = random();
    let outpoint = OutPoint {
        txid: txid(),
        vout: vout as u32,
    };
    let descriptor = derivator.descriptor();
    let satisfaction = max_input_satisfaction_size(&descriptor);
    Coin {
        txout,
        outpoint,
        coin_path: (KeyChain::Receive, index),
        height: None,
        sequence: Sequence::ZERO,
        status: CoinStatus::Unconfirmed,
        label: None,
        descriptor,
        satisfaction_size: satisfaction as u64,
    }
}

#[cfg(test)]
pub fn tr_signer() -> (HotSigner, SpkDerivator) {
    let nw = Network::Regtest;
    let signer = HotSigner::new(nw).unwrap();
    let path = tr_path(nw, ChildNumber::from_hardened_idx(0).unwrap()).unwrap();
    let xpub = signer.xpub(&path);
    let derivator = SpkDerivator::new_tr(xpub, nw).unwrap();
    (signer, derivator)
}

#[cfg(test)]
pub fn sum_inputs(psbt: &Psbt) -> u64 {
    psbt.inputs.iter().enumerate().fold(0, |sum, (pos, i)| {
        if let Some(txout) = &i.witness_utxo {
            sum + txout.value.to_sat()
        } else if let Some(tx) = &i.non_witness_utxo {
            sum + tx.output[pos].value.to_sat()
        } else {
            panic!("missing amount")
        }
    })
}

#[cfg(test)]
pub fn sum_outputs(psbt: &Psbt) -> u64 {
    psbt.unsigned_tx
        .output
        .iter()
        .fold(0, |sum, o| sum + o.value.to_sat())
}

#[cfg(test)]
pub fn self_recipient(amount: u64, derivator: &SpkDerivator, index: u32) -> Recipient {
    use crate::coin::KeyChain;

    let address = derivator.receive_at(index).as_unchecked().clone();
    Recipient {
        address,
        amount: super::Amount::Value(amount),
        label: None,
        origin: Some((KeyChain::Receive, index)),
        descriptor: Some(derivator.descriptor()),
    }
}

#[cfg(test)]
pub fn external_recipient(amount: u64) -> Recipient {
    let (_signer, derivator) = tr_signer();
    let index: u16 = random();
    let address = derivator.receive_at(index as u32).as_unchecked().clone();
    Recipient {
        address,
        amount: super::Amount::Value(amount),
        label: None,
        origin: None,
        descriptor: None,
    }
}

#[cfg(test)]
pub fn funding_coin(amount: u64, derivator: &SpkDerivator, index: u32) -> Coin {
    use crate::{coin::KeyChain, transaction::max_input_satisfaction_size, CoinStatus};

    let spk = derivator.receive_at(index).script_pubkey();
    let descriptor = derivator.descriptor();
    let (tx, pos) = funding_tx(spk, (amount as f64) / 100_000_000.0);
    let txid = tx.compute_txid();

    let txout = tx.output[pos].clone();
    let outpoint = OutPoint {
        txid,
        vout: (pos as u32),
    };

    let satisfaction = max_input_satisfaction_size(&descriptor);

    Coin {
        txout,
        outpoint,
        coin_path: (KeyChain::Receive, index),
        height: None,
        sequence: Sequence::ZERO,
        status: CoinStatus::Unconfirmed,
        label: None,
        descriptor,
        satisfaction_size: satisfaction as u64,
    }
}

#[cfg(test)]
pub fn funding_tx(spk: ScriptBuf, amount: f64) -> (bitcoin::Transaction, usize /* position */) {
    let num_inputs = rand::thread_rng().gen_range(1..10);
    let num_outputs: usize = rand::thread_rng().gen_range(1..5);

    let mut inserted = false;
    let mut pos = 0;
    let mut input = vec![];
    let mut output = vec![];

    for _ in 0..num_inputs {
        input.push(random_input());
    }

    for p in 0..num_outputs {
        let r = rand::thread_rng().gen_range(1..5);
        if (r == 0 || p == (num_outputs.saturating_sub(1))) && !inserted {
            output.push(TxOut {
                value: Amount::from_btc(amount).unwrap(),
                script_pubkey: spk.clone(),
            });
            pos = p;
            inserted = true;
        } else {
            output.push(random_output());
        }
    }

    (
        bitcoin::Transaction {
            version: bitcoin::transaction::Version(2),
            lock_time: bitcoin::absolute::LockTime::Blocks(bitcoin::absolute::Height::ZERO),
            input,
            output,
        },
        pos,
    )
}
