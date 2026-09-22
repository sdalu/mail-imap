//! Thunderbird's tag-key encoding.
//!
//! A Thunderbird tag has a display name and a colour, both kept in the
//! user's `prefs.js`; what reaches the server is the *key* alone, so a
//! keyword written by Thunderbird is unreadable on its own. This module
//! reads them back.
//!
//! `nsMsgTagService::AddTag` (comm-central,
//! `mailnews/base/src/nsMsgTagService.cpp`) builds the key from the
//! tag's UTF-8 bytes: ASCII alphanumerics are kept, so are printable
//! ASCII symbols outside ``=()[]{}%*"\<>;&``, and every other byte is
//! written `=%02x`. The whole key is then lowercased — which the source
//! calls harmless because hex is case-insensitive, and which is why a
//! key can never be turned back into the tag's original capitalisation.
//!
//! Only decoding is implemented. This tool writes IMAP's own modified
//! UTF-7 ([`super::modutf7`]) rather than adopting one client's scheme;
//! what it owes the reader is the ability to *read* what that client
//! wrote.
//!
//! Older Thunderbird encoded keys in modified UTF-7 instead — and
//! lowercased those too, which is how a `r&aok-gie` ends up in a live
//! mailbox with its base64 flattened.

/// The ASCII symbols Thunderbird refuses to keep literally, mirroring
/// the `strchr` in `AddTag`. They are the IMAP atom-specials plus the
/// characters its own escape scheme needs.
const ESCAPED: &str = "=()[]{}%*\"\\<>;&";

/// Decode a Thunderbird tag key, when the name can be one.
///
/// Returns `None` when the name is not something `AddTag` could have
/// produced (a character it would have escaped, a truncated `=xx`, bytes
/// that are not UTF-8) or when decoding changes nothing — so a plain
/// `invoice` stays silent, and only a key that actually says something
/// else speaks.
pub fn decode(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'=' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hi = (hex[0] as char).to_digit(16)?;
            let lo = (hex[1] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        // Anything Thunderbird would have escaped cannot appear raw in
        // one of its keys, so this is some other client's keyword.
        if !c.is_ascii_graphic() || ESCAPED.contains(c as char) {
            return None;
        }
        out.push(c);
        i += 1;
    }
    let text = String::from_utf8(out).ok()?;
    (text != name).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_accented_tag() {
        // "régie" is r + c3 a9 + gie, lowercased by AddTag.
        assert_eq!(decode("r=c3=a9gie").as_deref(), Some("régie"));
        // Hex digits are read in either case; the escaped byte is é
        // whatever the surrounding letters do. (AddTag lowercases the
        // whole key, so a real one never has the uppercase form.)
        assert_eq!(decode("R=C3=A9GIE").as_deref(), Some("RéGIE"));
    }

    #[test]
    fn reads_the_characters_thunderbird_escapes() {
        assert_eq!(decode("my=20tag").as_deref(), Some("my tag"));
        assert_eq!(decode("a=3db").as_deref(), Some("a=b"));
        assert_eq!(decode("=28paren=29").as_deref(), Some("(paren)"));
        assert_eq!(decode("caf=c3=a9=20au=20lait").as_deref(), Some("café au lait"));
    }

    #[test]
    fn stays_silent_when_there_is_nothing_to_say() {
        assert_eq!(decode("invoice"), None, "decodes to itself");
        assert_eq!(decode("$label1"), None);
        assert_eq!(decode("MyTag"), None);
    }

    #[test]
    fn refuses_what_thunderbird_could_not_have_written() {
        assert_eq!(decode("r&AOk-gie"), None, "'&' is escaped by AddTag");
        assert_eq!(decode("a b"), None, "a space would have been =20");
        assert_eq!(decode("régie"), None, "non-ASCII would have been escaped");
        assert_eq!(decode("a=c3"), None, "truncated sequence");
        assert_eq!(decode("a=zz"), None, "not hex");
        assert_eq!(decode("a=c3b"), None, "c3 alone is not UTF-8");
    }
}
