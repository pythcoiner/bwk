//! RecipientProvider implementations for Silent Payment types.
//!
//! This module implements bwk-tx's RecipientProvider trait for SP types.
//! Uses newtype wrappers to satisfy the orphan rule.

use spdk_core::bitcoin::key::TapTweak;
use spdk_core::bitcoin::{ScriptBuf, TxOut, Weight};
use spdk_core::silentpayments::SilentPaymentAddress;
use spdk_core::{OwnedOutput, RecipientAddress};

use bwk_tx::{
    transaction::Amount, Error as TxError, FinalizationContext, PsbtOutputInfo, RecipientProvider,
};

const TR_OUTPUT_WEIGHT: u64 = 172;

use spdk_core::bitcoin::Network;

#[derive(Debug, Clone)]
pub struct SpRecipient {
    /// The silent payment address
    pub address: SilentPaymentAddress,
    /// Amount to send
    pub amount: Amount,
    /// Optional label index for BIP375
    pub label: Option<u32>,
    /// Network
    pub network: Network,
}

impl SpRecipient {
    /// Create a new SpRecipient from a SilentPaymentAddress
    pub fn new(address: SilentPaymentAddress, amount: u64, network: Network) -> Self {
        Self {
            address,
            amount: Amount::Value(amount),
            label: None,
            network,
        }
    }

    /// Create a new SpRecipient with a label
    pub fn with_label(
        address: SilentPaymentAddress,
        amount: u64,
        label: u32,
        network: Network,
    ) -> Self {
        Self {
            address,
            amount: Amount::Value(amount),
            label: Some(label),
            network,
        }
    }
}

impl RecipientProvider for SpRecipient {
    fn output_weight(&self) -> Weight {
        // SP outputs are always P2TR
        Weight::from_wu(TR_OUTPUT_WEIGHT)
    }

    fn create_script(&self, ctx: &FinalizationContext) -> ScriptBuf {
        let partial_secret = ctx
            .partial_secret
            .expect("SP output requires partial_secret in FinalizationContext");

        // Generate output pubkey using silentpayments crate
        let pubkeys =
            silentpayments::sending::generate_recipient_pubkeys(vec![self.address], partial_secret)
                .expect("failed to generate SP recipient pubkeys");

        let output_pubkeys = pubkeys
            .get(&self.address)
            .expect("missing pubkey for SP address");

        let pubkey = output_pubkeys[0];
        ScriptBuf::new_p2tr_tweaked(pubkey.dangerous_assume_tweaked())
    }

    fn psbt_output_info(&self) -> PsbtOutputInfo {
        PsbtOutputInfo::SilentPayment {
            scan_pubkey: self.address.get_scan_key(),
            spend_pubkey: self.address.get_spend_key(),
            label: self.label,
        }
    }

    fn is_silent_payment(&self) -> bool {
        true
    }

    fn amount(&self) -> Amount {
        self.amount.clone()
    }

    fn set_amount(&mut self, amount: Amount) {
        self.amount = amount;
    }

    fn network(&self) -> Network {
        self.network
    }
}

#[derive(Debug, Clone)]
pub struct SpRecipientAddress {
    pub inner: RecipientAddress,
    pub amount: Amount,
    pub network: Network,
}

impl SpRecipientAddress {
    /// Create a new SpRecipientAddress with an amount
    pub fn new(addr: RecipientAddress, amount: u64, network: Network) -> Self {
        Self {
            inner: addr,
            amount: Amount::Value(amount),
            network,
        }
    }

    /// Create from a SilentPaymentAddress
    pub fn from_sp(addr: SilentPaymentAddress, amount: u64, network: Network) -> Self {
        Self {
            inner: RecipientAddress::SpAddress(addr),
            amount: Amount::Value(amount),
            network,
        }
    }
}

impl RecipientProvider for SpRecipientAddress {
    fn output_weight(&self) -> Weight {
        match &self.inner {
            RecipientAddress::SpAddress(_) => Weight::from_wu(TR_OUTPUT_WEIGHT),
            RecipientAddress::LegacyAddress(addr) => {
                let script = addr.clone().assume_checked().script_pubkey();
                TxOut {
                    value: spdk_core::bitcoin::Amount::MAX_MONEY,
                    script_pubkey: script,
                }
                .weight()
            }
            RecipientAddress::Data(data) => {
                // OP_RETURN: OP_RETURN (1) + push (1-2) + data
                let script_len = 1 + 1 + data.len().min(80);
                // output = 8 (value) + 1 (varint) + script_len
                let output_size = 8 + 1 + script_len;
                Weight::from_wu((output_size * 4) as u64)
            }
        }
    }

    fn create_script(&self, ctx: &FinalizationContext) -> ScriptBuf {
        match &self.inner {
            RecipientAddress::SpAddress(sp) => {
                let partial_secret = ctx
                    .partial_secret
                    .expect("SP output requires partial_secret");

                let pubkeys =
                    silentpayments::sending::generate_recipient_pubkeys(vec![*sp], partial_secret)
                        .expect("failed to generate SP recipient pubkeys");

                let output_pubkeys = pubkeys.get(sp).expect("missing pubkey for SP address");

                let pubkey = output_pubkeys[0];
                ScriptBuf::new_p2tr_tweaked(pubkey.dangerous_assume_tweaked())
            }
            RecipientAddress::LegacyAddress(addr) => addr.clone().assume_checked().script_pubkey(),
            RecipientAddress::Data(data) => {
                use spdk_core::bitcoin::script::PushBytesBuf;
                let mut op_return = PushBytesBuf::with_capacity(data.len());
                op_return
                    .extend_from_slice(data)
                    .expect("data too large for OP_RETURN");
                ScriptBuf::new_op_return(op_return)
            }
        }
    }

    fn psbt_output_info(&self) -> PsbtOutputInfo {
        match &self.inner {
            RecipientAddress::SpAddress(sp) => PsbtOutputInfo::SilentPayment {
                scan_pubkey: sp.get_scan_key(),
                spend_pubkey: sp.get_spend_key(),
                label: None,
            },
            _ => PsbtOutputInfo::None,
        }
    }

    fn is_silent_payment(&self) -> bool {
        matches!(self.inner, RecipientAddress::SpAddress(_))
    }

    fn amount(&self) -> Amount {
        self.amount.clone()
    }

    fn set_amount(&mut self, amount: Amount) {
        self.amount = amount;
    }

    fn network(&self) -> Network {
        self.network
    }
}

//=============================================================================
// SpPartialSecretProvider for Account
//=============================================================================

use bwk_tx::{Coin, SpPartialSecretProvider};
use spdk_core::bitcoin::OutPoint;

use crate::Account;

impl SpPartialSecretProvider for Account {
    fn compute_partial_secret(
        &self,
        inputs: &[Coin],
    ) -> Result<spdk_core::bitcoin::secp256k1::SecretKey, TxError> {
        // Extract (OutPoint, OwnedOutput) from our coin store
        let selected_utxos: Vec<(OutPoint, OwnedOutput)> = inputs
            .iter()
            .map(|coin| {
                let entry = self.get_coin(&coin.outpoint).ok_or(TxError::CoinNotFound)?;
                Ok((coin.outpoint, entry.owned_output().clone()))
            })
            .collect::<Result<Vec<_>, TxError>>()?;

        self.sp_client()
            .get_partial_secret_for_selected_utxos(&selected_utxos)
            .map_err(|_| TxError::SpPartialSecret)
    }
}
