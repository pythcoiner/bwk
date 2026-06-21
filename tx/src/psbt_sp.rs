//! BIP375 silent-payment PSBT output fields, stored in the PSBT `unknown` map.
//!
//! Mainline rust-bitcoin has no typed silent-payment PSBT fields, so the BIP375
//! output fields are stored in the `unknown` map under their real keytypes. The
//! wire format is unchanged, so a BIP375-aware external signer still reads them.

use bitcoin::psbt::{raw, Output};
use bitcoin::secp256k1::PublicKey;

/// BIP375 keytype for the silent-payment v0 recipient info (scan + spend keys).
const PSBT_OUT_SP_V0_INFO: u8 = 0x09;
/// BIP375 keytype for the optional silent-payment v0 label.
const PSBT_OUT_SP_V0_LABEL: u8 = 0x0a;
/// Version byte prefixing the SP v0 info value.
const SP_V0_INFO_VERSION: u8 = 0x00;

/// Store the BIP375 silent-payment recipient info on a PSBT output's `unknown`
/// map: PSBT_OUT_SP_V0_INFO (version byte + 33-byte scan key + 33-byte spend key)
/// and, when present, PSBT_OUT_SP_V0_LABEL (u32 little-endian).
pub fn set_sp_v0_output(
    output: &mut Output,
    scan_key: PublicKey,
    spend_key: PublicKey,
    label: Option<u32>,
) {
    let mut info = Vec::with_capacity(67);
    info.push(SP_V0_INFO_VERSION);
    info.extend_from_slice(&scan_key.serialize());
    info.extend_from_slice(&spend_key.serialize());
    output.unknown.insert(
        raw::Key {
            type_value: PSBT_OUT_SP_V0_INFO,
            key: Vec::new(),
        },
        info,
    );
    if let Some(label) = label {
        output.unknown.insert(
            raw::Key {
                type_value: PSBT_OUT_SP_V0_LABEL,
                key: Vec::new(),
            },
            label.to_le_bytes().to_vec(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::secp256k1::{Secp256k1, SecretKey};
    use bitcoin::{absolute, transaction, Amount, ScriptBuf, Transaction, TxOut};

    fn key(b: u8) -> PublicKey {
        let secp = Secp256k1::new();
        PublicKey::from_secret_key(&secp, &SecretKey::from_slice(&[b; 32]).unwrap())
    }

    #[test]
    fn sp_v0_output_stored_and_round_trips() {
        let scan = key(1);
        let spend = key(2);

        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: ScriptBuf::new(),
            }],
        };
        let mut psbt = bitcoin::Psbt::from_unsigned_tx(tx).unwrap();
        set_sp_v0_output(&mut psbt.outputs[0], scan, spend, Some(7));

        let check = |out: &Output| {
            let info = out
                .unknown
                .get(&raw::Key {
                    type_value: PSBT_OUT_SP_V0_INFO,
                    key: Vec::new(),
                })
                .expect("sp info present");
            assert_eq!(info.len(), 67);
            assert_eq!(info[0], SP_V0_INFO_VERSION);
            assert_eq!(&info[1..34], &scan.serialize()[..]);
            assert_eq!(&info[34..67], &spend.serialize()[..]);
            let label = out
                .unknown
                .get(&raw::Key {
                    type_value: PSBT_OUT_SP_V0_LABEL,
                    key: Vec::new(),
                })
                .expect("label present");
            assert_eq!(label, &7u32.to_le_bytes());
        };
        check(&psbt.outputs[0]);

        // Wire-format survival: serialize and parse back.
        let bytes = psbt.serialize();
        let parsed = bitcoin::Psbt::deserialize(&bytes).unwrap();
        check(&parsed.outputs[0]);
    }
}
