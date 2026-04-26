use std::{path::PathBuf, str::FromStr, sync::Arc};

use bwk_descriptor::descriptor::ScriptType;
use bwk_persist::{self as persist, PersistenceBackend, PersistenceKind};
use bwk_sign::hot_signer::HotSigner;
use miniscript::{bitcoin, Descriptor, DescriptorPublicKey};
use serde::{Deserialize, Serialize};

/// Filename used by [`bwk_persist::FileConfigStore`].
///
/// Lives on `Config` for backwards-compat with existing on-disk layouts;
/// one could build its `FileConfigStore` path as
/// `config.account_dir().join(Config::CONFIG_FILENAME)`.
pub const CONFIG_FILENAME: &str = "config.json";
/// Logical store name for the bwk per-address-tip subscription map.
/// Re-export of the canonical constant in [`bwk_persist`].
pub const STATUSES_STORE_KEY: &str = bwk_persist::STATUSES_STORE_KEY;
/// Row keys under the `account` store for the [`Tip`] singleton fields.
const TIP_RECEIVE_ROW: &str = "receive_index";
const TIP_CHANGE_ROW: &str = "change_index";

/// Returns the OS-specific data directory under `dir_name`.
///
/// Convenience for callers running on a desktop OS; consumers on
/// other platforms (mobile, embedded) typically compute their data
/// path natively and pass it as `data_dir` to `Config::new`.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn datadir(dir_name: &str) -> PathBuf {
    #[cfg(target_os = "linux")]
    let dir = {
        let mut dir = dirs::home_dir().unwrap();
        dir.push(dir_name);
        dir
    };

    #[cfg(not(target_os = "linux"))]
    let dir = {
        let mut dir = dirs::config_dir().unwrap();
        dir.push(dir_name);
        dir
    };

    maybe_create_dir(&dir);

    dir
}

/// Creates a directory if it does not exist.
pub fn maybe_create_dir(dir: &PathBuf) {
    if !dir.exists() {
        #[cfg(unix)]
        {
            use std::fs::DirBuilder;
            use std::os::unix::fs::DirBuilderExt;

            let mut builder = DirBuilder::new();
            builder.mode(0o700).recursive(true).create(dir).unwrap();
        }

        #[cfg(not(unix))]
        std::fs::create_dir_all(dir).unwrap();
    }
}

/// Represents the configuration settings for the application.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(skip)]
    pub data_dir: PathBuf,
    #[serde(skip)]
    pub dir_name: String,
    pub account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub electrum_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub electrum_port: Option<u16>,
    #[serde(default)]
    pub offline: Option<bool>,
    pub network: bitcoin::Network,
    pub look_ahead: u32,
    pub mnemonic: Option<String>,
    pub descriptor: Descriptor<DescriptorPublicKey>,
    pub persist: bool,
    /// When true, the label store is not loaded from or persisted to disk.
    /// Used by sp::Account to delegate label management to the parent's SpLabelStore.
    #[serde(skip)]
    pub skip_labels: bool,
    #[serde(skip)]
    pub persist_kind: PersistenceKind,
}

impl Config {
    /// Creates a new `Config` instance with the specified descriptor.
    ///
    /// # Arguments
    ///
    /// * `mnemonic` - A string representing the mnemonic words.
    /// * `account` - A string representing the account name.
    /// * `network` - the bitcoin network for this config.
    ///
    /// # Returns
    ///
    /// A `Config` instance initialized with the provided descriptor.
    pub fn new(
        mnemonic: Option<String>,
        account: String,
        network: bitcoin::Network,
        script: ScriptType,
        data_dir: PathBuf,
        dir_name: String,
        persist: bool,
    ) -> Option<Config> {
        let descriptor = match script {
            ScriptType::Segwit(_) | ScriptType::Taproot(_) => {
                if let Some(mnemo) = &mnemonic {
                    let signer = HotSigner::new_from_mnemonics(network, mnemo).unwrap();
                    script.to_descriptor(network, |d| signer.xpub(&d)).unwrap()
                } else {
                    return None;
                }
            }
            ScriptType::Descriptor(descriptor) => *descriptor,
        };
        Some(Config {
            data_dir,
            dir_name,
            account,
            electrum_url: None,
            electrum_port: None,
            network,
            look_ahead: 20,
            mnemonic,
            descriptor,
            persist,
            persist_kind: PersistenceKind::default(),
            offline: None,
            skip_labels: false,
        })
    }

    /// Return the directory where this account's files (or SQLite DB) live.
    pub fn account_dir(&self) -> PathBuf {
        let mut dir = self.data_dir.clone();
        dir.push(&self.dir_name);
        dir.push(&self.account);
        dir
    }

    /// View of this config that's safe to persist to disk.
    ///
    /// Under [`PersistenceKind::Sqlite`] the mnemonic is stripped so it
    /// never lands on disk; under [`PersistenceKind::Json`] (default)
    /// the config is returned unchanged. Used by `Account` when handing
    /// config to a [`bwk_persist::ConfigStore`].
    pub fn for_persistence(&self) -> Config {
        if self.excludes_signer_data() {
            let mut stripped = self.clone();
            stripped.mnemonic = None;
            stripped
        } else {
            self.clone()
        }
    }

    /// Construct the concrete persistence backend for this config.
    ///
    /// Returns [`NoopBackend`] when `persist` is false; otherwise a
    /// [`JsonBackend`](bwk_persist::JsonBackend) rooted at
    /// [`Config::account_dir`] when `persist_kind == Json`, or a
    /// [`SqliteBackend`](bwk_persist::SqliteBackend) at
    /// `{account_dir}/account.sqlite` when `persist_kind == Sqlite` (errors
    /// if the `sqlite` feature is off).
    pub fn build_backend(&self) -> Result<Arc<dyn PersistenceBackend>, persist::PersistError> {
        let dir = self.account_dir();
        persist::build_backend(self.persist.then_some(self.persist_kind), dir)
    }
    /// Allow to disable persistance of data, useful for tests
    pub fn enable_persist(mut self, persist: bool) -> Self {
        self.persist = persist;
        self
    }

    /// Select the on-disk persistence backend (JSON default, or SQLite).
    ///
    /// Under [`PersistenceKind::Sqlite`], signer material (mnemonic / any
    /// private key) is never written to disk — not to `config.json`, not to
    /// the SQLite file, not to `.signers`. See [`bwk_persist`] for the
    /// full rule. Callers must re-supply the seed on the next run.
    pub fn with_persist_kind(mut self, kind: PersistenceKind) -> Self {
        self.persist_kind = kind;
        self
    }

    /// Is this config configured to keep signer material out of on-disk
    /// writes?
    pub fn excludes_signer_data(&self) -> bool {
        matches!(self.persist_kind, PersistenceKind::Sqlite)
    }

    /// Returns the Electrum URL as a string.
    pub fn electrum_url(&self) -> String {
        self.electrum_url.clone().unwrap_or_default()
    }
    /// Returns the Electrum port as a string.
    pub fn electrum_port(&self) -> String {
        self.electrum_port
            .map(|v| format!("{v}"))
            .unwrap_or_default()
    }
    /// Returns the look-ahead value as a string.
    pub fn look_ahead(&self) -> String {
        self.look_ahead.to_string()
    }
    /// Returns the network as a `Network` instance.
    pub fn network(&self) -> bitcoin::Network {
        self.network
    }
    /// Sets the Electrum URL.
    pub fn set_electrum_url(&mut self, url: String) {
        self.electrum_url = Some(url);
    }
    /// Sets the Electrum port from a string.
    pub fn set_electrum_port(&mut self, port: String) {
        self.electrum_port = port.parse::<u16>().ok();
    }
    /// Sets the look-ahead value from a string.
    pub fn set_look_ahead(&mut self, look_ahead: String) {
        if let Ok(la) = look_ahead.parse::<u32>() {
            self.look_ahead = la;
        }
    }
    /// Sets the network.
    pub fn set_network(&mut self, network: bitcoin::Network) {
        self.network = network;
    }
    /// Sets the mnemonic.
    pub fn set_mnemonic(&mut self, mnemonic: String) {
        self.mnemonic = Some(mnemonic);
    }
    /// Sets the account name.
    pub fn set_account(&mut self, name: String) {
        self.account = name;
    }
    pub fn set_offline(&mut self, offline: bool) {
        self.offline = Some(offline);
    }
    pub fn offline(&self) -> bool {
        self.offline.unwrap_or(false)
    }
    pub fn dir_name(&self) -> &str {
        &self.dir_name
    }

    pub fn data_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }

    /// Persists the tip information through the given backend.
    ///
    /// Writes two rows under the `account` store: `receive_index` and
    /// `change_index`, each holding the JSON-encoded `u32`.
    pub fn persist_tip(backend: &dyn PersistenceBackend, receive: u32, change: u32) {
        let enc = |label: &str, v: u32| match serde_json::to_vec(&v) {
            Ok(b) => Some(b),
            Err(e) => {
                log::error!("persist_tip encode {label}: {e}");
                None
            }
        };
        let recv_bytes = match enc("receive_index", receive) {
            Some(b) => b,
            None => return,
        };
        let chg_bytes = match enc("change_index", change) {
            Some(b) => b,
            None => return,
        };
        if let Err(e) =
            backend.put_row(bwk_persist::ACCOUNT_STORE_KEY, TIP_RECEIVE_ROW, &recv_bytes)
        {
            log::error!("persist_tip put receive_index: {e}");
            return;
        }
        if let Err(e) = backend.put_row(bwk_persist::ACCOUNT_STORE_KEY, TIP_CHANGE_ROW, &chg_bytes)
        {
            log::error!("persist_tip put change_index: {e}");
        }
    }

    /// Retrieves the tip information through the given backend.
    pub fn tip_from_backend(backend: &dyn PersistenceBackend) -> Tip {
        let read = |row: &str| -> u32 {
            match backend.get_row(bwk_persist::ACCOUNT_STORE_KEY, row) {
                Ok(Some(bytes)) => serde_json::from_slice::<u32>(&bytes).unwrap_or_default(),
                _ => 0,
            }
        };
        Tip {
            receive: read(TIP_RECEIVE_ROW),
            change: read(TIP_CHANGE_ROW),
        }
    }
}

/// Represents the tip information for the current account.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Tip {
    pub receive: u32,
    pub change: u32,
}

/// Checks if the provided descriptor string is valid.
///
/// # Arguments
///
/// * `descriptor` - A string representing the descriptor to validate.
pub fn is_descriptor_valid(descriptor: String) -> bool {
    Descriptor::<DescriptorPublicKey>::from_str(&descriptor).is_ok()
}

#[cfg(test)]
pub mod tests {
    use bwk_persist::{ConfigStore, FileConfigStore};
    use miniscript::bitcoin::bip32::ChildNumber;

    use super::*;

    #[test]
    fn config_round_trips_through_file_store() {
        let temp = temp_dir::TempDir::new().unwrap();
        let path = temp.child("storage");
        let mnemonic = bip39::Mnemonic::generate(12).unwrap();
        let cfg = Config::new(
            Some(mnemonic.to_string()),
            "my_account".to_string(),
            bitcoin::Network::Regtest,
            ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
            path.clone(),
            "wallet".to_string(),
            true,
        )
        .unwrap();

        let store: FileConfigStore<Config> =
            FileConfigStore::new(cfg.account_dir().join(CONFIG_FILENAME));
        store.save(&cfg.for_persistence()).unwrap();
        let cfg2 = store.load().unwrap().expect("config persisted");
        assert_eq!(cfg.account, cfg2.account);
        assert_eq!(cfg2.account, "my_account");
    }

    #[test]
    fn for_persistence_under_sqlite_strips_mnemonic() {
        let temp = temp_dir::TempDir::new().unwrap();
        let path = temp.child("storage");
        let unique = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".to_string();
        let mut cfg = Config::new(
            Some(unique.clone()),
            "alice".to_string(),
            bitcoin::Network::Regtest,
            ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
            path.clone(),
            "wallet".to_string(),
            true,
        )
        .unwrap();
        cfg.persist_kind = PersistenceKind::Sqlite;

        let view = cfg.for_persistence();
        assert!(
            view.mnemonic.is_none(),
            "mnemonic must be stripped under SQLite"
        );
        let serialized = serde_json::to_string_pretty(&view).unwrap();
        assert!(
            !serialized.contains(&unique),
            "mnemonic must not appear in serialized form under SQLite mode: {serialized}"
        );
    }

    #[test]
    fn for_persistence_under_json_preserves_mnemonic() {
        let temp = temp_dir::TempDir::new().unwrap();
        let path = temp.child("storage");
        let unique = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".to_string();
        let cfg = Config::new(
            Some(unique.clone()),
            "alice".to_string(),
            bitcoin::Network::Regtest,
            ScriptType::Segwit(ChildNumber::from_hardened_idx(0).unwrap()),
            path.clone(),
            "wallet".to_string(),
            true,
        )
        .unwrap();

        let view = cfg.for_persistence();
        assert_eq!(view.mnemonic, Some(unique.clone()));
        let serialized = serde_json::to_string_pretty(&view).unwrap();
        assert!(
            serialized.contains(&unique),
            "mnemonic must appear in serialized form under JSON mode (default)"
        );
    }
}
