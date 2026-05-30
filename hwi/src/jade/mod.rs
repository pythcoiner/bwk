pub mod api;
pub mod pinserver;

use std::{
    collections::BTreeMap,
    fmt::Debug,
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{de::DeserializeOwned, Serialize};

use bitcoin::{
    bip32::{DerivationPath, Fingerprint, Xpub},
    psbt::Psbt,
    Network,
};

use serialport::{available_ports, SerialPort, SerialPortType};

use crate::{parse_version, utils};

use super::{AddressScript, DeviceKind, Error as HWIError, HWI};

pub const JADE_NETWORK_MAINNET: &str = "mainnet";
pub const JADE_NETWORK_TESTNET: &str = "testnet";

#[derive(Debug)]
pub struct Jade<T> {
    transport: T,
    network: &'static str,
    kind: DeviceKind,
    descriptor_name: Option<String>,
}

impl<T: Transport + Send> Jade<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            network: JADE_NETWORK_MAINNET,
            kind: DeviceKind::Jade,
            descriptor_name: None,
        }
    }

    pub fn with_network(mut self, network: Network) -> Self {
        if network == Network::Bitcoin {
            self.network = JADE_NETWORK_MAINNET;
        } else {
            self.network = JADE_NETWORK_TESTNET;
        }
        self
    }

    pub fn with_wallet(mut self, descriptor_name: String) -> Self {
        self.descriptor_name = Some(descriptor_name);
        self
    }

    pub fn ping(&self) -> Result<(), JadeError> {
        let _res: u64 = self
            .transport
            .request("ping", Option::<api::EmptyRequest>::None)?
            .into_result()?;
        Ok(())
    }

    pub fn get_info(&self) -> Result<api::GetInfoResponse, HWIError> {
        let info: api::GetInfoResponse = self
            .transport
            .request("get_version_info", Option::<api::EmptyRequest>::None)?
            .into_result()?;
        Ok(info)
    }

    pub fn get_registered_descriptors(
        &self,
    ) -> Result<BTreeMap<String, api::DescriptorInfoResponse>, HWIError> {
        let descriptors: BTreeMap<String, api::DescriptorInfoResponse> = self
            .transport
            .request(
                "get_registered_descriptors",
                Option::<api::EmptyRequest>::None,
            )?
            .into_result()?;
        Ok(descriptors)
    }

    pub fn get_registered_descriptor(
        &self,
        name: &str,
    ) -> Result<api::GetRegisteredDescriptorResponse, HWIError> {
        let registered: api::GetRegisteredDescriptorResponse = self
            .transport
            .request(
                "get_registered_descriptor",
                Some(api::GetRegisteredDescriptorParams {
                    descriptor_name: name,
                }),
            )?
            .into_result()?;
        Ok(registered)
    }

    pub fn auth(&self) -> Result<(), JadeError> {
        let res: api::AuthUserResponse = self
            .transport
            .request(
                "auth_user",
                Some(api::AuthUserParams {
                    network: self.network,
                    epoch: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|t| t.as_secs())
                        .unwrap_or(0),
                }),
            )?
            .into_result()?;

        if let api::AuthUserResponse::PinServerRequired { http_request } = res {
            let client = pinserver::PinServerClient::new();
            let pin_params: api::PinParams = client.request(http_request.params)?;
            let handshake_completed: bool = self
                .transport
                .request("pin", Some(pin_params))?
                .into_result()?;
            if !handshake_completed {
                return Err(JadeError::HandShakeRefused);
            }
        }
        Ok(())
    }
}

impl<T: Transport + Send> HWI for Jade<T> {
    fn device_kind(&self) -> DeviceKind {
        self.kind
    }

    fn get_version(&self) -> Result<super::Version, HWIError> {
        let info = self.get_info()?;
        parse_version(&info.jade_version)
    }

    fn get_master_fingerprint(&self) -> Result<Fingerprint, HWIError> {
        let xpub = self.get_extended_pubkey(&DerivationPath::master())?;
        Ok(xpub.fingerprint())
    }

    fn get_extended_pubkey(&self, path: &DerivationPath) -> Result<Xpub, HWIError> {
        let s: String = self
            .transport
            .request(
                "get_xpub",
                Some(api::GetXpubParams {
                    network: self.network,
                    path: path.to_u32_vec(),
                }),
            )?
            .into_result()?;
        let xpub = Xpub::from_str(&s).map_err(|e| HWIError::Device(e.to_string()))?;
        Ok(xpub)
    }

    fn display_address(&self, script: &AddressScript) -> Result<(), HWIError> {
        match (self.descriptor_name.as_ref(), script) {
            (Some(descriptor_name), AddressScript::Miniscript { index, change }) => {
                let _address: String = self
                    .transport
                    .request(
                        "get_receive_address",
                        Some(api::DescriptorAddressParams {
                            network: self.network,
                            branch: u32::from(*change),
                            pointer: *index,
                            descriptor_name,
                        }),
                    )?
                    .into_result()?;
                Ok(())
            }
            _ => Err(HWIError::UnimplementedMethod),
        }
    }

    fn register_wallet(&self, name: &str, policy: &str) -> Result<Option<[u8; 32]>, HWIError> {
        let (descriptor_template, keys) = utils::extract_keys_and_template::<String>(policy)?;
        let registered: bool = self
            .transport
            .request(
                "register_descriptor",
                Some(api::RegisterDescriptorParams {
                    network: self.network,
                    descriptor_name: name,
                    descriptor: descriptor_template,
                    datavalues: keys
                        .into_iter()
                        .enumerate()
                        .map(|(i, key)| (format!("@{i}"), key))
                        .collect(),
                }),
            )?
            .into_result()?;
        if !registered {
            Err(HWIError::UserRefused)
        } else {
            Ok(None)
        }
    }

    fn is_wallet_registered(&self, name: &str, policy: &str) -> Result<bool, HWIError> {
        let (descriptor_template, keys) = utils::extract_keys_and_template::<String>(policy)?;
        let datavalues: BTreeMap<String, String> = keys
            .into_iter()
            .enumerate()
            .map(|(i, key)| (format!("@{i}"), key))
            .collect();
        let registered_descriptors = self.get_registered_descriptors()?;
        if !registered_descriptors.contains_key(name) {
            return Ok(false);
        }

        let registered = self.get_registered_descriptor(name)?;

        Ok(registered.descriptor_name == name
            && registered.descriptor == descriptor_template
            && registered.datavalues == datavalues)
    }

    fn sign_tx(&self, psbt: &mut Psbt) -> Result<(), HWIError> {
        let first: api::Response<serde_bytes::ByteBuf> = self.transport.request(
            "sign_psbt",
            Some(api::SignPsbtParams {
                network: self.network,
                psbt: Psbt::serialize(psbt),
            }),
        )?;

        if let Some(e) = first.error {
            return Err(JadeError::Rpc(e).into());
        }

        let mut psbt_bytes = first
            .result
            .ok_or(JadeError::Transport(TransportError::NoErrorOrResult))?;

        if let (Some(mut seqlen), Some(mut seqnum)) = (first.seqlen, first.seqnum) {
            if seqlen > 1 {
                while seqnum < seqlen {
                    let mut res: api::Response<serde_bytes::ByteBuf> = self.transport.request(
                        "get_extended_data",
                        Some(api::GetExtendedDataParams {
                            origid: &first.id,
                            orig: "sign_psbt",
                            seqnum: seqnum + 1,
                            seqlen,
                        }),
                    )?;

                    if let Some(e) = res.error {
                        return Err(JadeError::Rpc(e).into());
                    }

                    if let Some(bytes) = res.result.as_mut() {
                        psbt_bytes.append(bytes);
                    } else {
                        return Err(JadeError::Transport(TransportError::NoErrorOrResult).into());
                    }

                    if let (Some(len), Some(num)) = (res.seqlen, res.seqnum) {
                        seqlen = len;
                        seqnum = num;
                    } else {
                        return Err(JadeError::Transport(TransportError::NoErrorOrResult).into());
                    }
                }
            }
        }

        let signed_psbt =
            Psbt::deserialize(&psbt_bytes).map_err(|e| HWIError::Device(e.to_string()))?;
        utils::merge_signatures(psbt, &signed_psbt);

        Ok(())
    }
}

impl<T: 'static + Transport + Send> From<Jade<T>> for Box<dyn HWI + Send> {
    fn from(s: Jade<T>) -> Box<dyn HWI + Send> {
        Box::new(s)
    }
}

fn exchange<S, D>(
    transport: &mut dyn SerialPort,
    method: &str,
    params: Option<S>,
) -> Result<api::Response<D>, JadeError>
where
    S: Serialize + Unpin,
    D: DeserializeOwned + Unpin,
{
    let id = std::process::id();
    let req = serde_cbor::to_vec(&api::Request {
        id: &id.to_string(),
        method,
        params,
    })
    .map_err(TransportError::from)?;

    transport.write_all(&req).map_err(TransportError::from)?;

    let response = read_stream(transport)?;

    if response.id != id.to_string() {
        return Err(TransportError::NonceMismatch.into());
    }

    Ok(response)
}

fn read_stream<D: DeserializeOwned>(
    stream: &mut dyn SerialPort,
) -> Result<api::Response<D>, TransportError> {
    let mut buf = Vec::<u8>::new();
    let mut chunk = [0u8; 1024];

    // Phase 1: initial read with long timeout (device may need time to respond)
    stream
        .set_timeout(JADE_RESPONSE_TIMEOUT)
        .map_err(TransportError::from)?;
    match stream.read(&mut chunk) {
        Ok(0) => {}
        Ok(n) => {
            buf.extend_from_slice(&chunk[..n]);
            if let Ok(response) = serde_cbor::from_slice(&buf) {
                return Ok(response);
            }
        }
        Err(e) => return Err(TransportError::Io(e)),
    }

    // Phase 2: subsequent reads with short inter-chunk timeout (mirrors 1s tokio::select!)
    stream
        .set_timeout(JADE_INTER_CHUNK_TIMEOUT)
        .map_err(TransportError::from)?;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Ok(response) = serde_cbor::from_slice(&buf) {
                    return Ok(response);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(e) => return Err(TransportError::Io(e)),
        }
    }

    match serde_cbor::from_slice(&buf) {
        Ok(response) => Ok(response),
        Err(_) => Err(TransportError::NoErrorOrResult),
    }
}

pub trait Transport: Debug {
    fn request<S: Serialize + Unpin, D: DeserializeOwned + Unpin>(
        &self,
        method: &str,
        params: Option<S>,
    ) -> Result<api::Response<D>, JadeError>;
}

impl Jade<SerialTransport> {
    pub fn enumerate() -> Result<Vec<Self>, JadeError> {
        let mut res = Vec::new();
        for port_name in SerialTransport::enumerate_potential_ports()? {
            let jade = Jade::<SerialTransport>::new(SerialTransport::new(port_name)?);
            jade.ping()?;
            res.push(jade);
        }
        Ok(res)
    }
}

pub struct SerialTransport {
    pub stream: Arc<Mutex<Box<dyn SerialPort + Send>>>,
}

impl std::fmt::Debug for SerialTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerialTransport").finish()
    }
}

pub const JADE_DEVICE_IDS: [(u16, u16); 6] = [
    (0x10c4, 0xea60),
    (0x1a86, 0x55d4),
    (0x0403, 0x6001),
    (0x1a86, 0x7523),
    (0x303a, 0x4001),
    (0x303a, 0x1001),
];

const DEFAULT_JADE_BAUD_RATE: u32 = 115200;
const JADE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const JADE_INTER_CHUNK_TIMEOUT: Duration = Duration::from_millis(1000);

impl SerialTransport {
    pub fn new(port_name: String) -> Result<Self, TransportError> {
        let mut port = serialport::new(port_name, DEFAULT_JADE_BAUD_RATE)
            .timeout(JADE_RESPONSE_TIMEOUT)
            .open()
            .map_err(TransportError::from)?;
        // Ensure RTS and DTR are not set (as this can cause the hw to reboot)
        // according to https://github.com/Blockstream/Jade/blob/master/jadepy/jade_serial.py#L56
        port.write_request_to_send(false)
            .map_err(TransportError::from)?;
        port.write_data_terminal_ready(false)
            .map_err(TransportError::from)?;
        Ok(Self {
            stream: Arc::new(Mutex::new(port)),
        })
    }

    pub fn enumerate_potential_ports() -> Result<Vec<String>, JadeError> {
        match available_ports() {
            Ok(ports) => Ok(ports
                .into_iter()
                .filter_map(|p| match p.port_type {
                    SerialPortType::UsbPort(info) => {
                        if JADE_DEVICE_IDS.contains(&(info.vid, info.pid)) {
                            Some(p.port_name)
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .collect()),
            Err(e) => Err(JadeError::Transport(e.into())),
        }
    }
}

impl Transport for SerialTransport {
    fn request<S: Serialize + Unpin, D: DeserializeOwned + Unpin>(
        &self,
        method: &str,
        params: Option<S>,
    ) -> Result<api::Response<D>, JadeError> {
        let mut guard = self.stream.lock().map_err(|e| {
            JadeError::Transport(TransportError::Io(std::io::Error::other(e.to_string())))
        })?;
        exchange(guard.as_mut(), method, params)
    }
}

#[derive(Debug)]
pub enum TransportError {
    Serialize(serde_cbor::Error),
    NoErrorOrResult,
    NonceMismatch,
    Io(std::io::Error),
    Serial(serialport::Error),
}

impl From<serde_cbor::Error> for TransportError {
    fn from(e: serde_cbor::Error) -> Self {
        Self::Serialize(e)
    }
}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serialport::Error> for TransportError {
    fn from(e: serialport::Error) -> Self {
        Self::Serial(e)
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Serialize(e) => write!(f, "{e}"),
            Self::NoErrorOrResult => write!(f, "No Error or Result"),
            Self::NonceMismatch => write!(f, "Nonce mismatched"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Serial(e) => write!(f, "{e}"),
        }
    }
}

#[derive(Debug)]
pub enum JadeError {
    Transport(TransportError),
    Rpc(api::Error),
    PinServer(pinserver::Error),
    HandShakeRefused,
}

impl From<TransportError> for JadeError {
    fn from(e: TransportError) -> Self {
        Self::Transport(e)
    }
}

impl From<pinserver::Error> for JadeError {
    fn from(e: pinserver::Error) -> Self {
        Self::PinServer(e)
    }
}

impl std::fmt::Display for JadeError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "{e}"),
            Self::Rpc(e) => write!(f, "{e:?}"),
            Self::PinServer(e) => write!(f, "{e:?}"),
            Self::HandShakeRefused => write!(f, "Handshake with pinserver refused"),
        }
    }
}

impl From<JadeError> for HWIError {
    fn from(e: JadeError) -> HWIError {
        match e {
            JadeError::Transport(e) => HWIError::Device(e.to_string()),
            JadeError::Rpc(e) => {
                if e.code == api::ErrorCode::UserCancelled as i32 {
                    HWIError::UserRefused
                } else if e.code == api::ErrorCode::NetworkMismatch as i32 {
                    HWIError::NetworkMismatch
                } else {
                    HWIError::Device(format!("{e:?}"))
                }
            }
            JadeError::PinServer(e) => HWIError::Device(format!("{e:?}")),
            JadeError::HandShakeRefused => {
                HWIError::Device("Handshake with pinserver refused".to_string())
            }
        }
    }
}
