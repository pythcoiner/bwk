use alloc::vec::Vec;

use crate::{
    reader::{self, Reader},
    request, response, DerivationPath, Fingerprint, Message, MessageType, PublicKey, Request,
    RequestId, Response, SignResponseKind, Xpub, DIRECTION_RESPONSE, ERROR_MESSAGE_LEN, MAGIC,
    MODEL_LEN, STATUS_ERROR, TYPE_MASK,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidMagic,
    ReservedVersion,
    UnknownMessageType(u8),
    ErrorStatusOnRequest,
    UnknownDescriptorForm(u8),
    UnknownSignatureKind(u8),
    UnknownSignResponseKind(u8),
    Read(reader::Error),
}

impl Error {
    pub fn info(&self) -> (i32, &'static str) {
        match self {
            Self::InvalidMagic => (200, "invalid magic\0"),
            Self::ReservedVersion => (201, "reserved protocol version\0"),
            Self::UnknownMessageType(_) => (202, "unknown message type\0"),
            Self::ErrorStatusOnRequest => (203, "request cannot carry error status\0"),
            Self::UnknownDescriptorForm(_) => (204, "unknown descriptor form\0"),
            Self::UnknownSignatureKind(_) => (205, "unknown signature kind\0"),
            Self::UnknownSignResponseKind(_) => (206, "unknown signing response kind\0"),
            Self::Read(inner) => inner.info(),
        }
    }
}

error_display!(Error);

impl From<reader::Error> for Error {
    fn from(error: reader::Error) -> Self {
        Self::Read(error)
    }
}

pub fn decode(bytes: &[u8]) -> Result<Message, Error> {
    let mut reader = Reader::new(bytes);
    read_magic(&mut reader)?;
    // A newer version only appends fields, so anything above VERSION parses as VERSION.
    if reader.u8()? == 0 {
        return Err(Error::ReservedVersion);
    }
    let msg_type = reader.u8()?;
    let response = msg_type & DIRECTION_RESPONSE != 0;
    let error = msg_type & STATUS_ERROR != 0;
    let message_type = MessageType::try_from(msg_type & TYPE_MASK)?;
    if error && !response {
        return Err(Error::ErrorStatusOnRequest);
    }
    let id = RequestId(reader.array()?);
    // The error flag is already known to be clear on a request.
    let decoded = match (response, error, message_type) {
        (false, _, MessageType::GetXpubs) => Message::Request(Request {
            id,
            body: request::Body::GetXpubs(read_get_xpubs_request(&mut reader)?),
        }),
        (false, _, MessageType::RegisterDescriptor) => Message::Request(Request {
            id,
            body: request::Body::RegisterDescriptor(read_register_request(&mut reader)?),
        }),
        (false, _, MessageType::AddressVerification) => Message::Request(Request {
            id,
            body: request::Body::VerifyAddress(read_verify_request(&mut reader)?),
        }),
        (false, _, MessageType::Signing) => Message::Request(Request {
            id,
            body: request::Body::Sign(read_sign_request(&mut reader)?),
        }),
        (true, true, _) => Message::Response(Response {
            id,
            body: response::Body::Error(read_error_response(&mut reader, message_type)?),
        }),
        (true, false, MessageType::GetXpubs) => Message::Response(Response {
            id,
            body: response::Body::Xpubs(read_xpubs_response(&mut reader)?),
        }),
        (true, false, MessageType::RegisterDescriptor) => Message::Response(Response {
            id,
            body: response::Body::Registration(read_registration_response(&mut reader)?),
        }),
        (true, false, MessageType::AddressVerification) => Message::Response(Response {
            id,
            body: response::Body::AddressUri(read_address_response(&mut reader)?),
        }),
        (true, false, MessageType::Signing) => Message::Response(Response {
            id,
            body: response::Body::Signed(read_signed_response(&mut reader)?),
        }),
    };
    Ok(decoded)
}

fn read_magic(reader: &mut Reader<'_>) -> Result<(), Error> {
    if reader.slice(MAGIC.len())? != MAGIC {
        return Err(Error::InvalidMagic);
    }
    Ok(())
}

fn read_get_xpubs_request(reader: &mut Reader<'_>) -> Result<request::GetXpubs, Error> {
    Ok(request::GetXpubs {
        derivation_paths: reader.vec(read_path)?,
    })
}

fn read_register_request(reader: &mut Reader<'_>) -> Result<request::RegisterDescriptor, Error> {
    Ok(request::RegisterDescriptor {
        descriptor_alias: reader.string()?,
        descriptor: reader.option(read_descriptor_body)?,
    })
}

fn read_verify_request(reader: &mut Reader<'_>) -> Result<request::VerifyAddress, Error> {
    Ok(request::VerifyAddress {
        descriptor_alias: reader.string()?,
        derivation_path: read_path(reader)?,
        address: reader.option(Reader::string)?,
        descriptor: reader.option(read_descriptor_body)?,
        proof: reader.option(Reader::bytes)?,
    })
}

fn read_sign_request(reader: &mut Reader<'_>) -> Result<request::Sign, Error> {
    Ok(request::Sign {
        descriptors: reader.vec(read_descriptor)?,
        psbt: reader.bytes()?,
        want_kind: reader.option(read_sign_response_kind)?,
    })
}

fn read_xpubs_response(reader: &mut Reader<'_>) -> Result<response::Xpubs, Error> {
    Ok(response::Xpubs {
        xpubs: reader.vec(read_xpub)?,
        fingerprint: Fingerprint(reader.array()?),
        model: reader.fixed_string(MODEL_LEN)?,
        version: read_version(reader)?,
        capabilities: response::Capabilities(reader.u32_be()?),
    })
}

fn read_registration_response(reader: &mut Reader<'_>) -> Result<response::Registration, Error> {
    Ok(response::Registration {
        descriptor_alias: reader.string()?,
        registered: reader.option(Reader::bool)?,
        stored: reader.option(Reader::bool)?,
        proof: reader.option(Reader::bytes)?,
    })
}

fn read_address_response(reader: &mut Reader<'_>) -> Result<response::AddressUri, Error> {
    Ok(response::AddressUri {
        uri: reader.option(Reader::string)?,
    })
}

fn read_signed_response(reader: &mut Reader<'_>) -> Result<response::Signed, Error> {
    match read_sign_response_kind(reader)? {
        SignResponseKind::Psbt => Ok(response::Signed::Psbt(reader.bytes()?)),
        SignResponseKind::Signatures => {
            Ok(response::Signed::Signatures(reader.vec(read_signature)?))
        }
    }
}

fn read_error_response(
    reader: &mut Reader<'_>,
    message_type: MessageType,
) -> Result<response::ErrorBody, Error> {
    Ok(response::ErrorBody {
        message_type,
        error: reader.u8()?.into(),
        message: reader.fixed_string(ERROR_MESSAGE_LEN)?,
    })
}

fn read_descriptor(reader: &mut Reader<'_>) -> Result<request::Descriptor, Error> {
    Ok(request::Descriptor {
        alias: reader.string()?,
        body: read_descriptor_body(reader)?,
        proof: reader.option(Reader::bytes)?,
    })
}

fn read_descriptor_body(reader: &mut Reader<'_>) -> Result<request::DescriptorBody, Error> {
    match reader.u8()? {
        request::DESCRIPTOR_BIP380 => Ok(request::DescriptorBody::Bip380(reader.string()?)),
        request::DESCRIPTOR_BIP388 => Ok(request::DescriptorBody::Bip388 {
            keys: reader.vec(Reader::string)?,
            policy: reader.string()?,
        }),
        value => Err(Error::UnknownDescriptorForm(value)),
    }
}

fn read_signature(reader: &mut Reader<'_>) -> Result<response::SignatureEntry, Error> {
    let input_index = reader.u32_be()?;
    match reader.u8()? {
        response::SIGNATURE_ECDSA => Ok(response::SignatureEntry::Ecdsa {
            input_index,
            public_key: PublicKey(reader.array()?),
            signature: reader.bytes()?,
        }),
        response::SIGNATURE_TAP_KEY => Ok(response::SignatureEntry::TapKey {
            input_index,
            signature: reader.bytes()?,
        }),
        response::SIGNATURE_TAP_SCRIPT => Ok(response::SignatureEntry::TapScript {
            input_index,
            xonly_public_key: reader.array()?,
            tap_leaf_hash: reader.array()?,
            signature: reader.bytes()?,
        }),
        value => Err(Error::UnknownSignatureKind(value)),
    }
}

fn read_path(reader: &mut Reader<'_>) -> Result<DerivationPath, Error> {
    let count = reader.u8()? as usize;
    let mut children = Vec::new();
    for _ in 0..count {
        children.push(reader.u32_be()?);
    }
    Ok(DerivationPath(children))
}

fn read_xpub(reader: &mut Reader<'_>) -> Result<Xpub, Error> {
    Ok(Xpub(reader.array()?))
}

fn read_version(reader: &mut Reader<'_>) -> Result<response::FirmwareVersion, Error> {
    let major = reader.u16_be()?;
    let minor = reader.u16_be()?;
    let patch = ((reader.u8()? as u32) << 16) | ((reader.u8()? as u32) << 8) | reader.u8()? as u32;
    let flag = response::ReleaseFlag::from(reader.u8()?);
    Ok(response::FirmwareVersion {
        major,
        minor,
        patch,
        flag,
    })
}

fn read_sign_response_kind(reader: &mut Reader<'_>) -> Result<SignResponseKind, Error> {
    SignResponseKind::try_from(reader.u8()?)
}
