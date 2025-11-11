use std::fmt::Display;

use miniscript::{Descriptor, DescriptorPublicKey};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct DescrFingerprint([u8; 8]);

impl DescrFingerprint {
    pub fn new(value: &Descriptor<DescriptorPublicKey>) -> Self {
        let descr_str = value.to_string();
        let len = descr_str.len();
        let fg: [u8; 8] = descr_str.as_bytes()[len - 8..len]
            .try_into()
            .expect("static size");
        DescrFingerprint(fg)
    }
}

impl Display for DescrFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str = String::from_utf8(self.0.to_vec()).expect("always valid utf8");
        write!(f, "{str}")
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_descr_fingerprint() {
        let raw_descr = "wsh(or_d(pk([9d69155f/48'/1'/0'/2']tpubDDxT9mkZzWwkKwpGT5fY6iiM9muYTPkTx6Eig8dpHR7TChuGGCWYAHVmpW1ciido5RiFWwjzYsF1GZHkEHg2nrYp3zNtx3QQRkznyLhQ77x/<0;1>/*),and_v(v:pkh([9d69155f/48'/1'/0'/2']tpubDDxT9mkZzWwkKwpGT5fY6iiM9muYTPkTx6Eig8dpHR7TChuGGCWYAHVmpW1ciido5RiFWwjzYsF1GZHkEHg2nrYp3zNtx3QQRkznyLhQ77x/<2;3>/*),older(52596))))#gx5f42wh";
        let descr = Descriptor::<DescriptorPublicKey>::from_str(raw_descr).unwrap();
        let descr_fg = DescrFingerprint::new(&descr);
        assert_eq!("gx5f42wh", descr_fg.to_string());
    }
}
