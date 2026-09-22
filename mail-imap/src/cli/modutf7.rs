//! IMAP modified UTF-7 (RFC 3501 §5.1.3).
//!
//! IMAP atoms are ASCII, so anything else — an accented mailbox name, a
//! Thunderbird tag called "régie" — travels encoded: printable ASCII
//! stands for itself, `&` becomes `&-`, and everything else is the
//! UTF-16BE code units in a base64 variant that writes `,` for `/`,
//! introduced by `&` and terminated by `-`.
//!
//! The encoding matters here for one reason beyond display: its payload
//! is **base64, so it is case-sensitive**. `r&AOk-gie` is "régie" and
//! `r&aok-gie` is "r檉gie" — two wire keys that differ only in case.
//! That is why comparison folds case on the *decoded* text, and only
//! for ASCII (see `same_keyword`).
//!
//! Note that whether a given name IS a wire key cannot be told from the
//! name: `pen&ink-notes` decodes cleanly (to "pen詹notes") and
//! `fish&chips-2024` does not. So keyword input is literal text by
//! default and `--wire` sends it verbatim.

use anyhow::{bail, Result};

/// Does this name carry a modified UTF-7 shift sequence?
pub fn is_encoded(name: &str) -> bool {
    name.contains('&')
}

/// Encode a name for the wire. Pure-ASCII names come back unchanged
/// except for a literal `&`, which has to be written `&-`.
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();

    fn flush(pending: &mut Vec<u16>, out: &mut String) {
        if pending.is_empty() {
            return;
        }
        let mut bytes = Vec::with_capacity(pending.len() * 2);
        for unit in pending.drain(..) {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        out.push('&');
        out.push_str(&to_modified_base64(&bytes));
        out.push('-');
    }

    for c in name.chars() {
        if c == '&' {
            flush(&mut pending, &mut out);
            out.push_str("&-");
        } else if ('\u{20}'..='\u{7e}').contains(&c) {
            flush(&mut pending, &mut out);
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Decode a wire name. Returns an error when the shift sequences are
/// malformed — a server may hand back anything, and a keyword that is
/// not valid modified UTF-7 is simply a keyword with an `&` in it.
pub fn decode(name: &str) -> Result<String> {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp + 1..];
        let Some(end) = rest.find('-') else {
            bail!("unterminated '&' sequence in '{}'", name);
        };
        let payload = &rest[..end];
        rest = &rest[end + 1..];
        if payload.is_empty() {
            out.push('&');
            continue;
        }
        let bytes = from_modified_base64(payload, name)?;
        if bytes.len() % 2 != 0 {
            bail!("odd number of bytes in '&{}-' of '{}'", payload, name);
        }
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .collect();
        match String::from_utf16(&units) {
            Ok(text) => out.push_str(&text),
            Err(_) => bail!("'&{}-' of '{}' is not valid UTF-16", payload, name),
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Is this exactly what `encode` would produce for its own decoding?
///
/// RFC 3501 §5.1.3 forbids encoding printable ASCII and forbids a null
/// shift, but a decoder accepts both, so `&AEE-` ("A") and
/// `&AOk-&AOk-` ("éé") decode cleanly while being nobody's output.
/// Treating those as wire forms would let two spellings of one name
/// past deduplication.
pub fn is_canonical(name: &str) -> bool {
    match decode(name) {
        Ok(text) => encode(&text) == name,
        Err(_) => false,
    }
}

/// The decoded form, when the name is encoded and decodes cleanly and
/// the result differs from what is on the wire. This is what a reader
/// wants to see beside the raw keyword.
pub fn decoded_display(name: &str) -> Option<String> {
    if !is_encoded(name) {
        return None;
    }
    match decode(name) {
        Ok(text) if text != name => Some(text),
        _ => None,
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

fn to_modified_base64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let chars = [
            ALPHABET[(n >> 18) as usize & 63],
            ALPHABET[(n >> 12) as usize & 63],
            ALPHABET[(n >> 6) as usize & 63],
            ALPHABET[n as usize & 63],
        ];
        // No padding: keep only the characters the input bits reach.
        let keep = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for c in &chars[..keep] {
            out.push(*c as char);
        }
    }
    out
}

fn from_modified_base64(payload: &str, whole: &str) -> Result<Vec<u8>> {
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for c in payload.chars() {
        let Some(value) = ALPHABET.iter().position(|a| *a as char == c) else {
            bail!("invalid base64 character '{}' in '{}'", c, whole);
        };
        acc = (acc << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    // Leftover bits must be zero padding, never data.
    if bits >= 6 || (acc & ((1 << bits) - 1)) != 0 {
        bail!("trailing bits in '&{}-' of '{}'", payload, whole);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3501_example_round_trips() {
        // The mailbox name from RFC 3501 §5.1.3.
        let decoded = "~peter/mail/台北/日本語";
        let encoded = "~peter/mail/&U,BTFw-/&ZeVnLIqe-";
        assert_eq!(encode(decoded), encoded);
        assert_eq!(decode(encoded).unwrap(), decoded);
    }

    #[test]
    fn thunderbird_tag_keys() {
        assert_eq!(encode("régie"), "r&AOk-gie");
        assert_eq!(decode("r&AOk-gie").unwrap(), "régie");
        // The same key with its base64 lowercased is a different word —
        // this is why keyword comparison cannot fold case blindly.
        assert_eq!(decode("r&aok-gie").unwrap(), "r檉gie");
        assert_ne!(decode("r&AOk-gie").unwrap(), decode("r&aok-gie").unwrap());
    }

    #[test]
    fn ascii_passes_through_and_ampersand_is_escaped() {
        assert_eq!(encode("invoice"), "invoice");
        assert_eq!(encode("INBOX"), "INBOX");
        assert_eq!(encode("R&D"), "R&-D");
        assert_eq!(decode("R&-D").unwrap(), "R&D");
        assert_eq!(decode("invoice").unwrap(), "invoice");
    }

    #[test]
    fn round_trip_of_awkward_names() {
        for name in ["régie", "台北", "a&b&c", "&", "é&é", "Ünïcödé tag", "$Important"] {
            assert_eq!(decode(&encode(name)).unwrap(), name, "round trip of {}", name);
        }
    }

    #[test]
    fn non_canonical_encodings_decode_but_are_not_wire_forms() {
        assert_eq!(decode("&AEE-").unwrap(), "A", "it does decode");
        assert!(!is_canonical("&AEE-"), "but encode would never emit it");
        assert!(!is_canonical("&AOk-&AOk-"), "a null shift between runs");
        assert!(is_canonical("r&AOk-gie"));
        assert!(is_canonical("invoice"));
        assert!(is_canonical("R&-D"));
        assert!(!is_canonical("AT&T"), "does not decode at all");
    }

    #[test]
    fn malformed_sequences_are_errors_not_guesses() {
        assert!(decode("r&AOk").is_err(), "unterminated");
        assert!(decode("&!!!-").is_err(), "invalid base64");
        assert!(decode("&A-").is_err(), "odd bits");
    }

    #[test]
    fn decoded_display_only_speaks_when_it_has_something_to_say() {
        assert_eq!(decoded_display("r&AOk-gie").as_deref(), Some("régie"));
        assert_eq!(decoded_display("invoice"), None, "not encoded");
        assert_eq!(decoded_display("R&-D").as_deref(), Some("R&D"));
        assert_eq!(decoded_display("r&AOk"), None, "undecodable: say nothing");
    }
}
