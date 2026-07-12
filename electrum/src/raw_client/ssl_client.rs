use super::Error;
use std::{
    io::{ErrorKind, Read, Write},
    net::{self, SocketAddr},
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(target_os = "android")]
use {bwk_utils::android_root_certs, native_tls::Certificate};

/// A connected TLS stream plus the bytes already read past the last delimiter.
/// native-tls has no peek, so we keep a residual buffer across read calls to
/// implement a non-blocking `try_read`.
pub struct TlsState {
    tls: native_tls::TlsStream<net::TcpStream>,
    buf: Vec<u8>,
}

impl std::fmt::Debug for TlsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsState").finish()
    }
}

type SslStream = Arc<Mutex<TlsState>>;

#[derive(Debug)]
pub struct SslClient {
    url: String,
    port: u16,
    pub(crate) stream: Option<SslStream>,
    pub(crate) read_timeout: Option<Duration>,
    pub(crate) write_timeout: Option<Duration>,
    pub(crate) verif_certificate: bool,
}

impl Clone for SslClient {
    fn clone(&self) -> Self {
        Self {
            url: self.url.clone(),
            port: self.port,
            stream: self.stream.clone(),
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
            verif_certificate: self.verif_certificate,
        }
    }
}

impl Drop for SslClient {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

impl Default for SslClient {
    fn default() -> Self {
        Self {
            url: Default::default(),
            port: 50002,
            stream: None,
            read_timeout: None,
            write_timeout: None,
            verif_certificate: true,
        }
    }
}

impl SslClient {
    pub fn url(mut self, url: &str) -> Self {
        if !self.is_connected() {
            self.url = url.into();
        } else {
            log::error!("Cannot change url of a connected SslClient!")
        }
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        if !self.is_connected() {
            self.port = port;
        } else {
            log::error!("Cannot change port of a connected TcpClient!")
        }
        self
    }

    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    pub fn try_connect(&mut self, timeout: Option<Duration>) -> Result<(), Error> {
        let url = format!("{}:{}", self.url, self.port);
        let mut builder = native_tls::TlsConnector::builder();

        #[cfg(target_os = "android")]
        {
            for cert in android_root_certs().map_err(Error::Io)? {
                let cert = Certificate::from_pem(&cert)
                    .or_else(|_| Certificate::from_der(&cert))
                    .map_err(Error::Tls)?;
                builder.add_root_certificate(cert);
            }
        }

        // do not verify for self-signed certs
        if !self.verif_certificate {
            builder.danger_accept_invalid_certs(true);
            builder.danger_accept_invalid_hostnames(true);
        }
        let connector = builder.build().map_err(Error::Tls)?;
        let tcp = if let Some(timeout) = timeout {
            let addr = SocketAddr::from_str(&url).map_err(|_| Error::SocketAddr)?;
            net::TcpStream::connect_timeout(&addr, timeout).map_err(Error::TcpStream)?
        } else {
            net::TcpStream::connect(url).map_err(Error::TcpStream)?
        };
        tcp.set_read_timeout(self.read_timeout)
            .map_err(Error::TcpStream)?;
        tcp.set_write_timeout(self.write_timeout)
            .map_err(Error::TcpStream)?;
        let tls = connector
            .connect(&self.url, tcp)
            .map_err(Error::TlsHandshake)?;
        let state = Arc::new(Mutex::new(TlsState {
            tls,
            buf: Vec::new(),
        }));

        if self.stream.is_none() {
            self.stream = Some(state);
            Ok(())
        } else {
            Err(Error::AlreadyConnected)
        }
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<(), Error> {
        if let Some(stream) = self.stream.as_mut() {
            let stream = stream.lock().map_err(|_| Error::Mutex)?;
            stream
                .tls
                .get_ref()
                .set_read_timeout(timeout)
                .map_err(Error::TcpStream)?;
        }
        self.read_timeout = timeout;
        Ok(())
    }

    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> Result<(), Error> {
        if let Some(stream) = self.stream.as_mut() {
            let stream = stream.lock().map_err(|_| Error::Mutex)?;
            stream
                .tls
                .get_ref()
                .set_write_timeout(timeout)
                .map_err(Error::TcpStream)?;
        }
        self.write_timeout = timeout;
        Ok(())
    }

    pub fn send(state: &mut TlsState, request: &str) -> Result<(), Error> {
        state
            .tls
            .write_all(request.as_bytes())
            .map_err(Error::TcpStream)?;
        // add a \n char for EOL
        state.tls.write_all(&[10]).map_err(Error::TcpStream)?;
        state.tls.flush().map_err(Error::TcpStream)?;
        Ok(())
    }

    /// Split off the first line (including the trailing `\n`) from the residual
    /// buffer if it holds a complete one, leaving the remainder in `buf`.
    fn take_line(buf: &mut Vec<u8>) -> Option<String> {
        let pos = buf.iter().position(|b| *b == b'\n')?;
        let rest = buf.split_off(pos + 1);
        let line = std::mem::replace(buf, rest);
        Some(String::from_utf8_lossy(&line).into_owned())
    }

    /// Non-blocking read of one newline-terminated line. Returns `Ok(None)` when
    /// no full line is available yet.
    pub fn try_read(state: &mut TlsState) -> Result<Option<String>, Error> {
        if let Some(line) = Self::take_line(&mut state.buf) {
            return Ok(Some(line));
        }
        state
            .tls
            .get_mut()
            .set_nonblocking(true)
            .map_err(|_| Error::SetNonBlocking)?;
        let mut tmp = [0u8; 1024];
        let result = loop {
            match state.tls.read(&mut tmp) {
                // connection closed
                Ok(0) => break Ok(None),
                Ok(n) => {
                    state.buf.extend_from_slice(&tmp[..n]);
                    if let Some(line) = Self::take_line(&mut state.buf) {
                        break Ok(Some(line));
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break Ok(None),
                Err(e) => break Err(Error::TcpStream(e)),
            }
        };
        state
            .tls
            .get_mut()
            .set_nonblocking(false)
            .map_err(|_| Error::SetBlocking)?;
        result
    }

    /// Blocking read of one newline-terminated line.
    pub fn read(state: &mut TlsState) -> Result<String, Error> {
        state
            .tls
            .get_mut()
            .set_nonblocking(false)
            .map_err(|_| Error::SetBlocking)?;
        let mut tmp = [0u8; 1024];
        loop {
            if let Some(line) = Self::take_line(&mut state.buf) {
                return Ok(line);
            }
            match state.tls.read(&mut tmp) {
                // connection closed: flush whatever remains as the final line
                Ok(0) => {
                    let line = std::mem::take(&mut state.buf);
                    return Ok(String::from_utf8_lossy(&line).into_owned());
                }
                Ok(n) => state.buf.extend_from_slice(&tmp[..n]),
                Err(e) => return Err(Error::TcpStream(e)),
            }
        }
    }

    pub fn close(&mut self) -> Result<(), Error> {
        if let Some(stream) = self.stream.take() {
            stream
                .try_lock()
                .map_err(|_| Error::Mutex)?
                .tls
                .shutdown()
                .map_err(|_| Error::ShutDown)?;
            Ok(())
        } else {
            Err(Error::NotConnected)
        }
    }
}
