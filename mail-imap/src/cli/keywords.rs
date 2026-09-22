//! What is known about IMAP keywords: the IANA registry, and the
//! conventions no registry covers.
//!
//! Keywords are the user-defined half of an IMAP message's flags (the
//! half this tool calls tags): they carry no `\` prefix, and a server
//! may accept any atom. A subset is registered so that clients agree on
//! what they mean, and RFC 5788 §3.1 asks registered names to start with
//! `$`.
//!
//! The first table is that registry, copied on 2026-09-22 from
//! <https://www.iana.org/assignments/imap-jmap-keywords>. It is
//! advisory, never a whitelist: an unregistered keyword is perfectly
//! legal and is passed through as typed. What the table buys is the
//! *spelling* — a registered keyword typed in another case is sent in
//! its registered form, the same normalization `\seen` → `\Seen` gets.
//! The second table, [`WELL_KNOWN`], holds what real clients write
//! without any registry's blessing.

/// Registered keywords usable as IMAP keywords, in their registered
/// spelling.
const REGISTERED: &[&str] = &[
    "$MDNSent",         // RFC 3503
    "$Forwarded",       // RFC 5550
    "$SubmitPending",   // RFC 5550
    "$Submitted",       // RFC 5550
    "$Junk",            // Alexey Melnikov
    "$NotJunk",         // Alexey Melnikov
    "$Phishing",        // Rob Mueller
    "$Important",       // RFC 8457
    "$autosent",        // RFC 9979
    "$canunsubscribe",  // RFC 9979
    "$followed",        // RFC 9979
    "$hasattachment",   // RFC 9979
    "$hasmemo",         // RFC 9979
    "$hasnoattachment", // RFC 9979
    "$imported",        // RFC 9979
    "$istrusted",       // RFC 9979
    "$MailFlagBit0",    // RFC 9979
    "$MailFlagBit1",    // RFC 9979
    "$MailFlagBit2",    // RFC 9979
    "$maskedemail",     // RFC 9979
    "$memo",            // RFC 9979
    "$muted",           // RFC 9979
    "$new",             // RFC 9979
    "$notify",          // RFC 9979
    "$unsubscribed",    // RFC 9979
];

/// The registry is shared with JMAP, which spells four of IMAP's system
/// flags as keywords (RFC 8621 §4.1.1 changes their first character
/// from `\` to `$`), plus `$recent` for the flag IMAP4rev2 deprecated.
/// In IMAP these are *not* keywords, so they are refused with the flag
/// they stand for rather than stored as a tag nobody will read.
const JMAP_SPELLINGS: &[(&str, &str)] = &[
    ("$seen", "\\Seen"),
    ("$answered", "\\Answered"),
    ("$flagged", "\\Flagged"),
    ("$draft", "\\Draft"),
    ("$recent", "\\Recent"),
];

/// Keywords no registry defines but that turn up in real mailboxes,
/// with what they mean and who writes them.
///
/// This table is a convention, not a standard — it is never used for
/// validation, only to spell a name the way its writer does and to say
/// what it means when one is listed. Thunderbird keeps a tag's name and
/// colour in the user's `prefs.js` and sends only the key, so a
/// `$label1` arriving here is otherwise unreadable; its own traces show
/// both `$label1` and `$Label1`, which is a client agreeing that
/// keywords do not differ by case.
///
/// Thunderbird's *custom* tags are not listed because they cannot be:
/// `nsMsgTagService::AddTag` builds the key from the tag's UTF-8 bytes,
/// escaping anything outside a safe ASCII set as `=%02x` and
/// lowercasing the result, so the key is whatever the user typed. Older
/// versions encoded in modified UTF-7 instead and lowercased that too,
/// which is what a `r&aok-gie` in a live mailbox comes from.
const WELL_KNOWN: &[(&str, &str)] = &[
    ("$label1", "Thunderbird tag 1, \"Important\" unless renamed"),
    ("$label2", "Thunderbird tag 2, \"Work\" unless renamed"),
    ("$label3", "Thunderbird tag 3, \"Personal\" unless renamed"),
    ("$label4", "Thunderbird tag 4, \"To Do\" unless renamed"),
    ("$label5", "Thunderbird tag 5, \"Later\" unless renamed"),
    ("Junk", "junk marker (Thunderbird, SpamAssassin); the registry spells it $Junk"),
    ("NonJunk", "not-junk marker; the registry spells it $NotJunk"),
];

/// Every registered keyword usable in IMAP, in registry order.
pub fn registered() -> &'static [&'static str] {
    REGISTERED
}

/// Every well-known but unregistered keyword, with what it means.
pub fn well_known() -> &'static [(&'static str, &'static str)] {
    WELL_KNOWN
}

/// What a keyword means, when it is one this table knows about.
pub fn meaning(name: &str) -> Option<&'static str> {
    WELL_KNOWN
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, m)| *m)
}

/// The settled spelling of `name`: the registry's where it registers
/// one, else the spelling its writer uses, else none. Matching is
/// case-insensitive for the same reason it is for system flags — two
/// spellings of one name are never what the caller meant.
pub fn canonical(name: &str) -> Option<&'static str> {
    REGISTERED
        .iter()
        .find(|k| k.eq_ignore_ascii_case(name))
        .copied()
        .or_else(|| {
            WELL_KNOWN
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(k, _)| *k)
        })
}

/// The IMAP system flag a JMAP-only keyword spelling stands for.
pub fn jmap_spelling_of(name: &str) -> Option<&'static str> {
    JMAP_SPELLINGS
        .iter()
        .find(|(jmap, _)| jmap.eq_ignore_ascii_case(name))
        .map(|(_, imap)| *imap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_transcribed_once_and_consistently() {
        assert_eq!(REGISTERED.len(), 25);
        assert_eq!(JMAP_SPELLINGS.len(), 5);
        for name in REGISTERED {
            assert!(name.starts_with('$'), "{} lacks the $ prefix", name);
            assert!(
                jmap_spelling_of(name).is_none(),
                "{} is in both tables",
                name
            );
        }
        // No duplicates, under any casing.
        for (i, a) in REGISTERED.iter().enumerate() {
            for b in &REGISTERED[i + 1..] {
                assert!(!a.eq_ignore_ascii_case(b), "{} and {} collide", a, b);
            }
        }
    }

    #[test]
    fn the_two_tables_do_not_overlap() {
        for (name, _) in WELL_KNOWN {
            assert!(
                canonical_in(REGISTERED, name).is_none(),
                "{} is registered; it does not belong in the convention table",
                name
            );
            assert!(jmap_spelling_of(name).is_none(), "{} is a JMAP spelling", name);
        }
        for (i, (a, _)) in WELL_KNOWN.iter().enumerate() {
            for (b, _) in &WELL_KNOWN[i + 1..] {
                assert!(!a.eq_ignore_ascii_case(b), "{} and {} collide", a, b);
            }
        }
    }

    fn canonical_in(table: &[&'static str], name: &str) -> Option<&'static str> {
        table.iter().find(|k| k.eq_ignore_ascii_case(name)).copied()
    }

    #[test]
    fn well_known_keywords_are_spelled_and_explained() {
        assert_eq!(canonical("$LABEL1"), Some("$label1"));
        assert_eq!(canonical("junk"), Some("Junk"));
        assert!(meaning("$label1").unwrap().contains("Important"));
        assert!(meaning("$Label1").is_some(), "case-insensitive");
        assert!(meaning("Junk").unwrap().contains("$Junk"));
        // A registered keyword is not a convention, and vice versa.
        assert_eq!(meaning("$Important"), None);
        assert_eq!(meaning("invoice"), None);
    }

    #[test]
    fn canonical_spelling_is_case_insensitive() {
        assert_eq!(canonical("$important"), Some("$Important"));
        assert_eq!(canonical("$IMPORTANT"), Some("$Important"));
        assert_eq!(canonical("$Important"), Some("$Important"));
        assert_eq!(canonical("$mdnsent"), Some("$MDNSent"));
        assert_eq!(canonical("$mailflagbit1"), Some("$MailFlagBit1"));
        // An unregistered keyword is nobody's business but the server's.
        assert_eq!(canonical("invoice"), None);
        assert_eq!(canonical("$madeup"), None);
    }

    #[test]
    fn jmap_spellings_point_at_the_system_flag() {
        assert_eq!(jmap_spelling_of("$seen"), Some("\\Seen"));
        assert_eq!(jmap_spelling_of("$Seen"), Some("\\Seen"));
        assert_eq!(jmap_spelling_of("$recent"), Some("\\Recent"));
        assert_eq!(jmap_spelling_of("$junk"), None);
    }
}
