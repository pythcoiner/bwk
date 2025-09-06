use std::{
    collections::BTreeMap,
    path::PathBuf,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

use bwk_backoff::Backoff;
use bwk_electrum::client::{CoinRequest, CoinResponse};
use miniscript::{
    bitcoin::{self, absolute, EcdsaSighashType, OutPoint, ScriptBuf, TxOut},
    psbt::PsbtExt,
};
use serde::{Deserialize, Serialize};

use crate::{
    address_store::{AddressEntry, AddressTip},
    coin::Coin,
    coin_store::{CoinEntry, CoinStore},
    config::{Config, Tip},
    derivator::Derivator,
    label_store::{LabelKey, LabelStore},
    signing_manager::SigningManager,
    tx_store::TxStore,
};

/// The factor that non-witness serialization data is multiplied by during weight calculation.
const WITNESS_SCALE_FACTOR: u64 = 4;

const DUST_AMOUNT: u64 = 5_000;

pub struct TransactionTemplate {
    pub inputs: Vec<RustCoin>,
    pub outputs: Vec<Output>,
    pub fee_sats: u64,
    fee_sats_vb: f64,
}

pub struct TransactionSimulation {
    pub spendable: bool,
    pub has_change: bool,
    pub estimated_weight: u64,
    pub error: String,
}

pub struct Output {
    pub address: String,
    pub amount: u64, // amount in sats
    pub label: String,
    pub max: bool, // if max == true, amount is not taken in account,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
pub enum CoinStatus {
    Unconfirmed,
    Confirmed,
    BeingSpend,
    Spent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
pub enum AddrAccount {
    Receive,
    Change,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
pub enum AddressStatus {
    NotUsed,
    Used,
    Reused,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CoinState {
    pub coins: Vec<RustCoin>,
    pub confirmed_coins: usize,
    pub confirmed_balance: u64,
    pub unconfirmed_coins: usize,
    pub unconfirmed_balance: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RustCoin {
    pub value: u64,
    pub height: u64,
    pub confirmed: bool,
    pub status: CoinStatus,
    pub outpoint: String,
    pub address: RustAddress,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RustAddress {
    pub address: String,
    pub status: AddressStatus,
    pub account: AddrAccount,
    pub index: u32,
}

impl From<AddrAccount> for u32 {
    fn from(value: AddrAccount) -> Self {
        match value {
            AddrAccount::Receive => 0,
            AddrAccount::Change => 2,
        }
    }
}

impl From<u32> for AddrAccount {
    fn from(value: u32) -> Self {
        match value {
            0 => AddrAccount::Receive,
            1 => AddrAccount::Change,
            _ => unimplemented!(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Transaction {
    pub height: Option<u64>,
    pub txid: String,
    pub inputs: Vec<TxInput>,
    pub outputs: Vec<TxOutput>,
    pub fees: u64,
    pub weight: u64,
}

#[derive(Debug, Clone)]
pub struct TxInput {
    pub vin: usize,
    pub outpoint: String,
    pub value: u64,
    pub owned: bool,
}

#[derive(Debug, Clone)]
pub struct TxOutput {
    pub vout: usize,
    pub spk: ScriptBuf,
    pub value: u64,
    pub owned: bool,
}

/// Represents different types of errors that can occur.
#[derive(Debug)]
pub enum Notification {
    Electrum(TxListenerNotif),
    AddressTipChanged,
    CoinUpdate,
    InvalidElectrumConfig,
    InvalidLookAhead,
    Stopped,
    Error(Error),
}

impl From<TxListenerNotif> for Notification {
    fn from(value: TxListenerNotif) -> Self {
        Notification::Electrum(value)
    }
}

impl From<Error> for Notification {
    fn from(value: Error) -> Self {
        Self::Error(value)
    }
}

#[derive(Debug)]
pub enum Error {
    CreatePool,
    JoinPool,
    InvalidOutPoint,
    CoinMissing,
    InvalidDenomination,
    RelayMissing,
    WrongElectrumConfig,
    PoolMissing,
    WrongKeyType,
    Satisfaction,
}

/// Represents notifications related to transaction listeners.
#[derive(Debug, Clone)]
pub enum TxListenerNotif {
    Started,
    Connected(String),
    Error(String),
    Stopped,
}

#[derive(Debug)]
pub struct Account {
    coin_store: Arc<Mutex<CoinStore>>,
    label_store: Arc<Mutex<LabelStore>>,
    receiver: Option<mpsc::Receiver<Notification>>,
    sender: mpsc::Sender<Notification>,
    tx_listener: Option<JoinHandle<()>>,
    config: Config,
    electrum_stop: Option<Arc<AtomicBool>>,
    signing_manager: SigningManager,
}

impl Drop for Account {
    fn drop(&mut self) {
        if let Some(stop) = self.electrum_stop.as_mut() {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

// Rust only interface
impl Account {
    /// Creates a new `Account` instance with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - The configuration for the account.
    ///
    /// # Returns
    ///
    /// A new `Account` instance.
    pub fn new(config: Config) -> Self {
        assert!(!config.account.is_empty());
        let (sender, receiver) = mpsc::channel();
        let tx_data = if config.persist {
            TxStore::store_from_file(config.transactions_path())
        } else {
            BTreeMap::new()
        };
        let tx_store =
            TxStore::new(tx_data, Some(config.transactions_path())).enable_persist(config.persist);
        let (receive, change) = if config.persist {
            let Tip { receive, change } = config.tip_from_file();
            (receive, change)
        } else {
            (0, 0)
        };
        let label_store = Arc::new(Mutex::new(
            LabelStore::from_file(config.clone()).enable_persist(config.persist),
        ));
        let coin_store = Arc::new(Mutex::new(CoinStore::new(
            config.network,
            config.descriptor.clone(),
            sender.clone(),
            receive,
            change,
            config.look_ahead,
            tx_store,
            label_store.clone(),
            Some(config.clone()),
        )));
        coin_store.lock().expect("poisoned").generate();
        let mut signing_manager =
            SigningManager::new(PathBuf::new(), config.dir_name()).enable_persist(config.persist);
        if let Some(mnemo) = config.mnemonic.clone() {
            signing_manager.new_hot_signer_from_mnemonic(config.network(), mnemo);
        }
        let mut account = Account {
            coin_store,
            label_store,
            tx_listener: None,
            electrum_stop: None,
            receiver: Some(receiver),
            sender,
            config,
            signing_manager,
        };
        account.start_electrum();
        account
    }

    pub fn network(&self) -> bitcoin::Network {
        self.config.network()
    }

    pub fn name(&self) -> String {
        self.config.account.clone()
    }

    pub fn descriptor(&self) -> String {
        self.config.descriptor.to_string()
    }

    /// Starts listening for transactions on the specified address and port.
    ///
    /// # Arguments
    ///
    /// * `addr` - The address to listen on.
    /// * `port` - The port to listen on.
    ///
    /// # Returns
    ///
    /// A tuple containing a sender for address tips and a stop flag.
    fn start_listen_txs(
        &mut self,
        addr: String,
        port: u16,
        config: Config,
    ) -> (mpsc::Sender<AddressTip>, Arc<AtomicBool>) {
        log::debug!("Account::start_poll_txs()");
        let (sender, address_tip) = mpsc::channel();
        let coin_store = self.coin_store.clone();
        let notification = self.sender.clone();
        let derivator = self.derivator();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_request = stop.clone();

        let poller = thread::spawn(move || {
            let client = match bwk_electrum::client::Client::new(&addr, port) {
                Ok(c) => c,
                Err(e) => {
                    log::error!("start_listen_txs(): fail to create electrum client {}", e);
                    let _ = notification.send(TxListenerNotif::Error(e.to_string()).into());
                    return;
                }
            };

            let addr = format!("{}:{}", addr, port);
            let _ = notification.send(TxListenerNotif::Connected(addr).into());

            let (request, response) = client.listen::<CoinRequest, CoinResponse>();

            listen_txs(
                coin_store,
                derivator,
                notification,
                address_tip,
                stop_request,
                request,
                response,
                Some(config),
            );
        });
        self.tx_listener = Some(poller);
        (sender, stop)
    }

    pub fn receiver(&mut self) -> Option<mpsc::Receiver<Notification>> {
        self.receiver.take()
    }

    /// Returns the derivator associated with the account.
    ///
    /// # Returns
    ///
    /// A `Derivator` instance.
    pub fn derivator(&self) -> Derivator {
        self.coin_store.lock().expect("poisoned").derivator()
    }

    /// Returns a map of coins associated with the account.
    ///
    /// # Returns
    ///
    /// A `BTreeMap` of `OutPoint` to `CoinEntry`.
    pub fn coins(&self) -> BTreeMap<OutPoint, CoinEntry> {
        self.coin_store.lock().expect("poisoned").coins()
    }

    /// Returns the receiving address at the specified index.
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the receiving address.
    ///
    /// # Returns
    ///
    /// A `bitcoin::Address` instance.
    pub fn recv_at(&self, index: u32) -> bitcoin::Address {
        self.coin_store
            .lock()
            .expect("poisoned")
            .derivator_ref()
            .receive_at(index)
    }

    /// Returns the change address at the specified index.
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the change address.
    ///
    /// # Returns
    ///
    /// A `bitcoin::Address` instance.
    pub fn change_at(&self, index: u32) -> bitcoin::Address {
        self.coin_store
            .lock()
            .expect("poisoned")
            .derivator_ref()
            .change_at(index)
    }

    /// Generates a new receiving address for the account.
    ///
    /// # Returns
    ///
    /// A `bitcoin::Address` instance.
    pub fn new_recv_addr(&mut self) -> bitcoin::Address {
        self.coin_store.lock().expect("poisoned").new_recv_addr()
    }
    /// Generates a new change address for the account.
    ///
    /// # Returns
    ///
    /// A `bitcoin::Address` instance.
    pub fn new_change_addr(&mut self) -> bitcoin::Address {
        self.coin_store.lock().expect("poisoned").new_change_addr()
    }

    /// Returns the current receiving watch tip index.
    ///
    /// # Returns
    ///
    /// The receiving watch tip index as a `u32`.
    pub fn recv_watch_tip(&self) -> u32 {
        self.coin_store.lock().expect("poisoned").recv_watch_tip()
    }

    /// Returns the current change watch tip index.
    ///
    /// # Returns
    ///
    /// The change watch tip index as a `u32`.
    pub fn change_watch_tip(&self) -> u32 {
        self.coin_store.lock().expect("poisoned").change_watch_tip()
    }

    /// Re-generate coin_store from tx_store
    pub fn generate_coins(&mut self) {
        self.coin_store.lock().expect("poisoned").generate();
    }
    /// Returns spendable coins for the account.
    pub fn spendable_coins(&self) -> CoinState {
        self.coin_store.lock().expect("poisoned").spendable_coins()
    }

    /// Returns a list of all historical transactions
    pub fn tx_history(&self) -> Vec<Transaction> {
        self.coin_store.lock().expect("poisoned").tx_history()
    }

    /// Calculates the satisfaction size for an input, returning the result
    /// in weight units (WU).
    ///
    /// This function estimates the size required to satisfy the inputs of a
    /// transaction based on the
    /// account's descriptor.
    ///
    /// # Returns
    ///
    /// A `Result<usize, Error>` where:
    /// - `Ok(usize)` indicates the estimated satisfaction size in weight units (WU).
    /// - `Err(Error)` indicates an error occurred during the calculation.
    ///
    /// # Errors
    ///
    /// This function can return an error in the following cases:
    /// - If the descriptor is invalid or cannot be parsed, leading to an
    ///   inability to determine the satisfaction size for the inputs.
    pub fn input_satisfaction_size(&self) -> Result<usize, Error> {
        self.config
            .descriptor
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

    /// Estimates the maximum possible weight in weight units of an unsigned
    /// transaction, `tx`, after satisfaction, assuming all inputs of `tx` are
    /// from this descriptor.
    ///
    /// # Arguments
    ///
    /// * `tx` - A reference to the `bitcoin::Transaction` for which the weight
    ///   is to be estimated.
    ///
    /// # Returns
    ///
    /// A `Result<u64, Error>` containing:
    /// - `Ok(u64)` - The estimated weight of the transaction in weight units (WU).
    /// - `Err(Error)` - An error if the estimation fails.
    ///
    /// # Errors
    ///
    /// This function can return an error in the following cases:
    /// - If the satisfaction size for the inputs cannot be calculated due to an
    ///   invalid descriptor.
    /// - If the input satisfaction size exceeds the maximum allowable weight,
    ///   leading to an overflow.
    /// - If the transaction contains no inputs, which would make weight estimation
    ///   impossible.
    /// - If there is an issue with the underlying logic of the weight calculation,
    ///   such as unexpected behavior from the `miniscript` library.
    ///
    /// # Note
    ///
    /// The weight is calculated based on the number of inputs and their respective
    /// satisfaction sizes.
    /// The function takes into account the Segwit marker and flag, ensuring that
    /// the weight is accurately represented according to Bitcoin's transaction weight rules.
    /// This logic have been borrowed from Liana wallet.
    pub fn tx_estimated_weight(&self, tx: &bitcoin::Transaction) -> Result<u64, Error> {
        let num_inputs: u64 = tx.input.len().try_into().unwrap();
        let max_sat_weight: u64 = self.input_satisfaction_size()?.try_into().unwrap();
        // Add weights together before converting to vbytes to avoid rounding up multiple times.
        let size = tx
            .weight()
            .to_wu()
            .checked_add(max_sat_weight.checked_mul(num_inputs).unwrap())
            .and_then(|weight| {
                weight.checked_add(
                    // Make sure the Segwit marker and flag are included:
                    // https://docs.rs/bitcoin/0.31.0/src/bitcoin/blockdata/transaction.rs.html#752-753
                    // https://docs.rs/bitcoin/0.31.0/src/bitcoin/blockdata/transaction.rs.html#968-979
                    if num_inputs > 0 && tx.input.iter().all(|txin| txin.witness.is_empty()) {
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
        Ok(size)
    }

    /// Returns the coin matching the given outpoint if found, else None.
    pub fn get_coin(&self, outpoint: &OutPoint) -> Option<Coin> {
        self.coin_store
            .lock()
            .expect("poisoned")
            .get(outpoint)
            .map(|e| e.coin)
    }

    /// Generates a static dummy script public key (SPK) for change outputs.
    ///
    /// This function always returns the same dummy spk,
    /// It is useful for simulating change outputs in a non-final transaction
    /// while allowing for easy replacement of this SPK later during the final
    /// crafting of the transaction.
    ///
    /// # Returns
    ///
    /// A `ScriptBuf` representing the dummy script public key for the change address.
    ///
    /// # Note
    ///
    /// The SPK returned by this function is not intended for actual use in
    /// transactions until it is replaced with a valid SPK at the time of final
    /// transaction crafting.
    pub fn dummy_spk(&self) -> ScriptBuf {
        ScriptBuf::from_bytes(vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
    }

    /// Assembles a Bitcoin transaction from the provided inputs and outputs.
    ///
    /// This function creates a new `bitcoin::Transaction` by populating its
    /// input and output fields based on the provided vectors of coin entries
    /// and transaction outputs. The transaction is initialized with a version
    /// and a lock time of zero.
    ///
    /// # Arguments
    ///
    /// * `inputs` - A reference to a vector of `CoinEntry` representing the
    ///   inputs for the transaction.
    /// * `outputs` - A reference to a vector of tuples, where each tuple contains
    ///   a `TxOut` and an optional tuple with an `AddrAccount` and an index,
    ///   representing the outputs for the transaction.
    ///
    /// # Returns
    ///
    /// A `bitcoin::Transaction` instance that contains the assembled inputs and outputs.
    fn assembly_tx(
        inputs: &Vec<CoinEntry>,
        outputs: &Vec<(TxOut, Option<(AddrAccount, u32)>)>,
    ) -> bitcoin::Transaction {
        let mut tx = bitcoin::Transaction {
            version: bitcoin::transaction::Version(2),
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![],
        };

        for i in inputs {
            tx.input.push(i.txin());
        }

        for o in outputs {
            tx.output.push(o.0.clone());
        }

        tx
    }

    /// Preprocesses a transaction based on the provided `TransactionTemplate`.
    ///
    /// This function processes the transaction template to estimate whether the
    /// transaction can be successfully executed, including whether it is spendable
    /// and if it requires change.
    ///
    /// # Arguments
    ///
    /// * `tx_template` - A `TransactionTemplate` containing the details of the
    ///   transaction to be processed.
    ///
    /// # Returns
    ///
    /// A `Result` containing:
    /// - A tuple with three elements:
    ///   - A `Vec<CoinEntry>` representing the inputs for the transaction.
    ///   - A `Vec<(TxOut, Option<(AddrAccount, u32)>)>` representing the outputs
    ///     for the transaction.
    ///   - A boolean indicating if the transaction contains a change output.
    ///
    /// # Errors
    ///
    /// This function can return an error in the following cases:
    /// - If both `fee_sats` and `fee_sats_vb` are filled, which is not allowed.
    /// - If neither `fee_sats` nor `fee_sats_vb` is filled, which is required.
    /// - If `fee_sats_vb` is filled with a value less than 1.0, which is below
    ///   the minimum allowed fee rate.
    /// - If the provided outpoints do not match any available coins in the coin
    ///   store. (external inputs are not allowed for now)
    /// - If the total inputs amount is less than the total outputs amount, making
    ///   the transaction invalid.
    /// - If the maximum output is selected but there are not enough reserves to fill it.
    /// - If the estimated fees exceed the available reserves.
    /// - If there is an issue while handling the transaction history or if the
    ///   transaction cannot be created for any other reason.
    ///
    /// # Note
    ///
    /// This function is intended for use in scenarios where users need to gather
    /// information about the transaction before finalizing it, allowing for
    /// adjustments based on the simulation results.
    #[allow(clippy::type_complexity)]
    pub fn process_transaction(
        &self,
        tx_template: &TransactionTemplate,
    ) -> Result<
        (
            Vec<CoinEntry>,                           /* inputs */
            Vec<(TxOut, Option<(AddrAccount, u32)>)>, /* outputs */
            bool,                                     /* change */
        ),
        String,
    > {
        // TODO: implement coin selection if no or not enough input provided

        if (tx_template.fee_sats > 0) && (tx_template.fee_sats_vb > 0.0) {
            return Err("Only one of fee_sats or fee_sats_vb must be filled!".to_string());
        } else if (tx_template.fee_sats == 0) && (tx_template.fee_sats_vb == 0.0) {
            return Err("One of fee_sats or fee_sats_vb must be filled!".to_string());
        } else if (tx_template.fee_sats == 0) && (tx_template.fee_sats_vb < 1.0) {
            return Err("Minimum allowed fee rate is 1 sat/vb!".to_string());
        }

        if tx_template.outputs.is_empty() {
            return Err("No outputs!".to_string());
        } else if tx_template.inputs.is_empty() {
            return Err("No inputs!".to_string());
        }

        let mut inputs_total = 0;
        let mut outputs_total = 0;

        // count maxed outputs
        let mut maxed_outputs = 0;
        let mut maxed_output = None;

        for o in &tx_template.outputs {
            if o.max {
                if maxed_outputs > 0 {
                    return Err("A single output can have the MAX field selected".to_string());
                } else {
                    maxed_outputs += 1;
                }
            }
        }

        // parse outpoints
        let mut outpoints = vec![];
        for inp in &tx_template.inputs {
            let parsed = OutPoint::from_str(&inp.outpoint);
            match parsed {
                Ok(op) => outpoints.push(op),
                Err(_) => return Err("Fail to parse Outpoint".to_string()),
            }
        }

        // craft outputs
        let mut outputs = vec![];
        {
            for out in &tx_template.outputs {
                if !out.max {
                    outputs_total += out.amount;
                }

                // parse address & sanitize address
                let addr = match bitcoin::Address::from_str(&out.address) {
                    Ok(a) => a,
                    Err(_) => return Err("Fail to parse address".to_string()),
                };
                if !addr.is_valid_for_network(self.config.network) {
                    return Err("Provided address is not valid for the current network".to_string());
                }
                let addr = addr.assume_checked();
                let amount = if out.max {
                    bitcoin::Amount::ZERO
                } else {
                    bitcoin::Amount::from_sat(out.amount)
                };
                let txout = bitcoin::TxOut {
                    value: amount,
                    script_pubkey: addr.script_pubkey(),
                };
                if out.max {
                    maxed_output = Some(addr.script_pubkey());
                }

                outputs.push((txout, None));
            }
        }

        // get informations about coins to spend
        let inputs = {
            let store = self.coin_store.lock().expect("poisoned");
            let mut inputs = Vec::<CoinEntry>::new();
            for op in outpoints {
                match store.get(&op) {
                    Some(coin) => {
                        inputs_total += coin.amount_sat();
                        inputs.push(coin);
                    }
                    // TODO: maybe support external inputs?
                    None => {
                        return Err("Provided outpoint do not match an available coin".to_string())
                    }
                }
            }
            inputs
        }; // <- release coin_store lock

        let fee_reserve = inputs_total - outputs_total;

        if maxed_output.is_some() && fee_reserve < DUST_AMOUNT {
            return Err("Not enough reserve to fill maxed output!".to_string());
        }

        if tx_template.fee_sats > fee_reserve {
            return Err("Not enough reserve to pay fees!".to_string());
        }

        // estimate the fee value if change is not needed
        let tx_without_change = Self::assembly_tx(&inputs, &outputs);
        let estimated_weight_without_change = match self.tx_estimated_weight(&tx_without_change) {
            Ok(w) => w,
            Err(e) => return Err(format!("Failed to estimate tx weight: {e:?}")),
        };
        let fees_without_change = if tx_template.fee_sats_vb > 0.0 {
            let estimated_fees_without_change =
                (tx_template.fee_sats_vb * estimated_weight_without_change as f64).ceil() as u64;
            if estimated_fees_without_change > fee_reserve {
                return Err("Not enough reserve to pay fees!".to_string());
            }
            estimated_fees_without_change
        } else {
            tx_template.fee_sats
        };

        let mut change = maxed_output.is_none() && (fee_reserve > DUST_AMOUNT);

        let fees = if change {
            // if a change output is expected we add a dummy output
            let dummy_spk = self.dummy_spk();
            let txout = bitcoin::TxOut {
                // NOTE: putting a dummy 0 amount, will be adjusted
                // after processing fees
                value: bitcoin::Amount::from_sat(0),
                // NOTE: we use here the dummy spk in order to make it easy to
                // find which output we need to substract fees from in a later step.
                script_pubkey: dummy_spk,
            };
            outputs.push((txout, None));

            // process tx weight w/ the added change
            let tx_with_change = Self::assembly_tx(&inputs, &outputs);
            let estimated_weight_with_change = match self.tx_estimated_weight(&tx_with_change) {
                Ok(w) => w,
                Err(e) => return Err(format!("Failed to estimate tx weight: {e:?}")),
            };

            let mut fees = if tx_template.fee_sats_vb > 0.0 {
                // fee rate is given, we estimate the fee value
                let estimated_fees_with_change =
                    (tx_template.fee_sats_vb * estimated_weight_with_change as f64).ceil() as u64;

                if estimated_fees_with_change > fee_reserve {
                    // if the reserve not contain enough for pay fees, we drop the change output
                    outputs.pop();
                    change = false;
                    fees_without_change
                } else {
                    estimated_fees_with_change
                }
            } else {
                // fee amount have been selected
                let min_fee = estimated_weight_with_change;
                if fee_reserve < min_fee {
                    outputs.pop();
                    change = false;
                }
                tx_template.fee_sats
            };

            if change {
                // if the resulting change amount < DUST amount
                // we drop the change output
                let change_amount = fee_reserve - fees;
                if change_amount < DUST_AMOUNT {
                    outputs.pop();
                    change = false;
                    fees = fee_reserve;
                }
            }

            fees
        } else {
            fees_without_change
        };

        if fees > fee_reserve {
            return Err("Not enough reserve to pay fees!".to_string());
        }

        // fill amount for maxed or change output
        let change_or_max = bitcoin::Amount::from_sat(fee_reserve - fees);
        if change {
            outputs.last_mut().expect("as a last output").0.value = change_or_max;
        } else {
            if change_or_max.to_sat() < DUST_AMOUNT {
                return Err("Maxed output amount is lower than the dust limit".to_string());
            }
            let dummy_spk = self.dummy_spk();
            for (txout, _) in &mut outputs {
                if txout.script_pubkey == dummy_spk {
                    txout.value = change_or_max;
                }
            }
        }

        // populate addresses indexes
        {
            let store = self.coin_store.lock().expect("poisoned");
            for out in &mut outputs {
                let spk = &out.0.script_pubkey;
                // get derivation index of this address
                // NOTE: this is needed by the signer to know it's a change/send-to-self
                out.1 = store.address_info(spk).map(|e| (e.account(), e.index()));
            }
        } // <- release coin_store lock here

        Ok((inputs, outputs, change))
    }

    pub fn change_index(&self, tx: &bitcoin::Transaction) -> Option<usize> {
        let dummy_spk = self.dummy_spk();
        for (index, TxOut { script_pubkey, .. }) in tx.output.iter().enumerate() {
            if *script_pubkey == dummy_spk {
                return Some(index);
            }
        }
        None
    }

    /// Simulates a transaction based on the provided `TransactionTemplate`
    ///
    /// This function processes the transaction template to estimate whether the
    /// transaction can be successfully executed, including whether it is spendable
    /// and if it requires change. It also calculates the estimated weight of the
    /// transaction in weight units (WU).
    ///
    /// # Arguments
    ///
    /// * `tx_template` - A `TransactionTemplate` containing the details of the
    ///   transaction to be simulated.
    ///
    /// # Returns
    ///
    /// A `Result` containing:
    /// - A tuple with three elements:
    ///   - A boolean indicating if the transaction is spendable.
    ///   - A boolean indicating if the transaction has change.
    ///   - An estimated weight of the transaction in weight units (WU).
    ///
    /// # Errors
    ///
    /// This function can return an error in the following cases:
    /// - If the transaction processing fails due to invalid outpoints in
    ///   `tx_template.inputs`.
    /// - If the provided outpoints do not match any available coins in the coin store
    /// - If the addresses in `tx_template.outputs` cannot be parsed into valid
    ///   `bitcoin::Address` instances.
    /// - If the parsed addresses are not valid for the current network.
    /// - If the total inputs amount is less than the total outputs amount, making
    ///   the transaction invalid.
    /// - If there is an issue while handling the transaction history or if the
    ///   transaction cannot be created for any other reason.
    ///
    /// # Note
    ///
    /// This function is intended for use in scenarios where users need to gather
    /// information about the transaction before finalizing it, allowing for
    /// adjustments based on the simulation results.
    pub fn simulate_transaction(&self, tx_template: TransactionTemplate) -> TransactionSimulation {
        let (inputs, outputs, has_change) = match self.process_transaction(&tx_template) {
            Ok(r) => r,
            Err(e) => {
                return TransactionSimulation {
                    spendable: false,
                    has_change: false,
                    estimated_weight: 0,
                    error: e,
                }
            }
        };
        let tx = Self::assembly_tx(&inputs, &outputs);
        let estimated_weight = match self.tx_estimated_weight(&tx) {
            Ok(ew) => ew,
            Err(e) => {
                return TransactionSimulation {
                    spendable: false,
                    has_change: false,
                    estimated_weight: 0,
                    error: format!("{e:?}"),
                }
            }
        };
        TransactionSimulation {
            spendable: true,
            has_change,
            estimated_weight,
            error: String::new(),
        }
    }

    /// Prepares a PSBT from a given `TransactionTemplate`.
    ///
    /// This function processes the provided transaction template to create a
    /// PSBT that is ready for signing.
    /// It gathers the necessary inputs and outputs based on the transaction
    /// template and populates the PSBT
    /// with the appropriate descriptors for each input and output.
    ///
    /// # Arguments
    ///
    /// * `tx_template` - A `TransactionTemplate` containing the details of
    ///   the transaction to be processed.
    ///   This includes inputs, outputs, and fee information.
    ///
    /// # Returns
    ///
    /// A `Box<PsbtResult>` containing the following:
    /// - `Ok(psbt)` - A successfully created PSBT as a string.
    /// - `Err(String)` - An error message if the PSBT could not be created.
    ///
    /// # Errors
    ///
    /// This function can return an error in the following cases:
    /// - If the provided `TransactionTemplate` is invalid or does not contain
    ///   the necessary information.
    /// - If the inputs specified in the transaction template do not match any
    ///   available coins in the coin store.
    /// - If the outputs specified in the transaction template cannot be parsed
    ///   into valid Bitcoin addresses.
    /// - If the total input amount is less than the total output amount, making
    ///   the transaction invalid.
    /// - If the fee specified is greater than the available reserves, preventing
    ///   the transaction from being processed.
    /// - If there is an issue while creating the PSBT from the unsigned transaction,
    ///   such as invalid input data.
    /// - If the function encounters any unexpected errors during processing.
    ///
    /// # Note
    ///
    /// This function is intended for use in scenarios where users need to prepare
    /// a transaction for signing, allowing for adjustments based on the provided
    /// transaction template before finalizing the transaction.
    pub fn prepare_transaction(
        &mut self,
        tx_template: TransactionTemplate,
    ) -> Result<String /* PSBT */, String /* error */> {
        let (inputs, mut outputs, change) = self.process_transaction(&tx_template)?;

        // if there is a change, we replace the dummy spk by a freshly generated spk
        if change {
            let dummy_spk = self.dummy_spk();
            for (txout, _) in &mut outputs {
                if txout.script_pubkey == dummy_spk {
                    txout.script_pubkey = self.new_change_addr().script_pubkey();
                }
            }
        }

        let tx = Self::assembly_tx(&inputs, &outputs);

        let mut psbt = bitcoin::Psbt::from_unsigned_tx(tx)
            .map_err(|_| "Fail to generate PSBT from unsigned transaction".to_string())?;

        let descriptor = self.config.descriptor.clone();
        let mut descriptors = descriptor
            .into_single_descriptors()
            .expect("have a multipath")
            .into_iter();
        let receive = descriptors.next().expect("have a multipath");
        let change = descriptors.next().expect("have a multipath");

        // Populate PSBT inputs
        for (index, coin) in inputs.iter().enumerate() {
            // NOTE: for now we consider all inputs to be owned by this descriptor
            let (account, addr_index) = coin.deriv();
            let descriptor = match account {
                AddrAccount::Receive => &receive,
                AddrAccount::Change => &change,
            };
            let definite = descriptor
                .at_derivation_index(addr_index)
                .expect("has a wildcard");
            if let Err(e) = psbt.update_input_with_descriptor(index, &definite) {
                log::error!("fail to update PSBT input w/ descriptor: {e}");
                return Err("Fail to update PSBT input with descriptor".into());
            }

            let input = psbt.inputs.get_mut(index).expect("valid index");

            // TODO: allow sighash to be passed in the TransactionTemplate
            input.sighash_type = Some(EcdsaSighashType::All.into());

            input.witness_utxo = Some(coin.txout());
        }

        // Populate PSBT outputs
        for (index, (_, deriv)) in outputs.iter().enumerate() {
            if let Some((account, addr_index)) = deriv {
                let descriptor = match *account {
                    AddrAccount::Receive => &receive,
                    AddrAccount::Change => &change,
                };
                let definite = descriptor
                    .at_derivation_index(*addr_index)
                    .expect("has a wildcard");
                if let Err(e) = psbt.update_output_with_descriptor(index, &definite) {
                    log::error!("fail to update PSBT output w/ descriptor: {e}");
                    return Err("Fail to update PSBT output with descriptor".into());
                }
            }
        }
        Ok(psbt.to_string())
    }

    /// Sets the Electrum server URL and port for the account.
    ///
    /// # Arguments
    ///
    /// * `url` - The URL of the Electrum server.
    /// * `port` - The port of the Electrum server.
    pub fn set_electrum(&mut self, url: String, port: String) {
        if let Ok(port) = port.parse::<u16>() {
            self.config.electrum_url = Some(url);
            self.config.electrum_port = Some(port);
            self.config.to_file();
        } else {
            self.sender
                .send(Notification::InvalidElectrumConfig)
                .expect("cannot fail");
        }
    }

    /// Starts the Electrum listener for the account.
    pub fn start_electrum(&mut self) {
        if let (None, Some(addr), Some(port)) = (
            &self.tx_listener,
            self.config.electrum_url.clone(),
            self.config.electrum_port,
        ) {
            let (tx_listener, electrum_stop) =
                self.start_listen_txs(addr, port, self.config.clone());
            self.coin_store.lock().expect("poisoned").init(tx_listener);
            self.electrum_stop = Some(electrum_stop);
        }
    }

    /// Stops the Electrum listener for the account.
    pub fn stop_electrum(&mut self) {
        if let Some(stop) = self.electrum_stop.as_mut() {
            stop.store(true, Ordering::Relaxed);
        }
        self.electrum_stop = None;
    }

    /// Sets the look-ahead value for the account.
    ///
    /// # Arguments
    ///
    /// * `look_ahead` - The look-ahead value to set.
    pub fn set_look_ahead(&mut self, look_ahead: String) {
        if let Ok(la) = look_ahead.parse::<u32>() {
            self.config.look_ahead = la;
            self.config.to_file();
        } else {
            self.sender
                .send(Notification::InvalidLookAhead)
                .expect("cannot fail");
        }
    }

    /// Returns the configuration of the account.
    ///
    /// # Returns
    ///
    /// A boxed `Config` instance.
    pub fn get_config(&self) -> Config {
        self.config.clone()
    }

    /// Returns the receiving address at the specified index as a string.
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the receiving address.
    ///
    /// # Returns
    ///
    /// The receiving address as a string.
    pub fn recv_addr_at(&self, index: u32) -> String {
        self.coin_store
            .lock()
            .expect("poisoned")
            .derivator_ref()
            .receive_at(index)
            .to_string()
    }

    /// Returns the change address at the specified index as a string.
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the change address.
    ///
    /// # Returns
    ///
    /// The change address as a string.
    pub fn change_addr_at(&self, index: u32) -> String {
        self.coin_store
            .lock()
            .expect("poisoned")
            .derivator_ref()
            .change_at(index)
            .to_string()
    }

    /// Generates a new receiving address entry for the account.
    ///
    /// # Returns
    ///
    /// A boxed `AddressEntry` instance.
    pub fn new_addr(&mut self) -> RustAddress {
        let addr = self.new_recv_addr();
        let index = self.coin_store.lock().expect("poisoned").recv_tip();
        AddressEntry {
            status: AddressStatus::NotUsed,
            address: addr.as_unchecked().clone(),
            account: AddrAccount::Receive,
            index,
        }
        .into()
    }

    /// Edits the label of a coin identified by the given outpoint.
    ///
    /// # Arguments
    ///
    /// * `outpoint` - A string representation of the outpoint for the coin.
    /// * `label` - The new label to set for the coin. If the label is empty, the label will be removed.
    pub fn edit_coin_label(&self, outpoint: String, label: String) {
        if let Ok(outpoint) = bitcoin::OutPoint::from_str(&outpoint) {
            if !label.is_empty() {
                self.label_store
                    .lock()
                    .expect("poisoned")
                    .edit(LabelKey::OutPoint(outpoint), Some(label));
            } else {
                self.label_store
                    .lock()
                    .expect("poisoned")
                    .remove(LabelKey::OutPoint(outpoint));
            }
        }
        if let Ok(mut store) = self.coin_store.try_lock() {
            store.generate();
        }
    }

    pub fn sign(&self, psbt: String) {
        self.signing_manager.sign(self.config.network(), psbt);
    }
}

// /// Creates a new account with the specified account name.
// ///
// /// # Arguments
// ///
// /// * `account` - The name of the account.
// ///
// /// # Returns
// ///
// /// A boxed `Account` instance.
// pub fn new_account(account: String) -> Box<Account> {
//     let config = Config::from_file(account);
//
//     let account = Account::new(config);
//     account.boxed()
// }

macro_rules! send_notif {
    ($notification:expr, $request:expr, $msg:expr) => {
        let res = $notification.send($msg.into());
        if res.is_err() {
            // stop detached client
            let _ = $request.send(CoinRequest::Stop);
            return;
        }
    };
}

macro_rules! send_electrum {
    ($request:expr, $notification:expr, $msg:expr) => {
        if $request.send($msg).is_err() {
            send_notif!($notification, $request, TxListenerNotif::Stopped);
            return;
        }
    };
}

/// Listens for transactions on the specified address and port.
///
/// # Arguments
///
/// * `addr` - The address to listen on.
/// * `port` - The port to listen on.
/// * `coin_store` - The coin store to update with transaction data.
/// * `signer` - The signer for the account.
/// * `notification` - The sender for notifications.
/// * `address_tip` - The receiver for address tips.
/// * `stop_request` - The stop flag for the listener.
#[allow(clippy::too_many_arguments)]
fn listen_txs<T: From<TxListenerNotif>>(
    coin_store: Arc<Mutex<CoinStore>>,
    derivator: Derivator,
    notification: mpsc::Sender<T>,
    address_tip: mpsc::Receiver<AddressTip>,
    stop_request: Arc<AtomicBool>,
    request: mpsc::Sender<CoinRequest>,
    response: mpsc::Receiver<CoinResponse>,
    config: Option<Config>,
) {
    log::info!("listen_txs(): started");
    send_notif!(notification, request, TxListenerNotif::Started);

    let mut statuses = if let Some(config) = &config {
        config.statuses_from_file()
    } else {
        BTreeMap::<ScriptBuf, (Option<String>, u32, u32)>::new()
    };

    if !statuses.is_empty() {
        let sub: Vec<_> = statuses.keys().cloned().collect();
        send_electrum!(request, notification, CoinRequest::Subscribe(sub));
    }

    fn persist_status(
        config: &Option<Config>,
        statuses: &BTreeMap<ScriptBuf, (Option<String>, u32, u32)>,
    ) {
        if let Some(cfg) = config.as_ref() {
            cfg.persist_statuses(statuses);
        }
    }

    let mut backoff = Backoff::new_ms(20);
    loop {
        // stop request from consumer side
        if stop_request.load(Ordering::Relaxed) {
            send_notif!(notification, request, TxListenerNotif::Stopped);
            let _ = request.send(CoinRequest::Stop);
            return;
        }

        let mut received = false;

        // listen for AddressTip update
        match address_tip.try_recv() {
            Ok(tip) => {
                log::debug!("listen_txs() receive {tip:?}");
                let AddressTip { recv, change } = tip;
                received = true;
                let mut sub = vec![];
                let r_spk = derivator.receive_at(recv).script_pubkey();
                if !statuses.contains_key(&r_spk) {
                    // FIXME: here we can be smart an not start at 0 but at `actual_tip`
                    for i in 0..recv {
                        let spk = derivator.receive_at(i).script_pubkey();
                        if !statuses.contains_key(&spk) {
                            statuses.insert(spk.clone(), (None, 0, i));
                            persist_status(&config, &statuses);
                            sub.push(spk);
                        }
                    }
                }
                let c_spk = derivator.change_at(recv).script_pubkey();
                if !statuses.contains_key(&c_spk) {
                    // FIXME: here we can be smart an not start at 0 but at `actual_tip`
                    for i in 0..change {
                        let spk = derivator.change_at(i).script_pubkey();
                        if !statuses.contains_key(&spk) {
                            statuses.insert(spk.clone(), (None, 1, i));
                            persist_status(&config, &statuses);
                            sub.push(spk);
                        }
                    }
                }
                if !sub.is_empty() {
                    send_electrum!(request, notification, CoinRequest::Subscribe(sub));
                }
            }
            Err(e) => match e {
                mpsc::TryRecvError::Empty => {}
                mpsc::TryRecvError::Disconnected => {
                    log::error!("listen_txs(): address store disconnected");
                    send_notif!(
                        notification,
                        request,
                        TxListenerNotif::Error("AddressStore disconnected".to_string())
                    );
                    // FIXME: what should we do there?
                    // it's AddressStore being dropped, but she should keep upating
                    // the actual spk set even if it cannot grow anymore
                }
            },
        }

        // listen for response
        match response.try_recv() {
            Ok(rsp) => {
                log::debug!("listen_txs() receive {rsp:#?}");
                received = true;
                match rsp {
                    CoinResponse::Status(elct_status) => {
                        let mut history = vec![];
                        for (spk, status) in elct_status {
                            if let Some((s, _, _)) = statuses.get_mut(&spk) {
                                // status is registered
                                if *s != status {
                                    // status changed
                                    if status.is_some() {
                                        // status is not empty so we ask for txs changes
                                        history.push(spk);
                                    } else {
                                        // status change from Some(_) to None we directly update
                                        // coin_store
                                        let mut store = coin_store.lock().expect("poisoned");
                                        let mut map = BTreeMap::new();
                                        map.insert(spk.clone(), vec![]);
                                        let _ = store.handle_history_response(map);
                                        store.generate();
                                    }
                                    // record the local status change
                                    *s = status;
                                }
                            } else if status.is_some() {
                                // status is not None & not registered
                                statuses.entry(spk.clone()).and_modify(|s| s.0 = status);
                                persist_status(&config, &statuses);
                                history.push(spk);
                            } else {
                                // status is None & not registered

                                // record local status
                                statuses.entry(spk.clone()).and_modify(|s| s.0 = status);
                                persist_status(&config, &statuses);

                                // update coin_store
                                let mut store = coin_store.lock().expect("poisoned");
                                let mut map = BTreeMap::new();
                                map.insert(spk.clone(), vec![]);
                                let _ = store.handle_history_response(map);
                            }
                        }
                        if !history.is_empty() {
                            let hist = CoinRequest::History(history);
                            log::debug!("listen_txs() send {:#?}", hist);
                            send_electrum!(request, notification, hist);
                        }
                        persist_status(&config, &statuses);
                    }
                    CoinResponse::History(map) => {
                        let mut store = coin_store.lock().expect("poisoned");
                        let (height_updated, missing_txs) = store.handle_history_response(map);
                        if !missing_txs.is_empty() {
                            send_electrum!(request, notification, CoinRequest::Txs(missing_txs));
                        }
                        if height_updated {
                            store.generate();
                        }
                    }
                    CoinResponse::Txs(txs) => {
                        let mut store = coin_store.lock().expect("poisoned");
                        store.handle_txs_response(txs);
                    }
                    CoinResponse::Stopped => {
                        send_notif!(notification, request, TxListenerNotif::Stopped);
                        let _ = request.send(CoinRequest::Stop);
                        return;
                    }
                    CoinResponse::Error(e) => {
                        send_notif!(notification, request, TxListenerNotif::Error(e));
                    }
                }
            }
            Err(e) => match e {
                mpsc::TryRecvError::Empty => {}
                mpsc::TryRecvError::Disconnected => {
                    // NOTE: here the electrum client is dropped, we cannot continue
                    log::error!("listen_txs() electrum client stopped unexpectedly");
                    send_notif!(notification, request, TxListenerNotif::Stopped);
                    let _ = request.send(CoinRequest::Stop);
                    return;
                }
            },
        }

        if received {
            continue;
        }
        backoff.snooze();
    }
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::mpsc::TryRecvError, time::Duration};

    use {bip39, miniscript::bitcoin::bip32::DerivationPath};

    use crate::{
        descriptor::wpkh,
        signer::HotSigner,
        test_utils::{funding_tx, setup_logger, spending_tx},
        tx_store::TxStore,
    };

    use super::*;

    struct CoinStoreMock {
        pub store: Arc<Mutex<CoinStore>>,
        pub notif: mpsc::Receiver<Notification>,
        pub request: mpsc::Receiver<CoinRequest>,
        pub response: mpsc::Sender<CoinResponse>,
        pub listener: JoinHandle<()>,
        pub stop: Arc<AtomicBool>,
        pub derivator: Derivator,
    }

    impl Drop for CoinStoreMock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    impl CoinStoreMock {
        fn new(recv_tip: u32, change_tip: u32, look_ahead: u32) -> Self {
            let (notif_sender, notif_recv) = mpsc::channel();
            let (tip_sender, tip_receiver) = mpsc::channel();
            let (req_sender, req_receiver) = mpsc::channel();
            let (resp_sender, resp_receiver) = mpsc::channel();

            let mnemonic = bip39::Mnemonic::generate(12).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let signer =
                HotSigner::new_from_mnemonics(bitcoin::Network::Regtest, &mnemonic.to_string())
                    .unwrap();
            let xpub = signer.xpub(&DerivationPath::from_str("m/84'/0'/0'/1").unwrap());
            let descriptor = wpkh(xpub);
            let derivator = Derivator::new(descriptor.clone(), bitcoin::Network::Regtest).unwrap();

            let tx_store = TxStore::new(Default::default(), None);
            let label_store = Arc::new(Mutex::new(LabelStore::new()));
            let coin_store = Arc::new(Mutex::new(CoinStore::new(
                bitcoin::Network::Regtest,
                descriptor.clone(),
                notif_sender.clone(),
                recv_tip,
                change_tip,
                look_ahead,
                tx_store,
                label_store,
                None,
            )));
            coin_store.lock().expect("poisoned").init(tip_sender);
            let store = coin_store.clone();
            let cloned_stop = stop.clone();
            let cloned_derivator = derivator.clone();

            let listener_handle = thread::spawn(move || {
                listen_txs(
                    coin_store,
                    cloned_derivator,
                    notif_sender,
                    tip_receiver,
                    stop,
                    req_sender,
                    resp_receiver,
                    None,
                );
            });

            CoinStoreMock {
                store,
                notif: notif_recv,
                request: req_receiver,
                response: resp_sender,
                listener: listener_handle,
                stop: cloned_stop,
                derivator,
            }
        }

        fn coins(&mut self) -> BTreeMap<OutPoint, CoinEntry> {
            self.store.lock().expect("poisoned").coins()
        }

        fn stop(&self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn simple_start_stop() {
        setup_logger();
        let mock = CoinStoreMock::new(0, 0, 20);
        thread::sleep(Duration::from_millis(10));
        assert!(!mock.listener.is_finished());
        assert!(matches!(
            mock.notif.try_recv().unwrap(),
            Notification::AddressTipChanged,
        ));
        assert!(matches!(
            mock.notif.try_recv().unwrap(),
            Notification::Electrum(TxListenerNotif::Started)
        ));
        mock.stop();
        thread::sleep(Duration::from_secs(1));
        assert!(mock.listener.is_finished());
    }

    fn simple_recv() -> (bitcoin::Transaction, CoinStoreMock) {
        setup_logger();
        let look_ahead = 5;
        let mut mock = CoinStoreMock::new(0, 0, look_ahead);
        thread::sleep(Duration::from_millis(500));
        assert!(!mock.listener.is_finished());
        assert!(matches!(
            mock.notif.try_recv().unwrap(),
            Notification::AddressTipChanged,
        ));
        assert!(matches!(
            mock.notif.try_recv().unwrap(),
            Notification::Electrum(TxListenerNotif::Started)
        ));

        let mut init_spks = vec![];
        for i in 0..(look_ahead + 1) {
            let spk = mock.derivator.receive_spk_at(i);
            init_spks.push(spk);
        }
        for i in 0..(look_ahead + 1) {
            let spk = mock.derivator.change_spk_at(i);
            init_spks.push(spk);
        }

        // receive initial subscriptions
        if let Ok(CoinRequest::Subscribe(v)) = mock.request.try_recv() {
            // NOTE: we expect (tip + 1 + look_ahead )
            assert_eq!(v.len(), 12);
            for spk in &init_spks {
                assert!(v.contains(spk));
            }
        } else {
            panic!()
        }

        // electrum server send spks statuses (None)
        let statuses: BTreeMap<_, _> = init_spks.clone().into_iter().map(|s| (s, None)).collect();
        mock.response.send(CoinResponse::Status(statuses)).unwrap();

        thread::sleep(Duration::from_millis(100));

        assert!(mock.coins().is_empty());

        let spk_recv_0 = mock.derivator.receive_spk_at(0);

        // server send a status update at recv(0)
        let mut statuses = BTreeMap::new();
        statuses.insert(spk_recv_0.clone(), Some("1_tx_unco".to_string()));
        mock.response.send(CoinResponse::Status(statuses)).unwrap();
        thread::sleep(Duration::from_millis(100));

        // server should receive an history request for this spk
        if let Ok(CoinRequest::History(v)) = mock.request.try_recv() {
            assert!(v == vec![spk_recv_0.clone()]);
        } else {
            panic!()
        }

        thread::sleep(Duration::from_millis(100));

        let tx_0 = funding_tx(spk_recv_0.clone(), 0.1);

        // server must send history response
        let mut history = BTreeMap::new();
        history.insert(spk_recv_0.clone(), vec![(tx_0.compute_txid(), None)]);
        mock.response.send(CoinResponse::History(history)).unwrap();

        thread::sleep(Duration::from_millis(100));

        // server should receive a tx request
        if let Ok(CoinRequest::Txs(v)) = mock.request.try_recv() {
            assert!(v == vec![tx_0.compute_txid()]);
        } else {
            panic!()
        }

        thread::sleep(Duration::from_millis(100));

        // server send the requested tx
        mock.response
            .send(CoinResponse::Txs(vec![tx_0.clone()]))
            .unwrap();

        thread::sleep(Duration::from_millis(100));

        // now the store contain one coin
        let mut coins = mock.coins();
        assert_eq!(coins.len(), 1);
        let coin = coins.pop_first().unwrap().1;

        // the coin is unconfirmed
        assert_eq!(coin.height(), None);
        assert_eq!(coin.status(), CoinStatus::Unconfirmed);

        // NOTE: the coin is now confirmed

        // server send a status update at recv(0)
        let mut statuses = BTreeMap::new();
        statuses.insert(spk_recv_0.clone(), Some("1_tx_conf".to_string()));
        mock.response.send(CoinResponse::Status(statuses)).unwrap();
        thread::sleep(Duration::from_millis(100));

        // server should receive an history request for this spk
        if let Ok(CoinRequest::History(v)) = mock.request.try_recv() {
            assert!(v == vec![spk_recv_0.clone()]);
        } else {
            panic!()
        }

        thread::sleep(Duration::from_millis(100));

        // server must send history response
        let mut history = BTreeMap::new();
        // the coin have now 1 confirmation
        history.insert(spk_recv_0.clone(), vec![(tx_0.compute_txid(), Some(1))]);
        mock.response.send(CoinResponse::History(history)).unwrap();

        thread::sleep(Duration::from_millis(100));

        // NOTE: coin_store already have the tx it should not ask it
        assert!(matches!(mock.request.try_recv(), Err(TryRecvError::Empty)));

        // the coin is now confirmed
        let mut coins = mock.coins();
        assert_eq!(coins.len(), 1);
        let coin = coins.pop_first().unwrap().1;
        assert_eq!(coin.height(), Some(1));
        assert_eq!(coin.status(), CoinStatus::Confirmed);
        (tx_0, mock)
    }

    #[test]
    fn recv_and_spend() {
        // init & receive one coin
        let (tx_0, mut mock) = simple_recv();
        let spk_recv_0 = mock.derivator.receive_spk_at(0);

        // spend this coin
        let outpoint = mock.coins().pop_first().unwrap().0;
        let tx_1 = spending_tx(outpoint);

        // NOTE: the coin is now spent

        // server send a status update at recv(0)
        let mut statuses = BTreeMap::new();
        statuses.insert(spk_recv_0.clone(), Some("1_tx_spent".to_string()));
        mock.response.send(CoinResponse::Status(statuses)).unwrap();
        thread::sleep(Duration::from_millis(100));

        // server should receive an history request for this spk
        if let Ok(CoinRequest::History(v)) = mock.request.try_recv() {
            assert!(v == vec![spk_recv_0.clone()]);
        } else {
            panic!()
        }

        thread::sleep(Duration::from_millis(100));

        // server must send history response
        let mut history = BTreeMap::new();
        // the coin have now 1 confirmation
        history.insert(
            spk_recv_0.clone(),
            vec![(tx_0.compute_txid(), Some(1)), (tx_1.compute_txid(), None)],
        );
        mock.response.send(CoinResponse::History(history)).unwrap();

        thread::sleep(Duration::from_millis(100));

        // server should receive a tx request only for tx_1
        if let Ok(CoinRequest::Txs(v)) = mock.request.try_recv() {
            assert!(v == vec![tx_1.compute_txid()]);
        } else {
            panic!()
        }

        // server send the requested tx
        mock.response
            .send(CoinResponse::Txs(vec![tx_1.clone()]))
            .unwrap();

        thread::sleep(Duration::from_millis(100));

        // now the store contain one spent coin
        let mut coins = mock.coins();
        assert_eq!(coins.len(), 1);
        let coin = coins.pop_first().unwrap().1;

        // the coin is unconfirmed
        assert_eq!(coin.status(), CoinStatus::Spent);
    }

    #[test]
    fn simple_reorg() {
        // init & receive one coin
        let (tx_0, mut mock) = simple_recv();
        let spk_recv_0 = mock.derivator.receive_spk_at(0);

        // NOTE: the coin is now spent we can reorg it

        // server send a status update at recv(0)
        let mut statuses = BTreeMap::new();
        statuses.insert(spk_recv_0.clone(), Some("1_tx_reorg".to_string()));
        mock.response.send(CoinResponse::Status(statuses)).unwrap();
        thread::sleep(Duration::from_millis(100));

        // server should receive an history request for this spk
        if let Ok(CoinRequest::History(v)) = mock.request.try_recv() {
            assert!(v == vec![spk_recv_0.clone()]);
        } else {
            panic!()
        }

        thread::sleep(Duration::from_millis(100));

        // server must send history response
        let mut history = BTreeMap::new();
        // NOTE: confirmation height is changed to 2
        history.insert(spk_recv_0.clone(), vec![(tx_0.compute_txid(), Some(2))]);
        mock.response.send(CoinResponse::History(history)).unwrap();

        thread::sleep(Duration::from_millis(100));

        // server do not receive a tx request as the store already go the tx
        assert!(matches!(mock.request.try_recv(), Err(TryRecvError::Empty)));

        // the store still contain one spent coin
        let mut coins = mock.coins();
        assert_eq!(coins.len(), 1);
        let coin = coins.pop_first().unwrap().1;

        // the coin have a confirmation height of 2
        assert_eq!(coin.height(), Some(2));
    }
}
