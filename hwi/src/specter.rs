use std::fmt::Debug;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use bitcoin::{
    bip32::{DerivationPath, Fingerprint, Xpub},
    psbt::Psbt,
    taproot,
};

use serialport::{available_ports, SerialPort, SerialPortType};

use super::{AddressScript, DeviceKind, Error as HWIError, HWI};

#[derive(Debug)]
pub struct Specter<T> {
    transport: T,
    kind: DeviceKind,
}

impl<T: Transport> Specter<T> {
    pub fn fingerprint(&self) -> Result<Fingerprint, SpecterError> {
        self.transport
            .request("\r\n\r\nfingerprint\r\n")
            .and_then(|resp| {
                Fingerprint::from_str(&resp).map_err(|e| SpecterError::Device(e.to_string()))
            })
    }

    pub fn get_extended_pubkey(&self, path: &DerivationPath) -> Result<Xpub, SpecterError> {
        self.transport
            .request(&format!("\r\n\r\nxpub {path}\r\n"))
            .and_then(|resp| Xpub::from_str(&resp).map_err(|e| SpecterError::Device(e.to_string())))
    }

    /// If the descriptor contains master public keys but doesn't contain wildcard derivations,
    /// the default derivation /{0,1}/* will be added by the device to all extended keys in the descriptor.
    /// See: https://github.com/cryptoadvance/specter-diy/blob/master/docs/descriptors.md#default-derivations
    /// If at least one of the xpubs has a wildcard derivation the descriptor will not be changed.
    /// /** is an equivalent of /{0,1}/*.
    pub fn add_wallet(&self, name: &str, policy: &str) -> Result<(), SpecterError> {
        self.transport
            .request(&format!(
                "\r\n\r\naddwallet {}&{}\r\n",
                name,
                policy
                    .replace("/**", "/{0,1}/*")
                    // currently specter does not support <0;1> but {0,1}
                    .replace('<', "{")
                    .replace(';', ",")
                    .replace('>', "}")
            ))
            .and_then(|resp| {
                if resp.is_empty() || resp == "success" {
                    Ok(())
                } else if resp == "error: User cancelled" {
                    Err(SpecterError::UserCancelled)
                } else {
                    Err(SpecterError::Device(resp))
                }
            })
    }

    pub fn sign(&self, psbt: &Psbt) -> Result<Psbt, SpecterError> {
        self.transport
            .request(&format!("\r\n\r\nsign {psbt}\r\n"))
            .and_then(|resp| {
                if resp == "error: User cancelled" {
                    Err(SpecterError::UserCancelled)
                } else {
                    Psbt::from_str(&resp).map_err(|e| SpecterError::Device(e.to_string()))
                }
            })
    }
}

impl<T: Transport> HWI for Specter<T> {
    fn device_kind(&self) -> DeviceKind {
        self.kind
    }

    fn get_version(&self) -> Result<super::Version, HWIError> {
        Err(HWIError::UnimplementedMethod)
    }

    fn get_master_fingerprint(&self) -> Result<Fingerprint, HWIError> {
        Ok(self.fingerprint()?)
    }

    fn get_extended_pubkey(&self, path: &DerivationPath) -> Result<Xpub, HWIError> {
        Ok(self.get_extended_pubkey(path)?)
    }

    fn display_address(&self, _script: &AddressScript) -> Result<(), HWIError> {
        Err(HWIError::UnimplementedMethod)
    }

    fn register_wallet(&self, name: &str, policy: &str) -> Result<Option<[u8; 32]>, HWIError> {
        self.add_wallet(name, policy)?;
        Ok(None)
    }

    fn is_wallet_registered(&self, _name: &str, _policy: &str) -> Result<bool, HWIError> {
        Err(HWIError::UnimplementedMethod)
    }

    fn sign_tx(&self, psbt: &mut Psbt) -> Result<(), HWIError> {
        let mut new_psbt = self.sign(psbt)?;
        // Psbt returned by specter wallet has all unnecessary fields removed,
        // only global transaction and partial signatures for all inputs remain in it.
        // In order to have the full Psbt, the partial_sigs are extracted and appended
        // to the original psbt.
        let mut has_signed = false;
        for i in 0..new_psbt.inputs.len() {
            if !new_psbt.inputs[i].partial_sigs.is_empty() {
                has_signed = true;
                psbt.inputs[i]
                    .partial_sigs
                    .append(&mut new_psbt.inputs[i].partial_sigs)
            }
            if !new_psbt.inputs[i].tap_script_sigs.is_empty() {
                has_signed = true;
                psbt.inputs[i]
                    .tap_script_sigs
                    .append(&mut new_psbt.inputs[i].tap_script_sigs)
            }
            if new_psbt.inputs[i].tap_key_sig.is_some() {
                has_signed = true;
                psbt.inputs[i].tap_key_sig = new_psbt.inputs[i].tap_key_sig;
            } else {
                // Specter does not populate PSBT_TAP_KEY_SIG at v1.9.0
                // see https://github.com/cryptoadvance/specter-diy/issues/277#issuecomment-2183906271
                if let Some(witness) = &new_psbt.inputs[i].final_script_witness {
                    if let Some(sig) = witness.nth(0) {
                        if let Ok(sig) = taproot::Signature::from_slice(sig) {
                            psbt.inputs[i].tap_key_sig = Some(sig);
                            has_signed = true;
                        }
                    }
                }
            }
        }
        if !has_signed {
            return Err(SpecterError::DeviceDidNotSign.into());
        }

        Ok(())
    }
}

impl<T: 'static + Transport + Send> From<Specter<T>> for Box<dyn HWI + Send> {
    fn from(s: Specter<T>) -> Box<dyn HWI + Send> {
        Box::new(s)
    }
}

fn exchange<W: Write, R: BufRead>(
    writer: &mut W,
    reader: &mut R,
    req: &str,
) -> Result<String, SpecterError> {
    writer
        .write_all(req.as_bytes())
        .map_err(|e| SpecterError::Device(e.to_string()))?;
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| SpecterError::Device(e.to_string()))?; // ACK line
    line.clear();
    reader
        .read_line(&mut line)
        .map_err(|e| SpecterError::Device(e.to_string()))?; // response line
    Ok(line.trim_end().to_string())
}

pub trait Transport: Debug {
    fn request(&self, req: &str) -> Result<String, SpecterError>;
}

#[derive(Debug)]
pub struct TcpTransport;
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:8789";

impl Transport for TcpTransport {
    fn request(&self, req: &str) -> Result<String, SpecterError> {
        let mut stream =
            TcpStream::connect(DEFAULT_ADDRESS).map_err(|e| SpecterError::Device(e.to_string()))?;
        let read_stream = stream
            .try_clone()
            .map_err(|e| SpecterError::Device(e.to_string()))?;
        let mut reader = BufReader::new(read_stream);
        let result = exchange(&mut stream, &mut reader, req);
        let _ = stream.shutdown(std::net::Shutdown::Both);
        result
    }
}

pub type SpecterSimulator = Specter<TcpTransport>;

impl SpecterSimulator {
    pub fn try_connect() -> Result<Self, HWIError> {
        let s = SpecterSimulator {
            transport: TcpTransport {},
            kind: DeviceKind::SpecterSimulator,
        };
        let _ = s.get_master_fingerprint()?;
        Ok(s)
    }
}

impl Specter<SerialTransport> {
    pub fn new(port_name: String) -> Result<Self, SpecterError> {
        let transport = SerialTransport::new(port_name)?;
        Ok(Self {
            transport,
            kind: DeviceKind::Specter,
        })
    }
    pub fn enumerate() -> Result<Vec<Self>, SpecterError> {
        let mut res = Vec::new();
        for port_name in SerialTransport::enumerate_potential_ports()? {
            let specter = Specter::<SerialTransport>::new(port_name)?;
            if specter.get_master_fingerprint().is_ok() {
                res.push(specter);
            }
        }
        Ok(res)
    }
}

pub struct SerialTransport {
    stream: Arc<Mutex<Box<dyn SerialPort + Send>>>,
}

impl std::fmt::Debug for SerialTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerialTransport").finish()
    }
}

impl SerialTransport {
    pub const SPECTER_VID: u16 = 61525;
    pub const SPECTER_PID: u16 = 38914;

    pub fn new(port_name: String) -> Result<Self, SpecterError> {
        let mut port = serialport::new(port_name, 9600)
            .timeout(std::time::Duration::from_secs(5))
            .open()
            .map_err(|e| SpecterError::Device(e.to_string()))?;
        port.write_data_terminal_ready(true)
            .map_err(|e| SpecterError::Device(e.to_string()))?;
        Ok(Self {
            stream: Arc::new(Mutex::new(port)),
        })
    }

    pub fn enumerate_potential_ports() -> Result<Vec<String>, SpecterError> {
        match available_ports() {
            Ok(ports) => Ok(ports
                .into_iter()
                .filter_map(|p| match p.port_type {
                    SerialPortType::UsbPort(info) => {
                        if info.vid == SerialTransport::SPECTER_VID
                            && info.pid == SerialTransport::SPECTER_PID
                        {
                            Some(p.port_name)
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .collect()),
            Err(e) => Err(SpecterError::Device(format!(
                "Error listing serial ports: {e}"
            ))),
        }
    }
}

impl Transport for SerialTransport {
    fn request(&self, req: &str) -> Result<String, SpecterError> {
        let mut guard = self
            .stream
            .lock()
            .map_err(|e| SpecterError::Device(e.to_string()))?;
        let port: &mut dyn SerialPort = guard.as_mut();
        port.write_all(req.as_bytes())
            .map_err(|e| SpecterError::Device(e.to_string()))?;
        let mut reader = BufReader::new(port);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| SpecterError::Device(e.to_string()))?; // ACK
        line.clear();
        reader
            .read_line(&mut line)
            .map_err(|e| SpecterError::Device(e.to_string()))?; // response
        Ok(line.trim_end().to_string())
    }
}

#[derive(Debug)]
pub enum SpecterError {
    DeviceNotFound,
    DeviceDidNotSign,
    Device(String),
    UserCancelled,
}

impl std::fmt::Display for SpecterError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::DeviceNotFound => write!(f, "Specter not found"),
            Self::DeviceDidNotSign => write!(f, "Specter did not sign the psbt"),
            Self::Device(e) => write!(f, "Specter error: {e}"),
            Self::UserCancelled => write!(f, "User cancelled operation"),
        }
    }
}

impl From<SpecterError> for HWIError {
    fn from(e: SpecterError) -> HWIError {
        match e {
            SpecterError::DeviceNotFound => HWIError::DeviceNotFound,
            SpecterError::DeviceDidNotSign => HWIError::DeviceDidNotSign,
            SpecterError::Device(e) => HWIError::Device(e),
            SpecterError::UserCancelled => HWIError::UserRefused,
        }
    }
}
