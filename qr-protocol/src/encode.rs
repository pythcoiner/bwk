use alloc::{string::String, vec::Vec};

use crate::{
    request, response, DerivationPath, MessageType, Request, RequestId, Response, SignResponseKind,
    Xpub, DIRECTION_RESPONSE, ERROR_MESSAGE_LEN, MAGIC, MAX_BYTES, MAX_VEC, MODEL_LEN,
    REQUEST_ID_LEN, STATUS_ERROR, VERSION,
};

pub const MAX_PATH: usize = 255;
pub const MAX_PATCH: u32 = 0x00ff_ffff;
const ERROR_CODE_FUTURE_MIN: u8 = 0x0c;
const ERROR_CODE_FUTURE_MAX: u8 = 0xfe;
const CAPABILITIES_MASK: u32 = 0x0000_000f;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    PathTooLong(usize),
    PatchTooLarge(u32),
    ReservedCapabilityBits(u32),
    ErrorCodeOutOfRange(u8),
    VecTooLarge(usize),
    BytesTooLarge(usize),
    StringNul,
    FixedStringTooLong { len: usize, max: usize },
}

impl Error {
    pub fn info(&self) -> (i32, &'static str) {
        match self {
            Self::PathTooLong(_) => (300, "derivation path exceeds the maximum depth\0"),
            Self::PatchTooLarge(_) => (301, "firmware patch version is too large\0"),
            Self::ReservedCapabilityBits(_) => (302, "reserved capability bits are set\0"),
            Self::ErrorCodeOutOfRange(_) => {
                (303, "unknown error code is outside the reserved range\0")
            }
            Self::VecTooLarge(_) => (304, "vector exceeds the maximum item count\0"),
            Self::BytesTooLarge(_) => (305, "byte field exceeds the maximum length\0"),
            Self::StringNul => (306, "string contains a nul byte\0"),
            Self::FixedStringTooLong { .. } => (307, "string does not fit its fixed field\0"),
        }
    }
}

error_display!(Error);

pub fn encode_request(request: &Request) -> Result<Vec<u8>, Error> {
    let mut out = envelope(request.body.message_type(), false, false, request.id);
    encode_request_body(&mut out, &request.body)?;
    Ok(out)
}

pub fn encode_response(response: &Response) -> Result<Vec<u8>, Error> {
    let status = matches!(response.body, response::Body::Error(_));
    let mut out = envelope(response.body.message_type(), true, status, response.id);
    encode_response_body(&mut out, &response.body)?;
    Ok(out)
}

fn envelope(message_type: MessageType, response: bool, error: bool, id: RequestId) -> Vec<u8> {
    let mut out = Vec::with_capacity(MAGIC.len() + 2 + REQUEST_ID_LEN);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(
        message_type.value()
            | if response { DIRECTION_RESPONSE } else { 0 }
            | if error { STATUS_ERROR } else { 0 },
    );
    out.extend_from_slice(&id.0);
    out
}

fn encode_request_body(out: &mut Vec<u8>, body: &request::Body) -> Result<(), Error> {
    match body {
        request::Body::GetXpubs(request) => write_vec(out, &request.derivation_paths, write_path),
        request::Body::RegisterDescriptor(request) => {
            write_string(out, &request.descriptor_alias)?;
            write_option(out, request.descriptor.as_ref(), write_descriptor_body)
        }
        request::Body::VerifyAddress(request) => {
            write_string(out, &request.descriptor_alias)?;
            write_path(out, &request.derivation_path)?;
            write_option(out, request.address.as_ref(), write_string)?;
            write_option(out, request.descriptor.as_ref(), write_descriptor_body)?;
            write_option(out, request.proof.as_ref(), |out, bytes| {
                write_bytes(out, bytes)
            })
        }
        request::Body::Sign(request) => {
            write_vec(out, &request.descriptors, write_descriptor)?;
            write_bytes(out, &request.psbt)?;
            write_option(out, request.want_kind.as_ref(), |out, kind| {
                out.push(kind.value());
                Ok(())
            })
        }
    }
}

fn encode_response_body(out: &mut Vec<u8>, body: &response::Body) -> Result<(), Error> {
    match body {
        response::Body::Xpubs(response) => {
            write_vec(out, &response.xpubs, write_xpub)?;
            out.extend_from_slice(&response.fingerprint.0);
            write_fixed_string(out, &response.model, MODEL_LEN)?;
            write_version(out, &response.version)?;
            write_capabilities(out, &response.capabilities)
        }
        response::Body::Registration(response) => {
            write_string(out, &response.descriptor_alias)?;
            write_option(out, response.registered.as_ref(), write_bool)?;
            write_option(out, response.stored.as_ref(), write_bool)?;
            write_option(out, response.proof.as_ref(), |out, bytes| {
                write_bytes(out, bytes)
            })
        }
        response::Body::AddressUri(response) => {
            write_option(out, response.uri.as_ref(), write_string)
        }
        response::Body::Signed(response) => match response {
            response::Signed::Psbt(psbt) => {
                out.push(SignResponseKind::Psbt.value());
                write_bytes(out, psbt)
            }
            response::Signed::Signatures(signatures) => {
                out.push(SignResponseKind::Signatures.value());
                write_vec(out, signatures, write_signature)
            }
        },
        response::Body::Error(response) => {
            write_error_code(out, response.error)?;
            write_fixed_string(out, &response.message, ERROR_MESSAGE_LEN)
        }
    }
}

fn write_descriptor(out: &mut Vec<u8>, descriptor: &request::Descriptor) -> Result<(), Error> {
    write_string(out, &descriptor.alias)?;
    write_descriptor_body(out, &descriptor.body)?;
    write_option(out, descriptor.proof.as_ref(), |out, bytes| {
        write_bytes(out, bytes)
    })
}

fn write_descriptor_body(out: &mut Vec<u8>, body: &request::DescriptorBody) -> Result<(), Error> {
    out.push(body.value());
    match body {
        request::DescriptorBody::Bip380(value) => write_string(out, value),
        request::DescriptorBody::Bip388 { keys, policy } => {
            write_vec(out, keys, write_string)?;
            write_string(out, policy)
        }
    }
}

fn write_signature(out: &mut Vec<u8>, entry: &response::SignatureEntry) -> Result<(), Error> {
    let kind = entry.value();
    match entry {
        response::SignatureEntry::Ecdsa {
            input_index,
            public_key,
            signature,
        } => {
            write_signature_header(out, *input_index, kind);
            out.extend_from_slice(&public_key.0);
            write_bytes(out, signature)
        }
        response::SignatureEntry::TapKey {
            input_index,
            signature,
        } => {
            write_signature_header(out, *input_index, kind);
            write_bytes(out, signature)
        }
        response::SignatureEntry::TapScript {
            input_index,
            xonly_public_key,
            tap_leaf_hash,
            signature,
        } => {
            write_signature_header(out, *input_index, kind);
            out.extend_from_slice(xonly_public_key);
            out.extend_from_slice(tap_leaf_hash);
            write_bytes(out, signature)
        }
    }
}

fn write_signature_header(out: &mut Vec<u8>, input_index: u32, kind: u8) {
    out.extend_from_slice(&input_index.to_be_bytes());
    out.push(kind);
}

fn write_path(out: &mut Vec<u8>, path: &DerivationPath) -> Result<(), Error> {
    if path.0.len() > MAX_PATH {
        return Err(Error::PathTooLong(path.0.len()));
    }
    out.push(path.0.len() as u8);
    for child in &path.0 {
        out.extend_from_slice(&child.to_be_bytes());
    }
    Ok(())
}

fn write_xpub(out: &mut Vec<u8>, xpub: &Xpub) -> Result<(), Error> {
    out.extend_from_slice(&xpub.0);
    Ok(())
}

fn write_version(out: &mut Vec<u8>, version: &response::FirmwareVersion) -> Result<(), Error> {
    if version.patch > MAX_PATCH {
        return Err(Error::PatchTooLarge(version.patch));
    }
    out.extend_from_slice(&version.major.to_be_bytes());
    out.extend_from_slice(&version.minor.to_be_bytes());
    out.push((version.patch >> 16) as u8);
    out.push((version.patch >> 8) as u8);
    out.push(version.patch as u8);
    out.push(version.flag.value());
    Ok(())
}

fn write_capabilities(
    out: &mut Vec<u8>,
    capabilities: &response::Capabilities,
) -> Result<(), Error> {
    if capabilities.0 & !CAPABILITIES_MASK != 0 {
        return Err(Error::ReservedCapabilityBits(capabilities.0));
    }
    out.extend_from_slice(&capabilities.0.to_be_bytes());
    Ok(())
}

fn write_error_code(out: &mut Vec<u8>, error: response::Error) -> Result<(), Error> {
    if let response::Error::Unknown(value) = error {
        if !(ERROR_CODE_FUTURE_MIN..=ERROR_CODE_FUTURE_MAX).contains(&value) {
            return Err(Error::ErrorCodeOutOfRange(value));
        }
    }
    out.push(error.value());
    Ok(())
}

fn write_option<T, F>(out: &mut Vec<u8>, item: Option<&T>, write: F) -> Result<(), Error>
where
    F: FnOnce(&mut Vec<u8>, &T) -> Result<(), Error>,
{
    match item {
        Some(item) => {
            out.push(1);
            write(out, item)
        }
        None => {
            out.push(0);
            Ok(())
        }
    }
}

fn write_vec<T, F>(out: &mut Vec<u8>, items: &[T], mut write: F) -> Result<(), Error>
where
    F: FnMut(&mut Vec<u8>, &T) -> Result<(), Error>,
{
    if items.len() > MAX_VEC {
        return Err(Error::VecTooLarge(items.len()));
    }
    write_compact(out, items.len() as u64);
    for item in items {
        write(out, item)?;
    }
    Ok(())
}

fn write_bool(out: &mut Vec<u8>, value: &bool) -> Result<(), Error> {
    out.push(u8::from(*value));
    Ok(())
}

fn write_string(out: &mut Vec<u8>, s: &String) -> Result<(), Error> {
    if s.as_bytes().contains(&0) {
        return Err(Error::StringNul);
    }
    write_bytes(out, s.as_bytes())
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::BytesTooLarge(bytes.len()));
    }
    write_compact(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
    Ok(())
}

fn write_fixed_string(out: &mut Vec<u8>, s: &str, len: usize) -> Result<(), Error> {
    if s.as_bytes().contains(&0) {
        return Err(Error::StringNul);
    }
    if s.len() > len {
        return Err(Error::FixedStringTooLong {
            len: s.len(),
            max: len,
        });
    }
    out.extend_from_slice(s.as_bytes());
    out.resize(out.len() + len - s.len(), 0);
    Ok(())
}

fn write_compact(out: &mut Vec<u8>, value: u64) {
    match value {
        0..=0xfc => out.push(value as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(value as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(value as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
}
