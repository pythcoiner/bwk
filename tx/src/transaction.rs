use miniscript::{
    bitcoin::{self, absolute, transaction::Version, Network, Psbt, TxOut, Weight},
    Descriptor, DescriptorPublicKey, ForEachKey,
};
use serde::{Deserialize, Serialize};

use crate::{
    coin::{shuffle_coins, Coin, KeyChain},
    recipient::{shuffle_recipients, Recipient},
    DUST_AMOUNT,
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
    Derivator,
    Descriptor,
    Input,
    Output,
    CoinSelection,
    CoinSelectionFee,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    pub change_descriptor: Descriptor<DescriptorPublicKey>,
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
    pub fn into_psbt(&self) -> Result<Psbt, Error> {
        // re-process the template as a sanity check
        let TransactionResult {
            tx_template, error, ..
        } = process_transaction(self.clone(), &self.change_descriptor);
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
                .push(i.to_psbt_input().map_err(|_| Error::Input)?);
        }

        for o in &self.outputs {
            psbt.outputs
                .push(o.to_psbt_output().map_err(|_| Error::Output)?);
        }

        Ok(psbt)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Amount {
    Value(u64),
    Max(Option<u64>),
    Anchor,
}

/// Estimate the satisfaction size for an input, returning the result
/// in weight units (WU).
/// Note: The output of this function represent the worst case scenario.
pub fn max_input_satisfaction_size(descriptor: &Descriptor<DescriptorPublicKey>) -> usize {
    descriptor
        .clone()
        .into_single_descriptors()
        .expect("multikey")
        .first()
        .expect("multikey")
        .clone()
        .max_weight_to_satisfy()
        .expect("invalid descriptor")
        .to_wu() as usize
}

/// Estimates the maximum possible weight of an unsigned transaction
pub fn tx_estimated_weight(tx_template: &TxTemplate) -> Weight {
    let mut inputs_weight = 0u64;
    for inp in &tx_template.inputs {
        inputs_weight += inp.satisfaction_size;
    }
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
    Weight::from_wu(size)
}

/// Estimate transaction weight from raw input/output weights.
/// Does not require TxTemplate or descriptors.
///
/// * `input_satisfaction_weights` - satisfaction weight in WU for each input
/// * `output_weights` - weight in WU for each output
pub fn estimated_weight_raw(input_satisfaction_weights: &[u64], output_weights: &[u64]) -> Weight {
    // Fixed overhead: version(4) + locktime(4) + input_count(1) + output_count(1) = 10 bytes
    // In weight units (non-witness): 10 * 4 = 40 WU
    // Plus segwit marker+flag: 2 WU
    let mut weight = 40u64 + 2;

    for &input_sat in input_satisfaction_weights {
        // Each input has: prevout(36) + script_sig_length(1) + sequence(4) = 41 bytes = 164 WU
        weight += 164 + input_sat;
    }

    for &output_w in output_weights {
        weight += output_w;
    }

    // varint adjustment for input/output counts > 252
    if input_satisfaction_weights.len() >= 253 {
        weight += 2 * 4; // 2 extra bytes for varint, non-witness
    }
    if output_weights.len() >= 253 {
        weight += 2 * 4;
    }

    Weight::from_wu(weight)
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
    pub tx_template: TxTemplate,
    pub change: Option<bitcoin::Amount>,
    pub fees: Option<bitcoin::Amount>,
    pub warnings: Vec<Warning>,
    pub error: Option<Error>,
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
    pub fees: Option<u64>,
    pub max: Option<u64>,
    pub change: Option<u64>,
    pub warnings: Vec<Warning>,
    pub error: Option<Error>,
}

pub enum Drain {
    Change,
    Max,
    None,
}

pub fn process_fees(
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
        match drain {
            Drain::None => {
                // TODO: warning
                fee_wo_change
            }
            Drain::Change => {
                result.change = Some(fee_allowance - fee_with_change);
                fee_with_change
            }
            Drain::Max => {
                result.max = Some(fee_allowance - fee_wo_change);
                fee_wo_change
            }
        }
    };
    result.fees = Some(fee);

    result
}

pub fn finalize_transaction<C>(
    res: TransactionResult,
    change_index: &mut C,
    descriptor: Descriptor<DescriptorPublicKey>,
    shuffle: bool,
) -> Result<Psbt, Error>
where
    C: FnMut() -> u32,
{
    if let Some(error) = res.error {
        return Err(error);
    }

    let mut template = res.tx_template;
    let change_descriptor = descriptor
        .clone()
        .into_single_descriptors()
        .expect("multipath")
        .get(1)
        .expect("multipath")
        .clone();
    let is_mainnet = change_descriptor.for_any_key(|k| match k {
        DescriptorPublicKey::XPub(k) => k.xkey.network.is_mainnet(),
        _ => unreachable!(),
    });

    let network = if is_mainnet {
        Network::Bitcoin
    } else {
        Network::Signet
    };

    if let Some(change_value) = res.change {
        let index = change_index();
        let address = descriptor
            .clone()
            .into_single_descriptors()
            .expect("multipath")
            .get(1)
            .expect("multipath")
            .at_derivation_index(index)
            .expect("derivation")
            .address(network)
            .expect("invalid descriptor");
        let change_recip = Recipient {
            address: address.as_unchecked().clone(),
            amount: Amount::Value(change_value.to_sat()),
            label: None, // FIXME: auto label change
            origin: Some((KeyChain::Change, index)),
            descriptor: Some(descriptor),
        };
        template.outputs.push(change_recip);
    }

    if shuffle {
        template.inputs = shuffle_coins(template.inputs);
        template.outputs = shuffle_recipients(template.outputs);
    }

    template.into_psbt()
}

/// Preprocesses a transaction based on the provided `TransactionTemplate`.
#[allow(clippy::type_complexity)]
pub fn process_transaction(
    tx_template: TxTemplate,
    descriptor: &Descriptor<DescriptorPublicKey>,
) -> TransactionResult {
    // TODO: implement coin selection if no or not enough input provided

    let mut result = TransactionResult::from_template(&tx_template);

    if tx_template.fees.is_null() {
        result.error = Some(Error::FeesNull);
        return result;
    }

    if tx_template.inputs.is_empty() {
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
    let tx_weight_wo_change = tx_estimated_weight(&tx_template);
    let tx_weight_with_change = tx_weight_wo_change + change_weight(descriptor);

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
            result
                .tx_template
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

#[cfg(all(test, feature = "test"))]
mod test {
    use super::*;
    use crate::{
        transaction::finalize_transaction,
        tx_builder::test::{external_recipient, funding_coin, sum_inputs, sum_outputs, tr_signer},
    };
    use miniscript::bitcoin;

    #[test]
    fn test_tr_tx() {
        let (_signer, derivator) = tr_signer();
        let descriptor = derivator.descriptor();

        let c1 = funding_coin(30_000, &derivator, 1);
        let c2 = funding_coin(50_000, &derivator, 2);

        let r1 = external_recipient(35_000);

        let template = TxTemplate {
            inputs: vec![c1, c2],
            outputs: vec![r1],
            fees: Fees::MilliSatsVb(1000),
            change_descriptor: descriptor.clone(),
        };

        let res = process_transaction(template.clone(), &descriptor);

        assert!(res.error.is_none());
        assert_eq!(res.change, Some(bitcoin::Amount::from_sat(44_788)));
        assert_eq!(res.fees, Some(bitcoin::Amount::from_sat(212)));

        let psbt = res.tx_template.into_psbt().unwrap();
        assert_eq!(sum_inputs(&psbt), 80_000);
        assert_eq!(sum_outputs(&psbt), 35_000); // change is not added

        let psbt =
            finalize_transaction(res.clone(), &mut (|| 1), descriptor.clone(), false).unwrap();
        assert_eq!(sum_inputs(&psbt), 80_000);
        assert_eq!(sum_outputs(&psbt), 80_000 - 212); // change is now added

        // shuffling must not change amounts
        let psbt =
            finalize_transaction(res.clone(), &mut (|| 1), descriptor.clone(), true).unwrap();
        assert_eq!(sum_inputs(&psbt), 80_000);
        assert_eq!(sum_outputs(&psbt), 80_000 - 212);
    }
}
