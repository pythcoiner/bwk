use bip39;
use std::fmt::Display;

#[derive(Debug, PartialEq)]
pub enum Error {
    SighashFail,
    InvalidSignature,
    InputNotOwned,
    XPrivFromSeed,
    MissingWitnessUtxo,
    SpkNotMatch,
    DerivationPath,
    Bip39(bip39::Error),
    UnregisteredDescriptor,
    DescriptorNetwork,
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::SighashFail => write!(f, "Sighash id not SIGHASH_ALL | SIGHASH_ANYONE_CAN_PAY"),
            Error::InvalidSignature => write!(f, "Signature processed is invalid"),
            Error::Bip39(e) => write!(f, "{}", e),
            Error::XPrivFromSeed => write!(f, "Fail to generate XPriv from seed"),
            Error::InputNotOwned => write!(f, "this input is not owned"),
            Error::MissingWitnessUtxo => write!(f, "witness_utxo field is missing in PSBT"),
            Error::SpkNotMatch => write!(f, "spk in spent output do not match"),
            Error::DerivationPath => write!(f, "Invalid derivation path"),
            Error::UnregisteredDescriptor => write!(f, "Unknown descriptor"),
            Error::DescriptorNetwork => write!(f, "Wrong descriptor network"),
        }
    }
}

impl From<bip39::Error> for Error {
    fn from(value: bip39::Error) -> Self {
        Error::Bip39(value)
    }
}
