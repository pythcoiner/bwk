use miniscript::{
    bitcoin::{self, absolute, transaction::Version, Address, Network, Psbt, TxOut, Txid, Weight},
    Descriptor, DescriptorPublicKey,
};
use serde::{Deserialize, Serialize};

use crate::{
    coin::{shuffle_coins, Coin, KeyChain},
    recipient::{shuffle_recipients, Recipient},
    DescrFingerprint, DUST_AMOUNT, WITNESS_SCALE_FACTOR,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Error {
    Satisfaction,
    FeesNull,
    NoInputs,
    NoOutputs,
    SingleMax,
    AddressNetwork,
    AddInput,
    NotEnoughForFee,
    Descriptor,
    Input,
    Output,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Warning {
    ChangeUnderDust(u64),
    MaxUnderDust(u64),
    ChangeCreateDust(u64),
    MaxCreateDust(u64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Fees {
    Sats(u64),
    MilliSatsVb(u64),
}

impl Fees {
    pub fn is_null(&self) -> bool {
        match self {
            Fees::Sats(f) => *f == 0,
            Fees::MilliSatsVb(f) => *f == 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxTemplate {
    pub inputs: Vec<Coin>,
    pub outputs: Vec<Recipient>,
    pub fees: Fees,
    /// Descriptor of the potential change address, used to process
    /// the weight of the change output
    pub change_descriptor: crate::DescrFingerprint,
}

impl TxTemplate {
    pub fn tx(&self) -> bitcoin::Transaction {
        let template = self.clone();
        let input = template.inputs.into_iter().map(Into::into).collect();
        let output = template.outputs.into_iter().map(Into::into).collect();
        bitcoin::Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input,
            output,
        }
    }
    pub fn into_psbt<D, T>(&self, descriptor: &D, tx: &T) -> Result<Psbt, Error>
    where
        D: Fn(crate::DescrFingerprint) -> Option<Descriptor<DescriptorPublicKey>>,
        T: Fn(Txid) -> Option<bitcoin::Transaction>,
    {
        // re-process the template as a sanity check
        let TransactionResult {
            tx_template, error, ..
        } = process_transaction(self.clone(), descriptor, self.change_descriptor);
        if let Some(error) = error {
            return Err(error);
        }

        let unsigned_tx = tx_template.tx();

        let mut psbt = bitcoin::Psbt {
            unsigned_tx,
            version: 0,
            xpub: Default::default(),
            proprietary: Default::default(),
            unknown: Default::default(),
            inputs: vec![],
            outputs: vec![],
        };

        for i in &self.inputs {
            psbt.inputs
                .push(i.to_psbt_input(descriptor, tx).map_err(|_| Error::Input)?);
        }

        for o in &self.outputs {
            psbt.outputs
                .push(o.to_psbt_output(descriptor).map_err(|_| Error::Output)?);
        }

        Ok(psbt)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Amount {
    Value(u64),
    Max(Option<u64>),
    Anchor,
}

/// Estimate the satisfaction size for an input, returning the result
/// in weight units (WU).
/// Note: The output of this function represent the worst case scenario.
pub fn input_satisfaction_size(
    descriptor: &Descriptor<DescriptorPublicKey>,
) -> Result<usize, Error> {
    descriptor
        .clone()
        .into_single_descriptors()
        .expect("multikey")
        .first()
        .expect("multikey")
        .clone()
        .max_weight_to_satisfy()
        .map_err(|_| Error::Satisfaction)
        .map(|w| w.to_wu() as usize)
}

/// Estimates the maximum possible weight of an unsigned transaction
pub fn tx_estimated_weight<F>(descriptor: F, tx_template: &TxTemplate) -> Result<Weight, Error>
where
    F: Fn(crate::DescrFingerprint) -> Option<Descriptor<DescriptorPublicKey>>,
{
    let mut inputs_weight = 0u64;
    for inp in &tx_template.inputs {
        let inp_fg = inp.descriptor_fingerprint;
        let inp_descriptor = descriptor(inp_fg).ok_or(Error::Descriptor)?;
        let inp_satisfaction_weight: u64 = input_satisfaction_size(&inp_descriptor)?
            .try_into()
            .expect("valid weight");
        inputs_weight += inp_satisfaction_weight;
    }
    // Add weights together before converting to vbytes to avoid rounding up multiple times.
    let size = tx_template
        .tx()
        .weight()
        .to_wu()
        .checked_add(inputs_weight)
        .and_then(|weight| {
            weight.checked_add(
                // Make sure the Segwit marker and flag are included:
                // https://docs.rs/bitcoin/0.31.0/src/bitcoin/blockdata/transaction.rs.html#752-753
                // https://docs.rs/bitcoin/0.31.0/src/bitcoin/blockdata/transaction.rs.html#968-979
                if tx_template
                    .tx()
                    .input
                    .iter()
                    .all(|txin| txin.witness.is_empty())
                {
                    2
                } else {
                    0
                },
            )
        })
        .unwrap();
    let size = size
        .checked_add(WITNESS_SCALE_FACTOR.checked_sub(1).unwrap())
        .unwrap()
        .checked_div(WITNESS_SCALE_FACTOR)
        .unwrap();
    Ok(Weight::from_wu(size))
}

pub fn change_weight(descriptor: &Descriptor<DescriptorPublicKey>) -> Weight {
    let spk = descriptor
        .clone()
        .into_single_descriptors()
        .unwrap()
        .first()
        .unwrap()
        .at_derivation_index(0)
        .unwrap()
        .address(Network::Bitcoin)
        .unwrap()
        .script_pubkey();

    let txout = TxOut {
        value: bitcoin::Amount::MAX_MONEY,
        script_pubkey: spk,
    };
    txout.weight()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionResult {
    tx_template: TxTemplate,
    change: Option<bitcoin::Amount>,
    fees: Option<bitcoin::Amount>,
    warnings: Vec<Warning>,
    error: Option<Error>,
}

impl TransactionResult {
    pub fn from_template(tx_template: &TxTemplate) -> Self {
        TransactionResult {
            tx_template: tx_template.clone(),
            change: None,
            fees: None,
            warnings: vec![],
            error: None,
        }
    }
}

pub struct FeeResult {
    fees: Option<u64>,
    max: Option<u64>,
    change: Option<u64>,
    warnings: Vec<Warning>,
    error: Option<Error>,
}

pub enum Drain {
    Change,
    Max,
    None,
}

fn process_fees(
    fees: Fees,
    weight_wo_change: Weight,
    weight_with_change: Weight,
    sum_inputs: u64,
    sum_outputs: u64,
    drain: Drain,
) -> FeeResult {
    let mut result = FeeResult {
        fees: None,
        max: None,
        change: None,
        warnings: vec![],
        error: None,
    };
    if sum_outputs > sum_inputs {
        result.error = Some(Error::AddInput);
        return result;
    }
    let fee_allowance = sum_inputs - sum_outputs;
    #[allow(unused_parens)]
    let (fee_wo_change, fee_with_change) = match fees {
        Fees::Sats(fee) => (fee, fee),
        Fees::MilliSatsVb(fee) => {
            let wo_change = (weight_wo_change.to_vbytes_ceil() * fee / 1_000);
            let with_change = (weight_with_change.to_vbytes_ceil() * fee / 1_000);
            (wo_change, with_change)
        }
    };

    // Not enough for pay fees
    if fee_wo_change > fee_allowance {
        result.error = Some(Error::NotEnoughForFee);
        return result;
    }

    let fee = if (fee_allowance - fee_wo_change) < DUST_AMOUNT {
        // Enough for fee but not for drain
        let lost = fee_allowance - fee_wo_change;
        match drain {
            Drain::Change => {
                result.warnings.push(Warning::ChangeUnderDust(lost));
            }
            Drain::Max => {
                result.warnings.push(Warning::MaxUnderDust(lost));
            }
            Drain::None => {}
        }
        fee_allowance
    } else if (fee_allowance - fee_with_change) < DUST_AMOUNT {
        // Create a drain < DUST, so we dont
        let lost = fee_allowance - fee_wo_change;
        match drain {
            Drain::Change => {
                result.warnings.push(Warning::ChangeCreateDust(lost));
            }
            Drain::Max => {
                result.warnings.push(Warning::MaxCreateDust(lost));
            }
            Drain::None => {}
        }
        fee_allowance
    } else {
        // Enough for drain
        let drain_value = fee_allowance - fee_with_change;
        match drain {
            Drain::None => {
                // TODO: warning
            }
            Drain::Change => {
                result.change = Some(drain_value);
            }
            Drain::Max => {
                result.max = Some(drain_value);
            }
        };
        fee_with_change
    };
    result.fees = Some(fee);

    result
}

pub fn finalize_transaction<D, T, C>(
    res: TransactionResult,
    descriptor: &D,
    tx: &T,
    change: &C,
    change_fingerprint: DescrFingerprint,
    shuffle: bool,
) -> Result<Psbt, Error>
where
    C: Fn() -> (Address, usize),
    D: Fn(crate::DescrFingerprint) -> Option<Descriptor<DescriptorPublicKey>>,
    T: Fn(Txid) -> Option<bitcoin::Transaction>,
{
    if let Some(error) = res.error {
        return Err(error);
    }

    let mut template = res.tx_template;

    if let Some(change_value) = res.change {
        let (address, index) = change();
        let change_recip = Recipient {
            address: address.as_unchecked().clone(),
            amount: Amount::Value(change_value.to_sat()),
            label: None, // FIXME: auto label change
            origin: Some((KeyChain::Change, index as u32)),
            descriptor_fingerprint: Some(change_fingerprint),
        };
        template.outputs.push(change_recip);
    }

    if shuffle {
        template.inputs = shuffle_coins(template.inputs);
        template.outputs = shuffle_recipients(template.outputs);
    }

    template.into_psbt(descriptor, tx)
}

/// Preprocesses a transaction based on the provided `TransactionTemplate`.
#[allow(clippy::type_complexity)]
pub fn process_transaction<F>(
    mut tx_template: TxTemplate,
    descriptor: &F,
    change_descriptor: crate::DescrFingerprint,
) -> TransactionResult
where
    F: Fn(crate::DescrFingerprint) -> Option<Descriptor<DescriptorPublicKey>>,
{
    // TODO: implement coin selection if no or not enough input provided

    let mut result = TransactionResult::from_template(&tx_template);

    if tx_template.fees.is_null() {
        result.error = Some(Error::FeesNull);
        return result;
    }

    if tx_template.outputs.is_empty() {
        // FIXME: we can send to change
        result.error = Some(Error::NoOutputs);
        return result;
    } else if tx_template.inputs.is_empty() {
        // FIXME: Coin selection
        result.error = Some(Error::NoInputs);
        return result;
    }

    let mut outputs_total = 0;
    let mut maxed_output = None;
    for (pos, o) in tx_template.outputs.iter().enumerate() {
        match o.amount {
            Amount::Value(sat) => outputs_total += sat,
            Amount::Max(_) => {
                if maxed_output.is_some() {
                    result.error = Some(Error::SingleMax);
                    return result;
                }
                maxed_output = Some(pos);
                // NOTE: we do not add the value of the MAX output as it's expected
                // to be re-processed
            }
            Amount::Anchor => { /* anchor has 0 sats outputs */ }
        }
    }

    let inputs_total = tx_template
        .inputs
        .iter()
        .fold(0u64, |sum, coin| sum + coin.txout.value.to_sat());

    // let tx = tx_template.tx();
    let tx_weight_wo_change = match tx_estimated_weight(descriptor, &tx_template) {
        Ok(w) => w,
        Err(e) => {
            result.error = Some(e);
            return result;
        }
    };
    let change_descriptor = match descriptor(change_descriptor) {
        Some(d) => d,
        None => {
            result.error = Some(Error::Descriptor);
            return result;
        }
    };
    let tx_weight_with_change = tx_weight_wo_change + change_weight(&change_descriptor);

    let drain = match maxed_output {
        Some(_) => Drain::Max,
        None => Drain::Change,
    };

    let FeeResult {
        fees,
        max,
        change,
        warnings: mut warning,
        error,
    } = process_fees(
        tx_template.fees,
        tx_weight_wo_change,
        tx_weight_with_change,
        inputs_total,
        outputs_total,
        drain,
    );

    result.warnings.append(&mut warning);
    if let Some(error) = error {
        result.error = Some(error);
        return result;
    }

    result.fees = fees.map(bitcoin::Amount::from_sat);
    // FIXME: maybe sanitize fees? fee < 10% total amount?

    match (maxed_output, max, change) {
        (Some(pos), Some(value), None) => {
            tx_template
                .outputs
                .get_mut(pos)
                .expect("max output missing")
                .amount = Amount::Max(Some(value));
        }
        (None, None, Some(change)) => result.change = Some(bitcoin::Amount::from_sat(change)),
        (None, None, None) => {}
        (_, _, _) => unreachable!(),
    }

    result
}

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;

    use bwk_descriptor::{tr_path, SpkDerivator};
    use bwk_sign::HotSigner;
    use bwk_utils::test::{random_input, random_output, txid};
    use miniscript::bitcoin::{
        self,
        bip32::ChildNumber,
        key::rand::{self, random, Rng},
        Amount, Network, OutPoint, Psbt, ScriptBuf, Sequence, TxOut,
    };

    use crate::{
        coin::KeyChain, descr_fingerprint, transaction::finalize_transaction, Coin, CoinStatus,
        DescrFingerprint, Recipient,
    };

    use super::{process_transaction, Fees, TxTemplate};

    fn tr_signer() -> (HotSigner, SpkDerivator) {
        let nw = Network::Regtest;
        let signer = HotSigner::new(nw).unwrap();
        let path = tr_path(nw, ChildNumber::from_hardened_idx(0).unwrap()).unwrap();
        let xpub = signer.xpub(&path);
        let derivator = SpkDerivator::new_tr(xpub, nw).unwrap();
        (signer, derivator)
    }

    fn receive_coin(amount: u64, derivator: &SpkDerivator, index: u32) -> Coin {
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
        let descriptor_fingerprint =
            descr_fingerprint::DescrFingerprint::new(&derivator.descriptor());
        Coin {
            txout,
            outpoint,
            coin_path: (KeyChain::Receive, index),
            height: None,
            sequence: Sequence::ZERO,
            status: CoinStatus::Unconfirmed,
            label: None,
            descriptor_fingerprint,
        }
    }

    fn self_recipient(amount: u64, derivator: &SpkDerivator, index: u32) -> Recipient {
        let address = derivator.receive_at(index).as_unchecked().clone();
        let descriptor_fingerprint =
            descr_fingerprint::DescrFingerprint::new(&derivator.descriptor());
        Recipient {
            address,
            amount: super::Amount::Value(amount),
            label: None,
            origin: Some((KeyChain::Receive, index)),
            descriptor_fingerprint: Some(descriptor_fingerprint),
        }
    }

    fn external_recipient(amount: u64) -> Recipient {
        let (_signer, derivator) = tr_signer();
        let index: u16 = random();
        let address = derivator.receive_at(index as u32).as_unchecked().clone();
        Recipient {
            address,
            amount: super::Amount::Value(amount),
            label: None,
            origin: None,
            descriptor_fingerprint: None,
        }
    }

    fn funding_coin(
        amount: u64,
        derivator: &SpkDerivator,
        index: u32,
    ) -> (bitcoin::Transaction, Coin) {
        let spk = derivator.receive_at(index).script_pubkey();
        let descriptor_fingerprint =
            descr_fingerprint::DescrFingerprint::new(&derivator.descriptor());
        let (tx, pos) = funding_tx(spk, (amount as f64) / 100_000_000.0);
        let txid = tx.compute_txid();

        let txout = tx.output[pos].clone();
        let outpoint = OutPoint {
            txid,
            vout: (pos as u32),
        };

        let coin = Coin {
            txout,
            outpoint,
            coin_path: (KeyChain::Receive, index),
            height: None,
            sequence: Sequence::ZERO,
            status: CoinStatus::Unconfirmed,
            label: None,
            descriptor_fingerprint,
        };

        (tx, coin)
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

    fn sum_inputs(psbt: &Psbt) -> u64 {
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

    fn sum_outputs(psbt: &Psbt) -> u64 {
        psbt.unsigned_tx
            .output
            .iter()
            .fold(0, |sum, o| sum + o.value.to_sat())
    }

    #[test]
    fn test_tr_tx() {
        let (_signer, derivator) = tr_signer();
        let descriptor = derivator.descriptor();
        let fg = DescrFingerprint::new(&descriptor);

        let mut tx_map = BTreeMap::new();

        let (tx1, c1) = funding_coin(30_000, &derivator, 1);
        let (tx2, c2) = funding_coin(50_000, &derivator, 2);

        tx_map.insert(tx1.compute_txid(), tx1);
        tx_map.insert(tx2.compute_txid(), tx2);

        let r1 = external_recipient(35_000);

        let template = TxTemplate {
            inputs: vec![c1, c2],
            outputs: vec![r1],
            fees: Fees::MilliSatsVb(1000),
            change_descriptor: fg,
        };

        let res = process_transaction(
            template.clone(),
            &(|f| (f == fg).then_some(descriptor.clone())),
            fg,
        );

        assert!(res.error.is_none());
        assert_eq!(res.change, Some(bitcoin::Amount::from_sat(44_914)));
        assert_eq!(res.fees, Some(bitcoin::Amount::from_sat(86)));

        let psbt = res
            .tx_template
            .into_psbt(
                &(|f| (f == fg).then_some(descriptor.clone())),
                &(|txid| tx_map.get(&txid).cloned()),
            )
            .unwrap();
        assert_eq!(sum_inputs(&psbt), 80_000);
        assert_eq!(sum_outputs(&psbt), 35_000); // change is not added

        let change = derivator.change_at(1);
        let psbt = finalize_transaction(
            res.clone(),
            &(|f| (f == fg).then_some(descriptor.clone())),
            &(|txid| tx_map.get(&txid).cloned()),
            &(|| (change.clone(), 1)),
            fg,
            false,
        )
        .unwrap();
        assert_eq!(sum_inputs(&psbt), 80_000);
        assert_eq!(sum_outputs(&psbt), 80_000 - 86); // change is now added

        // shuffling must not change amounts
        let psbt = finalize_transaction(
            res,
            &(|f| (f == fg).then_some(descriptor.clone())),
            &(|txid| tx_map.get(&txid).cloned()),
            &(|| (change.clone(), 1)),
            fg,
            true,
        )
        .unwrap();
        assert_eq!(sum_inputs(&psbt), 80_000);
        assert_eq!(sum_outputs(&psbt), 80_000 - 86); // change is now added
    }
}
