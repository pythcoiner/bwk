// from https://github.com/romanz/electrs/blob/bd6f93a1e3bf5eaaae4c3c7b92393560d2faa690/src/types.rs
use miniscript::bitcoin::{
    hashes::{hash_newtype, sha256, Hash},
    Script,
};

hash_newtype! {
    /// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-basics.html#script-hashes
    #[hash_newtype(backward)]
    pub struct ScriptHash(sha256::Hash);
}

impl ScriptHash {
    pub fn new(script: &Script) -> Self {
        ScriptHash::hash(script.as_bytes())
    }
}

// ***************************************************************************

hash_newtype! {
    /// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-basics.html#status
    pub struct StatusHash(sha256::Hash);
}

#[cfg(test)]
mod tests {
    use crate::electrum::types::ScriptHash;
    use miniscript::bitcoin::Address;
    use serde_json::{from_str, json};

    use std::str::FromStr;

    #[test]
    fn test_scripthash_serde() {
        let hex = "\"4b3d912c1523ece4615e91bf0d27381ca72169dbf6b1c2ffcc9f92381d4984a3\"";
        let scripthash: ScriptHash = from_str(hex).unwrap();
        assert_eq!(format!("\"{scripthash}\""), hex);
        assert_eq!(json!(scripthash).to_string(), hex);
    }

    #[test]
    fn test_scripthash() {
        let addr = Address::from_str("1KVNjD3AAnQ3gTMqoTKcWFeqSFujq9gTBT")
            .unwrap()
            .assume_checked();
        let scripthash = ScriptHash::new(&addr.script_pubkey());
        assert_eq!(
            scripthash,
            "00dfb264221d07712a144bda338e89237d1abd2db4086057573895ea2659766a"
                .parse()
                .unwrap()
        );
    }
}
