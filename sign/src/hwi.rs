use async_hwi::service::{SigningDeviceMsg, SupportedDevice};
use crossbeam::channel;
use miniscript::{
    bitcoin::{bip32::DerivationPath, Psbt},
    Descriptor, DescriptorPublicKey,
};

use crate::signer::{Signer, SignerNotif};

#[derive(Clone)]
pub enum HwMessage {
    Device(SigningDeviceMsg),
}

impl From<SigningDeviceMsg> for HwMessage {
    fn from(msg: SigningDeviceMsg) -> Self {
        HwMessage::Device(msg)
    }
}

pub struct HwSigner {
    device: SupportedDevice<HwMessage>,
    _sender: Option<channel::Sender<SignerNotif>>,
}

impl HwSigner {
    pub fn new(device: SupportedDevice<HwMessage>, _id: String) -> Self {
        Self {
            device,
            _sender: None,
        }
    }
}

impl Signer for HwSigner {
    fn init(&mut self, channel: channel::Sender<SignerNotif>) {
        self._sender = Some(channel);
    }

    fn info(&self) {}

    fn get_xpub(&self, deriv: DerivationPath, _display: bool) {
        self.device.get_extended_pubkey((), &deriv);
    }

    fn is_descriptor_registered(&self, _descriptor: Descriptor<DescriptorPublicKey>) {}

    fn register_descriptor(&mut self, _descriptor: Descriptor<DescriptorPublicKey>) {}

    fn sign_with_descriptor(&self, psbt: Psbt, _descriptor: Descriptor<DescriptorPublicKey>) {
        self.device.sign_tx((), psbt);
    }
}
