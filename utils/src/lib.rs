#[cfg(feature = "test")]
pub mod test;

#[cfg(target_os = "android")]
use std::io;

#[cfg(target_os = "android")]
pub fn android_root_certs() -> io::Result<Vec<Vec<u8>>> {
    let mut certs = Vec::new();

    for entry in std::fs::read_dir("/system/etc/security/cacerts")? {
        certs.push(std::fs::read(entry?.path())?);
    }

    if certs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "android cert store is empty",
        ));
    }

    Ok(certs)
}

pub fn short_string(s: String, len: usize) -> String {
    assert!(len > 6);
    let separator = if len % 2 != 0 { "." } else { ".." };
    let head = (len - 2).div_ceil(2);
    let tail = head;
    if s.len() <= head + tail + 2 {
        // No need to truncate if string is short
        return s.to_string();
    }
    format!("{}{separator}{}", &s[..head], &s[s.len() - tail..])
}
