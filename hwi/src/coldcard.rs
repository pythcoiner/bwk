use std::{
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard},
};

use bitcoin::{
    bip32::{DerivationPath, Fingerprint, Xpub},
    psbt::Psbt,
};
use hidapi::DeviceInfo;

use coldcard::protocol::DescriptorName;

use crate::{parse_version, AddressScript, DeviceKind, Error as HWIError, Version, HWI};
pub use coldcard as api;

#[derive(Debug)]
pub struct Coldcard {
    device: Arc<Mutex<coldcard::Coldcard>>,
    wallet_name: Option<String>,
}

impl Coldcard {
    pub fn with_wallet_name(mut self, wallet_name: String) -> Self {
        self.wallet_name = Some(wallet_name);
        self
    }

    fn device(&self) -> Result<MutexGuard<'_, coldcard::Coldcard>, HWIError> {
        self.device
            .lock()
            .map_err(|_| HWIError::Unexpected("Failed to unlock"))
    }
}

impl From<coldcard::Coldcard> for Coldcard {
    fn from(cc: coldcard::Coldcard) -> Self {
        Coldcard {
            device: Arc::new(Mutex::new(cc)),
            wallet_name: None,
        }
    }
}

impl HWI for Coldcard {
    fn device_kind(&self) -> DeviceKind {
        DeviceKind::Coldcard
    }

    /// The first semver version returned by coldcard is the firmware version.
    fn get_version(&self) -> Result<Version, HWIError> {
        let s = self.device()?.version()?;
        for line in s.split('\n') {
            if let Ok(version) = parse_version(line) {
                return Ok(version);
            }
        }
        Err(HWIError::UnsupportedVersion)
    }

    fn get_master_fingerprint(&self) -> Result<Fingerprint, HWIError> {
        let s = self.device()?.xpub(None)?;
        let xpub = Xpub::from_str(&s).map_err(|e| HWIError::Device(e.to_string()))?;
        Ok(xpub.fingerprint())
    }

    fn get_extended_pubkey(&self, path: &DerivationPath) -> Result<Xpub, HWIError> {
        let path = path.to_string();
        let path = if path.starts_with("m/") {
            path
        } else {
            format!("m/{path}")
        };
        let path = coldcard::protocol::DerivationPath::new(&path)
            .map_err(|e| HWIError::InvalidParameter("path", format!("{e:?}")))?;
        let s = self.device()?.xpub(Some(path))?;
        Xpub::from_str(&s).map_err(|e| HWIError::Device(e.to_string()))
    }

    fn display_address(&self, script: &AddressScript) -> Result<(), HWIError> {
        if let Some(name) = &self.wallet_name {
            let descriptor_name = coldcard::protocol::DescriptorName::new(name)
                .map_err(|_| HWIError::UnsupportedInput)?;
            if let AddressScript::Miniscript { index, change } = script {
                self.device()?
                    .miniscript_address(descriptor_name, *change, *index)?;
                Ok(())
            } else {
                Err(HWIError::UnimplementedMethod)
            }
        } else {
            Err(HWIError::UnimplementedMethod)
        }
    }

    fn register_wallet(&self, name: &str, policy: &str) -> Result<Option<[u8; 32]>, HWIError> {
        let payload = format!("{{\"name\":\"{name}\",\"desc\":\"{policy}\"}}");
        self.device()?.miniscript_enroll(payload.as_bytes())?;
        Ok(None)
    }

    fn is_wallet_registered(&self, name: &str, policy: &str) -> Result<bool, HWIError> {
        let descriptor_name = coldcard::protocol::DescriptorName::new(name)
            .map_err(|_| HWIError::UnsupportedInput)?;
        let desc = self.device()?.miniscript_get(descriptor_name)?;
        if let Some(desc) = desc {
            if let Some((policy, _)) = policy.replace('\'', "h").split_once('#') {
                Ok(desc.contains(policy))
            } else {
                Ok(desc.contains(policy))
            }
        } else {
            Ok(false)
        }
    }

    fn sign_tx(&self, psbt: &mut Psbt) -> Result<(), HWIError> {
        let mut cc = self.device()?;

        let wallet_name = if let Some(name) = self.wallet_name.clone() {
            Some(
                DescriptorName::new(name)
                    .map_err(|_| HWIError::Unexpected("Coldcard: Invalid wallet name"))?,
            )
        } else {
            None
        };

        cc.sign_psbt_miniscript(&psbt.serialize(), api::SignMode::Signed, wallet_name)?;

        let tx = loop {
            if let Some(tx) = cc.get_signed_tx()? {
                break tx;
            }
        };

        let mut new_psbt = Psbt::deserialize(&tx).map_err(|e| HWIError::Device(e.to_string()))?;

        for i in 0..new_psbt.inputs.len() {
            psbt.inputs[i]
                .partial_sigs
                .append(&mut new_psbt.inputs[i].partial_sigs);
            psbt.inputs[i]
                .tap_script_sigs
                .append(&mut new_psbt.inputs[i].tap_script_sigs);
            if let Some(sig) = new_psbt.inputs[i].tap_key_sig {
                psbt.inputs[i].tap_key_sig = Some(sig);
            }
        }

        Ok(())
    }
}

impl From<api::Error> for HWIError {
    fn from(e: api::Error) -> Self {
        if let api::Error::UnexpectedResponse(api::protocol::Response::Refused) = e {
            HWIError::UserRefused
        } else {
            HWIError::Device(e.to_string())
        }
    }
}

impl From<Coldcard> for Box<dyn HWI + Send> {
    fn from(s: Coldcard) -> Box<dyn HWI + Send> {
        Box::new(s)
    }
}

impl From<Coldcard> for Arc<dyn HWI + Sync + Send> {
    fn from(s: Coldcard) -> Arc<dyn HWI + Sync + Send> {
        Arc::new(s)
    }
}

pub fn is_coldcard(device_info: &DeviceInfo) -> bool {
    device_info.vendor_id() == api::COINKITE_VID && device_info.product_id() == api::CKCC_PID
}
