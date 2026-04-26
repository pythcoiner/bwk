//! Pre-sign PSBT validation against an Electrum server.
//!
//! Inspects the PSBT's outputs for already-used scripts and the inputs for
//! outpoints that have already been spent, returning a structured report.
//! Per-entry Electrum failures are recorded as warnings and do not abort the
//! whole call — the [`Result`] is reserved for errors that prevent the check
//! from running at all.

use std::collections::HashMap;

use miniscript::bitcoin::{OutPoint, Psbt, ScriptBuf, Transaction, Txid};

use crate::client::{Client, Error};

/// Report produced by [`Client::validate_psbt`].
#[derive(Debug, Clone, Default)]
pub struct PsbtValidationReport {
    /// Outputs whose script has already received funds at least once.
    pub reused_outputs: Vec<ReusedOutput>,
    /// Inputs whose outpoint has already been spent by another transaction.
    pub spent_inputs: Vec<SpentInput>,
    /// Non-fatal per-entry Electrum failures (kept so UIs can still warn).
    pub warnings: Vec<String>,
}

impl PsbtValidationReport {
    /// Returns true if no reused outputs and no already-spent inputs were found.
    pub fn is_clean(&self) -> bool {
        self.reused_outputs.is_empty() && self.spent_inputs.is_empty()
    }
}

/// One PSBT output whose script_pubkey has prior on-chain history.
#[derive(Debug, Clone)]
pub struct ReusedOutput {
    /// Index of the output in `psbt.unsigned_tx.output`.
    pub index: usize,
    /// The reused script_pubkey; the caller picks how to display it.
    pub script_pubkey: ScriptBuf,
}

/// One PSBT input whose outpoint has already been consumed by a different transaction.
#[derive(Debug, Clone)]
pub struct SpentInput {
    /// Index of the input in `psbt.unsigned_tx.input`.
    pub index: usize,
    /// The previously-spent outpoint.
    pub outpoint: OutPoint,
    /// Txid of the transaction that already spent the outpoint.
    pub spending_txid: Txid,
}

/// Error returned when [`Client::validate_psbt`] cannot run the check at all.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    /// Underlying Electrum client error not tied to a specific input/output.
    #[error("electrum error: {0}")]
    Electrum(#[from] Error),
}

impl Client {
    /// Pre-sign validation: report reused output scripts and already-spent input outpoints.
    ///
    /// Per-entry Electrum failures populate [`PsbtValidationReport::warnings`] and the
    /// scan continues; only failures that prevent the check from running at all bubble
    /// up as [`ValidationError`].
    pub fn validate_psbt(&mut self, psbt: &Psbt) -> Result<PsbtValidationReport, ValidationError> {
        let mut report = PsbtValidationReport::default();
        let mut tx_cache: HashMap<Txid, Transaction> = HashMap::new();

        // Outputs: any spk already received-to is flagged.
        for (idx, output) in psbt.unsigned_tx.output.iter().enumerate() {
            let spk = &output.script_pubkey;
            if spk.is_op_return() {
                continue;
            }
            match self.get_coins_tx_at(spk) {
                Ok(txids) if !txids.is_empty() => {
                    report.reused_outputs.push(ReusedOutput {
                        index: idx,
                        script_pubkey: spk.clone(),
                    });
                }
                Ok(_) => {}
                Err(e) => {
                    report.warnings.push(format!("output #{idx}: {e}"));
                }
            }
        }

        // Inputs: resolve each input's spk, then look for any history tx that spent it.
        for (idx, input) in psbt.unsigned_tx.input.iter().enumerate() {
            let outpoint = input.previous_output;
            let spk = match resolve_input_spk(self, &mut tx_cache, psbt, idx, outpoint) {
                Ok(s) => s,
                Err(msg) => {
                    report.warnings.push(format!("input #{idx}: {msg}"));
                    continue;
                }
            };

            let txids = match self.get_coins_tx_at(&spk) {
                Ok(t) => t,
                Err(e) => {
                    report.warnings.push(format!("input #{idx}: {e}"));
                    continue;
                }
            };

            for txid in &txids {
                if *txid == outpoint.txid {
                    continue;
                }
                let tx = match fetch_or_cache_tx(self, &mut tx_cache, *txid) {
                    Ok(t) => t,
                    Err(e) => {
                        report
                            .warnings
                            .push(format!("input #{idx}: tx {txid}: {e}"));
                        continue;
                    }
                };
                if tx.input.iter().any(|t| t.previous_output == outpoint) {
                    report.spent_inputs.push(SpentInput {
                        index: idx,
                        outpoint,
                        spending_txid: *txid,
                    });
                    break;
                }
            }
        }

        Ok(report)
    }
}

fn resolve_input_spk(
    client: &mut Client,
    cache: &mut HashMap<Txid, Transaction>,
    psbt: &Psbt,
    idx: usize,
    outpoint: OutPoint,
) -> Result<ScriptBuf, String> {
    let psbt_input = &psbt.inputs[idx];
    if let Some(witness_utxo) = &psbt_input.witness_utxo {
        return Ok(witness_utxo.script_pubkey.clone());
    }
    if let Some(non_witness_utxo) = &psbt_input.non_witness_utxo {
        let vout = outpoint.vout as usize;
        return non_witness_utxo
            .output
            .get(vout)
            .map(|o| o.script_pubkey.clone())
            .ok_or_else(|| format!("non_witness_utxo missing vout {vout}"));
    }
    let tx = fetch_or_cache_tx(client, cache, outpoint.txid)
        .map_err(|e| format!("fetch prev tx {}: {e}", outpoint.txid))?;
    tx.output
        .get(outpoint.vout as usize)
        .map(|o| o.script_pubkey.clone())
        .ok_or_else(|| format!("prev tx {} missing vout {}", outpoint.txid, outpoint.vout))
}

fn fetch_or_cache_tx(
    client: &mut Client,
    cache: &mut HashMap<Txid, Transaction>,
    txid: Txid,
) -> Result<Transaction, Error> {
    if let Some(tx) = cache.get(&txid) {
        return Ok(tx.clone());
    }
    let tx = client.get_tx(txid)?;
    cache.insert(txid, tx.clone());
    Ok(tx)
}
