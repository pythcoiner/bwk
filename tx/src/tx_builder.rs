use crate::{
    coin_selection::{CoinSelector, DefaultCoinSelector},
    recipient::SpPartialSecretProvider,
    transaction::{process_transaction, tx_estimated_weight, Amount, Error},
    Coin, Fees, Recipient, RecipientProvider, TransactionResult, TxTemplate,
};
use bitcoin::Psbt;

#[cfg(feature = "test")]
use {
    crate::{coin::KeyChain, FinalizationContext, PsbtOutputInfo, Warning},
    bitcoin::bip32::ChildNumber,
    bitcoin::Network,
    bwk_descriptor::{tr_path, wpkh_path, SpkDerivator},
    bwk_sign::HotSigner,
    bwk_sign::Signer,
    bwk_utils::test::{
        corepc_node, generate_blocks, get_tx, get_tx_height, random_input, random_output, txid,
    },
    miniscript::bitcoin::{
        self,
        key::rand::{self, random, Rng},
        OutPoint, ScriptBuf, Sequence, TxOut,
    },
    miniscript::{Descriptor, DescriptorPublicKey},
    std::collections::BTreeMap,
};

/// Trait for managing change address index tip.
/// Implemented by wallet types that need to track the next change index.
/// Called by RecipientProvider implementations when deriving new change addresses.
pub trait ChangeTip {
    fn next_index(&mut self) -> u32;
}

pub trait CoinSource {
    fn spendable_coins(&self) -> Vec<Coin>;
    #[cfg(feature = "test")]
    fn add_coin(&mut self, _coin: Coin) {}
    #[cfg(feature = "test")]
    fn remove_coin(&mut self, _coin: &Coin) {}
}

#[cfg(feature = "test")]
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

pub struct TxBuilder {
    change_provider: Box<dyn RecipientProvider>,
    pub tx_template: TxTemplate,
    coin_source: Option<Box<dyn CoinSource>>,
    sp_provider: Option<Box<dyn SpPartialSecretProvider>>,
    coin_selector: Box<dyn CoinSelector>,
    max_fee_percent: u8,
    max_fee_amount: u64,
    #[cfg(feature = "test")]
    derivator: Option<SpkDerivator>,
}

impl TxBuilder {
    pub fn new(change_provider: Box<dyn RecipientProvider>) -> Self {
        TxBuilder {
            change_provider,
            tx_template: TxTemplate {
                inputs: vec![],
                outputs: vec![],
                fees: crate::Fees::MilliSatsVb(1_000),
            },
            coin_source: None,
            sp_provider: None,
            coin_selector: Box::new(DefaultCoinSelector::default()),
            max_fee_percent: 10,
            max_fee_amount: 2_000_000,
            #[cfg(feature = "test")]
            derivator: None,
        }
    }

    #[cfg(feature = "test")]
    pub fn new_with_derivator(
        change_provider: Box<dyn RecipientProvider>,
        derivator: SpkDerivator,
    ) -> Self {
        TxBuilder {
            change_provider,
            tx_template: TxTemplate {
                inputs: vec![],
                outputs: vec![],
                fees: crate::Fees::MilliSatsVb(1_000),
            },
            coin_source: Some(Box::new(BTreeMap::new())),
            sp_provider: None,
            coin_selector: Box::new(DefaultCoinSelector::default()),
            max_fee_percent: 10,
            max_fee_amount: 2_000_000,
            derivator: Some(derivator),
        }
    }
    pub fn network(&self) -> bitcoin::Network {
        self.change_provider.network()
    }
    pub fn coin_source(mut self, coin_source: Box<dyn CoinSource>) -> Self {
        self.coin_source = Some(coin_source);
        self
    }
    pub fn sp_provider(mut self, provider: Box<dyn SpPartialSecretProvider>) -> Self {
        self.sp_provider = Some(provider);
        self
    }
    pub fn coin_selector(mut self, coin_selector: Box<dyn CoinSelector>) -> Self {
        self.coin_selector = coin_selector;
        self
    }
    pub fn max_fee_percent(mut self, pct: u8) -> Self {
        self.max_fee_percent = pct;
        self
    }
    pub fn max_fee_amount(mut self, sats: u64) -> Self {
        self.max_fee_amount = sats;
        self
    }
    pub fn fee(mut self, fee: u64) -> Self {
        self.tx_template.fees = Fees::Sats(fee);
        self
    }
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
    pub fn add_input(&mut self, coin: Coin) {
        if !self.tx_template.inputs.contains(&coin) {
            self.tx_template.inputs.push(coin);
        }
    }
    /// Add all spendable coins from the coin source as inputs (for drain)
    pub fn drain_inputs(&mut self) {
        if let Some(source) = &self.coin_source {
            for coin in source.spendable_coins() {
                self.add_input(coin);
            }
        }
    }
    /// Replace current template outputs by new set of outputs
    pub fn outputs(mut self, recipients: Vec<Box<dyn RecipientProvider>>) -> Self {
        self.tx_template.outputs = recipients;
        self
    }
    /// Add output to the set of outputs
    pub fn add_output(&mut self, recipient: impl RecipientProvider + 'static) {
        self.tx_template.outputs.push(Box::new(recipient));
    }
    /// Initialise a new template
    pub fn new_template(&mut self) {
        self.tx_template = TxTemplate {
            inputs: vec![],
            outputs: vec![],
            fees: crate::Fees::MilliSatsVb(1_000),
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
        self.tx_template.outputs.push(Box::new(recipient));
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
        self.tx_template.outputs.push(Box::new(recipient));
    }
    /// Try to craft a transaction from the current TxTemplate and output a TransactionResult
    pub fn simulate(&self) -> TransactionResult {
        process_transaction(
            self.tx_template.clone(),
            Some(self.change_provider.as_ref()),
        )
    }

    /// Generate a signable PSBT from the current TxTemplate
    pub fn generate(&mut self) -> Result<Psbt, Error> {
        let res = process_transaction(
            self.tx_template.clone(),
            Some(self.change_provider.as_ref()),
        );

        if let Some(error) = res.error {
            return Err(error);
        }

        let change_recip: Option<Box<dyn RecipientProvider>> =
            if let Some(change_value) = res.change {
                let mut change = self.change_provider.clone_box();
                change.set_amount(Amount::Value(change_value.to_sat()));
                Some(change)
            } else {
                None
            };

        res.tx_template.finalize(
            change_recip,
            true,
            self.sp_provider.as_deref(),
            self.change_provider.network(),
            self.max_fee_percent,
            self.max_fee_amount,
            false,
        )
    }
    pub fn select_coins(&self, target: u64, feerate: u64 /* msats/vb */) -> Vec<Coin> {
        // NOTE: target must contains fees for base tx + outputs
        if let Some(source) = &self.coin_source {
            let coins = source.spendable_coins();
            self.coin_selector.select_coins(coins, target, feerate)
        } else {
            vec![]
        }
    }
    pub fn pay_with_label(
        &mut self,
        amount: u64,
        address: bitcoin::Address,
        feerate: u64, /* msats/vb */
        label: Option<String>,
    ) -> Result<bitcoin::Psbt, Error> {
        self.new_template();
        let recipient = Recipient {
            address: address.as_unchecked().clone(),
            amount: crate::Amount::Value(amount),
            label,
            origin: None,
            descriptor: None,
        };
        self.add_output(recipient);
        let base_fee = tx_estimated_weight(&self.tx_template).to_vbytes_ceil() * feerate / 1000;
        let coins = self.select_coins(amount + base_fee, feerate);
        if coins.is_empty() {
            return Err(Error::CoinSelection);
        }
        for c in coins {
            self.add_input(c);
        }
        let res = self.simulate();
        let psbt = self.generate()?;
        let tx_weight = psbt.unsigned_tx.weight().to_vbytes_ceil();
        let min_fee = tx_weight * feerate / 1000;
        if let Some(fees) = res.fees {
            if min_fee > fees.to_sat() {
                Err(Error::CoinSelectionFee)
            } else {
                Ok(psbt)
            }
        } else {
            Err(Error::CoinSelectionFee)
        }
    }
    pub fn pay(
        &mut self,
        amount: u64,
        address: bitcoin::Address,
        feerate: u64, /* msats/vb */
    ) -> Result<bitcoin::Psbt, Error> {
        self.pay_with_label(amount, address, feerate, None)
    }
}

#[cfg(feature = "test")]
impl TxBuilder {
    fn derivator(&self) -> &SpkDerivator {
        self.derivator
            .as_ref()
            .expect("derivator required for this test method")
    }

    pub fn mark_tx_mined(&mut self) {
        use crate::transaction::max_input_satisfaction_size;

        let this_descriptor = self.derivator().descriptor();

        if let Some(source) = self.coin_source.as_mut() {
            for coin in &self.tx_template.inputs {
                source.remove_coin(coin);
            }
        }
        if let Some(source) = self.coin_source.as_mut() {
            let txid = self.tx_template.tx().compute_txid();
            for (pos, recipient) in self.tx_template.outputs.iter_mut().enumerate() {
                // Check if this output is a BIP32 change output belonging to this wallet
                if let PsbtOutputInfo::Bip32 { origin, descriptor } = recipient.psbt_output_info() {
                    if descriptor == this_descriptor {
                        // Create TxOut from recipient
                        let value = match recipient.amount() {
                            crate::Amount::Value(v) => bitcoin::Amount::from_sat(v),
                            crate::Amount::Max(Some(v)) => bitcoin::Amount::from_sat(v),
                            crate::Amount::Max(None) | crate::Amount::Anchor => continue,
                        };
                        let txout = bitcoin::TxOut {
                            value,
                            script_pubkey: recipient.create_script(&FinalizationContext {
                                inputs: &self.tx_template.inputs,
                                partial_secret: None,
                                network: bitcoin::Network::Bitcoin,
                            }),
                        };
                        let coin = Coin {
                            txout,
                            outpoint: OutPoint {
                                txid,
                                vout: pos as u32,
                            },
                            height: Some(0),
                            sequence: Sequence::ZERO,
                            status: crate::CoinStatus::Confirmed,
                            label: None,
                            satisfaction_size: max_input_satisfaction_size(&descriptor) as u64,
                            spend_info: crate::CoinSpendInfo::Bip32 {
                                coin_path: origin,
                                descriptor,
                            },
                        };
                        source.add_coin(coin);
                    }
                }
            }
        }
    }
    pub fn receive_coin(&mut self, coin: Coin) {
        let this_descriptor = self.derivator().descriptor();
        let matches = match &coin.spend_info {
            crate::CoinSpendInfo::Bip32 { descriptor, .. } => *descriptor == this_descriptor,
            crate::CoinSpendInfo::Sp { .. } => false,
        };
        if !matches {
            return;
        }
        if let Some(source) = self.coin_source.as_mut() {
            source.add_coin(coin);
        }
    }
    pub fn funding_input(&mut self, amount: u64) {
        let index: u8 = random();
        let coin = test::receive_coin(amount, self.derivator(), index as u32);
        self.tx_template.inputs.push(coin);
    }
    pub fn self_recipient(&mut self, amount: u64) -> Recipient {
        let index: u8 = random();
        test::self_recipient(amount, self.derivator(), index as u32)
    }
    pub fn dummy_external_output(&mut self, amount: u64) {
        let recipient = test::external_recipient(amount);
        self.tx_template.outputs.push(Box::new(recipient));
    }
    pub fn dummy_external_output_max(&mut self) {
        let recipient = test::external_recipient_max();
        self.tx_template.outputs.push(Box::new(recipient));
    }
    pub fn self_output(&mut self, amount: u64) {
        let index: u8 = random();
        let addr = self.derivator().receive_at(index as u32);
        let recipient = Recipient {
            address: addr.as_unchecked().clone(),
            amount: crate::Amount::Value(amount),
            label: None,
            origin: Some((KeyChain::Receive, index as u32)),
            descriptor: Some(self.derivator().descriptor()),
        };
        self.tx_template.outputs.push(Box::new(recipient));
    }
    pub fn receive_address_at(&self, index: u32) -> bitcoin::Address {
        self.derivator().receive_at(index)
    }
    pub fn fund_with_bitcoind(&mut self, bitcoind: &mut corepc_node::Client, amount: u64) -> Coin {
        use crate::transaction::max_input_satisfaction_size;

        let index: u8 = random();
        let addr = self.receive_address_at(index as u32);
        let txid = bitcoind
            .send_to_address(&addr, bitcoin::Amount::from_sat(amount))
            .unwrap()
            .txid()
            .unwrap();
        generate_blocks(bitcoind, 3);
        let height = get_tx_height(bitcoind, txid).unwrap();
        let tx = get_tx(bitcoind, txid).unwrap();
        let mut vout = None;
        for (pos, txout) in tx.output.iter().enumerate() {
            if txout.script_pubkey == addr.script_pubkey() {
                vout = Some((pos, txout.clone()));
                break;
            }
        }
        let (vout, txout) = vout.unwrap();
        let descriptor = self.derivator().descriptor();
        let satisfaction = max_input_satisfaction_size(&descriptor);
        let coin = Coin {
            txout,
            outpoint: OutPoint {
                txid,
                vout: vout as u32,
            },
            height: Some(height),
            sequence: Sequence::ZERO,
            status: crate::CoinStatus::Confirmed,
            label: None,
            satisfaction_size: satisfaction as u64,
            spend_info: crate::CoinSpendInfo::Bip32 {
                coin_path: (KeyChain::Receive, index as u32),
                descriptor: self.derivator().descriptor(),
            },
        };
        self.receive_coin(coin.clone());
        coin
    }
}

#[cfg(feature = "test")]
pub mod test {
    use super::*;
    use crate::Amount;

    /// Create a TxBuilder from a derivator (for tests)
    pub fn builder_from_derivator(derivator: SpkDerivator) -> TxBuilder {
        let descriptor = derivator.descriptor();
        let network = {
            use miniscript::ForEachKey;
            let is_mainnet = descriptor
                .clone()
                .into_single_descriptors()
                .expect("multipath")
                .get(1)
                .expect("multipath")
                .for_any_key(|k| match k {
                    DescriptorPublicKey::XPub(k) => k.xkey.network.is_mainnet(),
                    _ => false,
                });
            if is_mainnet {
                Network::Bitcoin
            } else {
                Network::Regtest
            }
        };
        let address = descriptor
            .clone()
            .into_single_descriptors()
            .expect("multipath")
            .get(1)
            .expect("multipath")
            .at_derivation_index(0)
            .expect("derivation")
            .address(network)
            .expect("address");
        let change_provider = Box::new(Recipient {
            address: address.as_unchecked().clone(),
            amount: Amount::Value(0),
            label: None,
            origin: Some((KeyChain::Change, 0)),
            descriptor: Some(descriptor),
        });
        TxBuilder::new_with_derivator(change_provider, derivator)
    }

    pub fn receive_coin(amount: u64, derivator: &SpkDerivator, index: u32) -> Coin {
        use crate::{
            coin::KeyChain, transaction::max_input_satisfaction_size, CoinSpendInfo, CoinStatus,
        };

        let spk = derivator.receive_at(index).script_pubkey();
        let txout = TxOut {
            value: bitcoin::Amount::from_sat(amount),
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
            height: None,
            sequence: Sequence::ZERO,
            status: CoinStatus::Unconfirmed,
            label: None,
            satisfaction_size: satisfaction as u64,
            spend_info: CoinSpendInfo::Bip32 {
                coin_path: (KeyChain::Receive, index),
                descriptor,
            },
        }
    }

    pub fn tr_signer() -> (HotSigner, SpkDerivator) {
        let nw = Network::Regtest;
        let mut signer = HotSigner::new(nw).unwrap();
        let path = tr_path(nw, ChildNumber::from_hardened_idx(0).unwrap()).unwrap();
        let xpub = signer.xpub(&path);
        let derivator = SpkDerivator::new_tr(xpub, nw).unwrap();
        signer.register_descriptor(derivator.descriptor());
        (signer, derivator)
    }

    pub fn taptree_signer() -> (HotSigner, SpkDerivator) {
        use std::str::FromStr;

        let nw = Network::Regtest;
        let not_signer = HotSigner::new(nw).unwrap();
        let mut signer = HotSigner::new(nw).unwrap();
        let path = tr_path(nw, ChildNumber::from_hardened_idx(0).unwrap()).unwrap();
        let xpub = signer.xpub(&path);
        let not_xpub = not_signer.xpub(&path);

        let descr_str = format!(
            "tr([{}/{}]{}/<0;1>/*,pk([{}/{}]{}/<0;1>/*))",
            not_xpub.origin.0,
            not_xpub.origin.1,
            not_xpub.xkey,
            xpub.origin.0,
            xpub.origin.1,
            xpub.xkey
        );
        let desccriptor =
            Descriptor::<DescriptorPublicKey>::from_str(&descr_str).expect("hardcoded descriptor");
        let derivator = SpkDerivator::new(desccriptor, nw).unwrap();
        signer.register_descriptor(derivator.descriptor());
        (signer, derivator)
    }

    pub fn wpkh_signer() -> (HotSigner, SpkDerivator) {
        let nw = Network::Regtest;
        let mut signer = HotSigner::new(nw).unwrap();
        let path = wpkh_path(nw, ChildNumber::from_hardened_idx(0).unwrap()).unwrap();
        let xpub = signer.xpub(&path);
        let derivator = SpkDerivator::new_wpkh(xpub, nw).unwrap();
        signer.register_descriptor(derivator.descriptor());
        (signer, derivator)
    }

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

    pub fn sum_outputs(psbt: &Psbt) -> u64 {
        psbt.unsigned_tx
            .output
            .iter()
            .fold(0, |sum, o| sum + o.value.to_sat())
    }

    pub fn self_recipient(amount: u64, derivator: &SpkDerivator, index: u32) -> Recipient {
        use crate::coin::KeyChain;

        let address = derivator.receive_at(index).as_unchecked().clone();
        Recipient {
            address,
            amount: Amount::Value(amount),
            label: None,
            origin: Some((KeyChain::Receive, index)),
            descriptor: Some(derivator.descriptor()),
        }
    }

    pub fn external_recipient(amount: u64) -> Recipient {
        let (_signer, derivator) = tr_signer();
        let index: u16 = random();

        let address = derivator.receive_at(index as u32).as_unchecked().clone();
        Recipient {
            address,
            amount: Amount::Value(amount),
            label: None,
            origin: None,
            descriptor: None,
        }
    }

    pub fn external_recipient_max() -> Recipient {
        let (_signer, derivator) = tr_signer();
        let index: u16 = random();
        let address = derivator.receive_at(index as u32).as_unchecked().clone();
        Recipient {
            address,
            amount: Amount::Max(None),
            label: None,
            origin: None,
            descriptor: None,
        }
    }

    pub fn funding_coin(amount: u64, derivator: &SpkDerivator, index: u32) -> Coin {
        use crate::{
            coin::KeyChain, transaction::max_input_satisfaction_size, CoinSpendInfo, CoinStatus,
        };

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
            height: None,
            sequence: Sequence::ZERO,
            status: CoinStatus::Unconfirmed,
            label: None,
            satisfaction_size: satisfaction as u64,
            spend_info: CoinSpendInfo::Bip32 {
                coin_path: (KeyChain::Receive, index),
                descriptor,
            },
        }
    }

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
                    value: bitcoin::Amount::from_btc(amount).unwrap(),
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

    pub fn generate_sign_broadcast(
        builder: &mut TxBuilder,
        signer: &HotSigner,
        bitcoind: &mut corepc_node::Client,
        fee: u64,
        change: u64,
        warnings: &[Warning],
        tx_size: u64,
    ) {
        let change = (change > 0).then_some(bitcoin::Amount::from_sat(change));
        let res = builder.simulate();
        assert_eq!(res.fees, Some(bitcoin::Amount::from_sat(fee)));
        assert_eq!(res.change, change);
        assert_eq!(res.warnings, warnings.to_vec());
        assert!(res.error.is_none());
        let mut psbt = builder.generate().unwrap();
        signer.sign(&mut psbt);
        let tx = signer.finalize(&mut psbt).unwrap();
        let size = tx.weight().to_vbytes_ceil();
        assert_eq!(size, tx_size);
        let _txid = bitcoind.send_raw_transaction(&tx).unwrap().txid().unwrap();
    }
}

#[cfg(all(test, feature = "test"))]
mod tests {
    use super::*;
    use crate::tx_builder::test::generate_sign_broadcast;
    use crate::tx_builder::test::taptree_signer;
    use crate::tx_builder::test::tr_signer;
    use crate::tx_builder::test::wpkh_signer;
    use bwk_utils::test::bitcoind_with_txindex;
    use test::sum_inputs;
    use test::sum_outputs;

    #[test]
    fn test_segwit_offline() {
        let (_signer, derivator) = wpkh_signer();
        let mut builder = test::builder_from_derivator(derivator);

        builder.funding_input(30_000);
        builder.funding_input(50_000);
        builder.self_output(45_000);
        builder.dummy_external_output(10_000);
        let res = builder.simulate();

        assert!(res.error.is_none());
        assert!(res.warnings.is_empty());
        assert_eq!(res.change, Some(bitcoin::Amount::from_sat(24_749)));
        assert_eq!(res.fees, Some(bitcoin::Amount::from_sat(251)));
        let tx = builder.simulate().tx_template;
        assert_eq!(tx.inputs.len(), 2);
        assert_eq!(tx.outputs.len(), 2);
    }

    #[test]
    fn test_segwit_online() {
        let mut node = bitcoind_with_txindex();
        let bitcoind = &mut node.client;
        let (signer, derivator) = wpkh_signer();
        let mut builder = test::builder_from_derivator(derivator);

        // 2 owned input + external input + change
        let c1 = builder.fund_with_bitcoind(bitcoind, 30_000);
        let c2 = builder.fund_with_bitcoind(bitcoind, 50_000);

        builder.add_input(c1);
        builder.add_input(c2);
        builder.dummy_external_output(35_000);

        generate_sign_broadcast(
            &mut builder,
            &signer,
            bitcoind,
            220,
            80_000 - 35_000 - 220,
            &[],
            220,
        );

        // 1 owned input + external input + change
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 200_000);

        builder.add_input(c1);
        builder.dummy_external_output(100_000);

        generate_sign_broadcast(
            &mut builder,
            &signer,
            bitcoind,
            153,
            200_000 - 100_000 - 153,
            &[],
            153,
        );

        // 3 owned input + 1 to self + external (MAX)
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 200_000);
        let c2 = builder.fund_with_bitcoind(bitcoind, 10_000);
        let c3 = builder.fund_with_bitcoind(bitcoind, 83_000);

        builder.add_input(c1);
        builder.add_input(c2);
        builder.add_input(c3);
        builder.self_output(125_000);
        builder.dummy_external_output_max();

        generate_sign_broadcast(&mut builder, &signer, bitcoind, 288, 0, &[], 288);

        // 1 owned input + 1 change
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 1_000_000);

        builder.add_input(c1);

        generate_sign_broadcast(
            &mut builder,
            &signer,
            bitcoind,
            110,
            1_000_000 - 110,
            &[],
            110,
        );

        // 1 owned input + 1 external max
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 123_000);

        builder.add_input(c1);
        builder.dummy_external_output_max();

        generate_sign_broadcast(&mut builder, &signer, bitcoind, 122, 0, &[], 122);
    }

    #[test]
    fn test_tapkey_online() {
        let mut node = bitcoind_with_txindex();
        let bitcoind = &mut node.client;
        let (signer, derivator) = tr_signer();
        let mut builder = test::builder_from_derivator(derivator);

        // 2 owned input + external input + change
        let c1 = builder.fund_with_bitcoind(bitcoind, 30_000);
        let c2 = builder.fund_with_bitcoind(bitcoind, 50_000);

        builder.add_input(c1);
        builder.add_input(c2);
        builder.dummy_external_output(35_000);

        generate_sign_broadcast(
            &mut builder,
            &signer,
            bitcoind,
            212,
            80_000 - 35_000 - 212,
            &[],
            212,
        );

        // 1 owned input + external input + change
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 200_000);

        builder.add_input(c1);
        builder.dummy_external_output(100_000);

        generate_sign_broadcast(
            &mut builder,
            &signer,
            bitcoind,
            154,
            200_000 - 100_000 - 154,
            &[],
            154,
        );

        // 3 owned input + 1 to self + external (MAX)
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 200_000);
        let c2 = builder.fund_with_bitcoind(bitcoind, 10_000);
        let c3 = builder.fund_with_bitcoind(bitcoind, 83_000);

        builder.add_input(c1);
        builder.add_input(c2);
        builder.add_input(c3);
        builder.self_output(125_000);
        builder.dummy_external_output_max();

        generate_sign_broadcast(&mut builder, &signer, bitcoind, 269, 0, &[], 269);

        // 1 owned input + 1 change
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 1_000_000);

        builder.add_input(c1);

        generate_sign_broadcast(
            &mut builder,
            &signer,
            bitcoind,
            111,
            1_000_000 - 111,
            &[],
            111,
        );

        // 1 owned input + 1 external max
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 123_000);

        builder.add_input(c1);
        builder.dummy_external_output_max();

        generate_sign_broadcast(&mut builder, &signer, bitcoind, 111, 0, &[], 111);
    }

    #[test]
    fn test_taptree_online() {
        let mut node = bitcoind_with_txindex();
        let bitcoind = &mut node.client;
        let (signer, derivator) = taptree_signer();
        let mut builder = test::builder_from_derivator(derivator);

        // 2 owned input + external input + change
        let c1 = builder.fund_with_bitcoind(bitcoind, 30_000);
        let c2 = builder.fund_with_bitcoind(bitcoind, 50_000);

        builder.add_input(c1);
        builder.add_input(c2);
        builder.dummy_external_output(35_000);

        generate_sign_broadcast(
            &mut builder,
            &signer,
            bitcoind,
            246,
            80_000 - 35_000 - 246,
            &[],
            246,
        );

        // 1 owned input + external input + change
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 200_000);

        builder.add_input(c1);
        builder.dummy_external_output(100_000);

        generate_sign_broadcast(
            &mut builder,
            &signer,
            bitcoind,
            172,
            200_000 - 100_000 - 172,
            &[],
            172,
        );

        // 3 owned input + 1 to self + external (MAX)
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 200_000);
        let c2 = builder.fund_with_bitcoind(bitcoind, 10_000);
        let c3 = builder.fund_with_bitcoind(bitcoind, 83_000);

        builder.add_input(c1);
        builder.add_input(c2);
        builder.add_input(c3);
        builder.self_output(125_000);
        builder.dummy_external_output_max();

        generate_sign_broadcast(&mut builder, &signer, bitcoind, 321, 0, &[], 321);

        // 1 owned input + 1 change
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 1_000_000);

        builder.add_input(c1);

        generate_sign_broadcast(
            &mut builder,
            &signer,
            bitcoind,
            129,
            1_000_000 - 129,
            &[],
            129,
        );

        // 1 owned input + 1 external max
        builder.new_template();
        let c1 = builder.fund_with_bitcoind(bitcoind, 123_000);

        builder.add_input(c1);
        builder.dummy_external_output_max();

        generate_sign_broadcast(&mut builder, &signer, bitcoind, 129, 0, &[], 129);
    }

    #[test]
    fn test_tx_multiparty() {
        let mut node = bitcoind_with_txindex();
        let bitcoind = &mut node.client;
        let (signer_a, derivator_a) = tr_signer();
        let (signer_b, derivator_b) = taptree_signer();
        let mut builder_a = test::builder_from_derivator(derivator_a);
        let mut builder_b = test::builder_from_derivator(derivator_b);

        let c1 = builder_a.fund_with_bitcoind(bitcoind, 30_000);
        let c2 = builder_b.fund_with_bitcoind(bitcoind, 30_000);

        let r1 = builder_a.self_recipient(29_850);
        let r2 = builder_b.self_recipient(29_850);

        builder_a.add_input(c1);
        builder_a.add_input(c2);
        builder_a.add_output(r1);
        builder_a.add_output(r2);

        let res = builder_a.simulate();
        assert_eq!(res.fees, Some(bitcoin::Amount::from_sat(300)));
        assert_eq!(res.change, None);
        assert_eq!(res.warnings, vec![Warning::ChangeUnderDust(71)]);
        assert!(res.error.is_none());
        let mut psbt = builder_a.generate().unwrap();
        signer_a.sign(&mut psbt);
        signer_b.sign(&mut psbt);
        let tx = signer_a.finalize(&mut psbt).unwrap();
        let size = tx.weight().to_vbytes_ceil();
        assert_eq!(size, 229);
        let _txid = bitcoind.send_raw_transaction(&tx).unwrap().txid().unwrap();
    }

    #[test]
    fn test_pay_online() {
        let mut node = bitcoind_with_txindex();
        let bitcoind = &mut node.client;
        let (signer, derivator) = tr_signer();
        let mut builder = test::builder_from_derivator(derivator);

        // receive few coins
        builder.fund_with_bitcoind(bitcoind, 1_000_000);
        builder.fund_with_bitcoind(bitcoind, 2_000_000);
        builder.fund_with_bitcoind(bitcoind, 3_000_000);
        builder.fund_with_bitcoind(bitcoind, 7_000_000);
        builder.fund_with_bitcoind(bitcoind, 10_000_000);

        let address = bitcoind
            .get_new_address(None, None)
            .unwrap()
            .address()
            .unwrap()
            .assume_checked();

        let psbt = builder.pay(5_300_000, address, 1000).unwrap();
        // NOTE: Coin selection avoids consolidation, a "dumb" choice should be coins 1 + 2 + 3
        // as it's sufficient for macth selection criteria, but it's bad privacy wise, so we give
        // priority to selection w/ less inputs, it's also better fee wise
        assert_eq!(psbt.inputs.len(), 1);
        assert_eq!(sum_inputs(&psbt), 7_000_000);
        assert_eq!(sum_outputs(&psbt), 7_000_000 - 142);

        generate_sign_broadcast(
            &mut builder,
            &signer,
            bitcoind,
            142,
            7_000_000 - 5_300_000 - 142,
            &[],
            142,
        );
    }
}
