//! BOM, binary and encoding handling.

use crate::BINARY_SNIFF_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Latin1,
}

impl Encoding {
    pub fn as_str(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf8Bom => "utf-8-bom",
            Encoding::Utf16Le => "utf-16le",
            Encoding::Utf16Be => "utf-16be",
            Encoding::Latin1 => "latin-1",
        }
    }
}

fn has_utf16_bom(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF])
}

/// True when the first `BINARY_SNIFF_BYTES` contain a NUL byte, or more than 10% of them are
/// control bytes other than tab, newline, carriage return and form feed. UTF-16 input with a
/// BOM is never binary.
pub fn is_binary(bytes: &[u8]) -> bool {
    if has_utf16_bom(bytes) {
        return false;
    }
    let sample = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    if sample.contains(&0) {
        return true;
    }
    let control = sample
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, 0x09 | 0x0A | 0x0D | 0x0C))
        .count();
    control * 10 > sample.len()
}

/// Decode to text. A UTF-8 BOM is dropped. UTF-16 needs a BOM; an unpaired surrogate becomes
/// U+FFFD and adds a message. Other invalid UTF-8 is decoded as Latin-1 and adds one message.
/// Line endings are normalised to `\n`.
pub fn decode(bytes: &[u8]) -> (String, Encoding, Option<String>) {
    let (text, enc, msg) = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        match std::str::from_utf8(rest) {
            Ok(s) => (s.to_string(), Encoding::Utf8Bom, None),
            Err(_) => latin1(bytes),
        }
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest, Encoding::Utf16Le)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        utf16(rest, Encoding::Utf16Be)
    } else {
        match std::str::from_utf8(bytes) {
            Ok(s) => (s.to_string(), Encoding::Utf8, None),
            Err(_) => latin1(bytes),
        }
    };
    (normalise(text), enc, msg)
}

fn latin1(bytes: &[u8]) -> (String, Encoding, Option<String>) {
    (
        bytes.iter().map(|&b| b as char).collect(),
        Encoding::Latin1,
        Some("not valid UTF-8; decoded as Latin-1".to_string()),
    )
}

fn utf16(rest: &[u8], enc: Encoding) -> (String, Encoding, Option<String>) {
    let mut bad = rest.len() % 2 == 1;
    let (pairs, _) = rest.as_chunks::<2>();
    let units = pairs.iter().map(|&p| {
        if enc == Encoding::Utf16Le {
            u16::from_le_bytes(p)
        } else {
            u16::from_be_bytes(p)
        }
    });
    let text: String = char::decode_utf16(units)
        .map(|r| {
            r.unwrap_or_else(|_| {
                bad = true;
                char::REPLACEMENT_CHARACTER
            })
        })
        .collect();
    let msg = bad.then(|| "unpaired UTF-16 surrogate replaced".to_string());
    (text, enc, msg)
}

fn normalise(text: String) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    }
}
