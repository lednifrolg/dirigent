//! Random values from the operating system.

use std::fmt::Write;

use crate::error::Error;

pub(crate) fn bytes<const N: usize>() -> Result<[u8; N], Error> {
    let mut buffer = [0; N];
    getrandom::fill(&mut buffer).map_err(Error::Random)?;
    Ok(buffer)
}

/// A random version 4 UUID, for event IDs.
pub(crate) fn uuid() -> Result<String, Error> {
    let mut b: [u8; 16] = bytes()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let mut text = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        write!(text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_have_the_version_4_shape() {
        let id = uuid().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
        assert_ne!(id, uuid().unwrap());
    }
}
