use alloc::{
    string::{FromUtf8Error, String},
    vec::Vec,
};

use crate::{MAX_BYTES, MAX_VEC};

pub const MAX_STRING: usize = 64 * 1024;

/// Byte-level read failures, with no knowledge of the protocol layered on top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated,
    LengthOverflow,
    NonCanonicalCompactSize,
    CompactSizeTooLarge(u64),
    InvalidBool(u8),
    InvalidPresence(u8),
    StringTooLarge(usize),
    BytesTooLarge(usize),
    VecTooLarge(usize),
    InvalidFixedStringPadding,
    InvalidUtf8(FromUtf8Error),
    StringNul,
}

impl Error {
    pub fn info(&self) -> (i32, &'static str) {
        match self {
            Self::Truncated => (100, "truncated field\0"),
            Self::LengthOverflow => (101, "length overflow\0"),
            Self::NonCanonicalCompactSize => (102, "non-canonical compact size\0"),
            Self::CompactSizeTooLarge(_) => (103, "compact size is too large for this platform\0"),
            Self::InvalidBool(_) => (104, "invalid bool\0"),
            Self::InvalidPresence(_) => (105, "invalid option presence\0"),
            Self::StringTooLarge(_) => (106, "string exceeds the maximum length\0"),
            Self::BytesTooLarge(_) => (107, "byte field exceeds the maximum length\0"),
            Self::VecTooLarge(_) => (108, "vector exceeds the maximum item count\0"),
            Self::InvalidFixedStringPadding => (109, "invalid fixed string padding\0"),
            Self::InvalidUtf8(_) => (110, "invalid utf-8 string\0"),
            Self::StringNul => (111, "string contains a nul byte\0"),
        }
    }
}

error_display!(Error);

impl From<FromUtf8Error> for Error {
    fn from(error: FromUtf8Error) -> Self {
        Self::InvalidUtf8(error)
    }
}

pub struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.slice(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(Error::InvalidBool(value)),
        }
    }

    pub fn u16_be(&mut self) -> Result<u16, Error> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub fn u32_be(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub fn bytes(&mut self) -> Result<Vec<u8>, Error> {
        let len = self.compact()?;
        if len > MAX_BYTES {
            return Err(Error::BytesTooLarge(len));
        }
        Ok(self.slice(len)?.to_vec())
    }

    pub fn string(&mut self) -> Result<String, Error> {
        let len = self.compact()?;
        if len > MAX_STRING {
            return Err(Error::StringTooLarge(len));
        }
        let bytes = self.slice(len)?;
        if bytes.contains(&0) {
            return Err(Error::StringNul);
        }
        Ok(String::from_utf8(bytes.to_vec())?)
    }

    pub fn fixed_string(&mut self, len: usize) -> Result<String, Error> {
        let bytes = self.slice(len)?;
        let nul = bytes.iter().position(|b| *b == 0).unwrap_or(len);
        if bytes[nul..].iter().any(|b| *b != 0) {
            return Err(Error::InvalidFixedStringPadding);
        }
        Ok(String::from_utf8(bytes[..nul].to_vec())?)
    }

    // Generic over the item error so a protocol-level reader can be passed in.
    pub fn option<T, E, F>(&mut self, read: F) -> Result<Option<T>, E>
    where
        E: From<Error>,
        F: FnOnce(&mut Self) -> Result<T, E>,
    {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(read(self)?)),
            value => Err(Error::InvalidPresence(value).into()),
        }
    }

    // The count is attacker-controlled, so the vector grows as items are read rather
    // than reserving up front.
    pub fn vec<T, E, F>(&mut self, mut read: F) -> Result<Vec<T>, E>
    where
        E: From<Error>,
        F: FnMut(&mut Self) -> Result<T, E>,
    {
        let len = self.compact()?;
        if len > MAX_VEC {
            return Err(Error::VecTooLarge(len).into());
        }
        let mut items = Vec::new();
        for _ in 0..len {
            items.push(read(self)?);
        }
        Ok(items)
    }

    pub fn compact(&mut self) -> Result<usize, Error> {
        let first = self.u8()?;
        let value = match first {
            0x00..=0xfc => first as u64,
            0xfd => {
                let value = u16::from_le_bytes(self.array()?) as u64;
                if value < 0xfd {
                    return Err(Error::NonCanonicalCompactSize);
                }
                value
            }
            0xfe => {
                let value = u32::from_le_bytes(self.array()?) as u64;
                if value <= 0xffff {
                    return Err(Error::NonCanonicalCompactSize);
                }
                value
            }
            0xff => {
                let value = u64::from_le_bytes(self.array()?);
                if value <= 0xffff_ffff {
                    return Err(Error::NonCanonicalCompactSize);
                }
                value
            }
        };
        usize::try_from(value).map_err(|_| Error::CompactSizeTooLarge(value))
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.slice(N)?);
        Ok(out)
    }

    pub fn slice(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let end = self.cursor.checked_add(len).ok_or(Error::LengthOverflow)?;
        if end > self.bytes.len() {
            return Err(Error::Truncated);
        }
        let slice = &self.bytes[self.cursor..end];
        self.cursor = end;
        Ok(slice)
    }
}
