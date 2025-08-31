use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::PathBuf,
    str::FromStr,
};

use bip39::Mnemonic;
use miniscript::{
    bitcoin::{self, bip32::DerivationPath, ScriptBuf},
    Descriptor, DescriptorPublicKey,
};
use serde::{Deserialize, Serialize};

use crate::signer::{wpkh, HotSigner};

const CONFIG_FILENAME: &str = "config.json";

/// Returns the data directory path based on the operating system.
///
/// On Linux, it returns the path to the `.qoinstr` directory in the user's home directory.
/// On other operating systems, it returns the path to the `Qoinstr` directory in the user's config directory.
/// The directory is created if it does not exist.
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
        dir.push("Qoinstr");
        dir
    };

    maybe_create_dir(&dir);

    dir
}

/// Creates a directory if it does not exist.
fn maybe_create_dir(dir: &PathBuf) {
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
    dir_name: &'static str,
    #[serde(skip)]
    pub account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub electrum_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub electrum_port: Option<u16>,
    pub network: bitcoin::Network,
    pub look_ahead: u32,
    pub mnemonic: String,
    pub descriptor: Descriptor<DescriptorPublicKey>,
    pub persist: bool,
}

impl Config {
    /// Allow to disable persistance of data, useful for tests
    pub fn enable_persist(mut self, persist: bool) -> Self {
        self.persist = persist;
        self
    }

    pub fn dir_name(&self) -> &'static str {
        self.dir_name
    }
    /// Lists all configuration directories in the data directory.
    ///
    /// # Returns
    ///
    /// A vector of strings representing the account names of the configurations
    /// found in the data directory.
    /// Lists all configuration directories in the data directory.
    pub fn list_configs(&self) -> Vec<String> {
        let path = datadir(self.dir_name);
        let mut out = vec![];
        if let Ok(folders) = fs::read_dir(path) {
            folders.for_each(|account| {
                if let Ok(entry) = account {
                    if let Ok(md) = entry.metadata() {
                        if md.is_dir() {
                            let acc_name = entry.file_name().to_str().unwrap().to_string();
                            let parsed = self.from_file(acc_name.clone());
                            if !parsed.account.is_empty() {
                                out.push(acc_name);
                            }
                        };
                    }
                }
            });
        }

        out
    }

    /// Checks if a configuration file exists for the given account.
    ///
    /// # Arguments
    ///
    /// * `account` - A string representing the account name.
    ///
    /// # Returns
    ///
    /// A boolean value indicating whether the configuration file exists.
    /// Checks if a configuration file exists for the given account.
    pub fn config_exists(&self, account: String) -> bool {
        let mut path = self.path(account.clone());
        path.push(CONFIG_FILENAME);
        path.exists()
    }
    /// Returns the path to the configuration directory for the specified account.
    ///
    /// # Arguments
    ///
    /// * `account` - A string representing the account name.
    ///
    /// # Returns
    ///
    /// A `PathBuf` representing the path to the configuration directory.
    pub fn path(&self, account: String) -> PathBuf {
        let mut dir = datadir(self.dir_name);
        dir.push(account);
        dir
    }

    /// Creates a `Config` instance from a configuration file.
    ///
    /// # Arguments
    ///
    /// * `account` - A string representing the account name.
    pub fn from_file(&self, account: String) -> Self {
        let mut path = self.path(account.clone());
        path.push(CONFIG_FILENAME);

        let mut file = File::open(path).unwrap();
        let mut content = String::new();
        let _ = file.read_to_string(&mut content);
        let mut conf: Config = serde_json::from_str(&content).unwrap();
        let mnemo = Mnemonic::from_str(&conf.mnemonic);
        if mnemo.is_ok() {
            conf.account = account;
        }
        conf
    }

    /// Returns the path to the transactions file for the current account.
    pub fn transactions_path(&self) -> PathBuf {
        let mut path = self.path(self.account.clone());
        path.push("transactions.json");
        path
    }

    /// Returns the path to the statuses file for the current account.
    pub fn statuses_path(&self) -> PathBuf {
        let mut path = self.path(self.account.clone());
        path.push("statuses.json");
        path
    }

    /// Returns the path to the tip file for the current account.
    pub fn tip_path(&self) -> PathBuf {
        let mut path = self.path(self.account.clone());
        path.push("tip.json");
        path
    }

    /// Returns the path to the labels file for the current account.
    pub fn labels_path(&self) -> PathBuf {
        let mut path = self.path(self.account.clone());
        path.push("labels.json");
        path
    }

    /// Persists the tip information to a file for the current account.
    ///
    /// # Arguments
    ///
    /// * `receive` - The amount to receive.
    /// * `change` - The amount of change.
    pub fn persist_tip(&self, receive: u32, change: u32) {
        if !self.persist {
            return;
        }
        let file = File::create(self.tip_path());
        match file {
            Ok(mut file) => {
                let tip = Tip { receive, change };
                let content = serde_json::to_string_pretty(&tip).expect("cannot fail");
                let _ = file.write(content.as_bytes());
            }
            Err(e) => {
                log::error!("Config::persist_tip() fail to open file: {e}");
            }
        }
    }

    /// Retrieves the tip information from the tip file for the current account.
    ///
    /// # Returns
    ///
    /// A `Tip` instance containing the tip information.
    pub fn tip_from_file(&self) -> Tip {
        if let Ok(mut file) = File::open(self.tip_path()) {
            let mut content = String::new();
            let _ = file.read_to_string(&mut content);
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Default::default()
        }
    }

    /// Persists the statuses information to a file for the current account.
    ///
    /// # Arguments
    ///
    /// * `statuses` - A reference to a `BTreeMap` containing the statuses information.
    pub fn persist_statuses(&self, statuses: &BTreeMap<ScriptBuf, (Option<String>, u32, u32)>) {
        if !self.persist {
            return;
        }
        let file = File::create(self.statuses_path());
        match file {
            Ok(mut file) => {
                let content = serde_json::to_string_pretty(statuses).expect("cannot fail");
                let _ = file.write(content.as_bytes());
            }
            Err(e) => {
                log::error!("Config::statuses() fail to open file: {e}");
            }
        }
    }

    /// Retrieves the statuses information from the statuses file for the current account.
    ///
    /// # Returns
    ///
    /// A `BTreeMap` containing the statuses information.
    pub fn statuses_from_file(&self) -> BTreeMap<ScriptBuf, (Option<String>, u32, u32)> {
        if let Ok(mut file) = File::open(self.statuses_path()) {
            let mut content = String::new();
            let _ = file.read_to_string(&mut content);
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Default::default()
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
        mnemonic: String,
        account: String,
        network: bitcoin::Network,
        dir_name: &'static str,
        persist: bool,
    ) -> Config {
        let signer = HotSigner::new_from_mnemonics(network, &mnemonic).unwrap();
        let n_path = match network {
            bitcoin::Network::Bitcoin => 0,
            _ => 1,
        };
        let a_path = 0;
        let deriv_path =
            DerivationPath::from_str(&format!("m/84'/{}'/{}'", n_path, a_path)).unwrap();
        let oxpub = signer.xpub(&deriv_path);
        let descriptor = wpkh(oxpub);
        Config {
            dir_name,
            account,
            electrum_url: None,
            electrum_port: None,
            network,
            look_ahead: 20,
            mnemonic,
            descriptor,
            persist,
        }
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
        self.mnemonic = mnemonic;
    }
    /// Sets the account name.
    pub fn set_account(&mut self, name: String) {
        self.account = name;
    }
    /// Saves the configuration to a file.
    pub fn to_file(&self) {
        let mut path = self.path(self.account.clone());
        maybe_create_dir(&path);
        path.push(CONFIG_FILENAME);

        log::warn!("Config::to_file() {:?}", path);

        let mut file = File::create(path).unwrap();
        let content = serde_json::to_string_pretty(&self).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }
}
