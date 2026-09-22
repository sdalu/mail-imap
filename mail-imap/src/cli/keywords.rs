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
/// spelling, each with a one-line description.
///
/// The registry itself carries no description — its columns are
/// Keyword, Type, Usage, Scope, Comments (empty throughout) and
/// Reference — so each line below is taken from the RFC that registers
/// the keyword, quoted or trimmed to fit. The three with a person
/// rather than an RFC as their reference have no normative text at
/// all; theirs say what clients use them for.
const REGISTERED: &[(&str, &str)] = &[
    // RFC 3503: set for every message that required an automatic MDN,
    // whether or not it was sent, and never unset once set.
    ("$MDNSent", "a return receipt (MDN) was sent for this message"),
    // RFC 5550 quotes verbatim where they define the keyword.
    ("$Forwarded", "the message was forwarded to another address"),
    ("$SubmitPending", "the message is awaiting submission"),
    ("$Submitted", "the message has been submitted for delivery"),
    // Registered by Alexey Melnikov and Rob Mueller; no defining RFC.
    ("$Junk", "the user considers this message junk"),
    ("$NotJunk", "the user considers this message not junk"),
    ("$Phishing", "the message is believed to be a phishing attempt"),
    // RFC 8457.
    ("$Important", "the user considers this message important"),
    // RFC 9979, quoted.
    ("$autosent", "generated and sent by the system for the user"),
    ("$canunsubscribe", "carries a valid RFC 8058 List-Unsubscribe header"),
    ("$followed", "the user wants future messages in this thread"),
    ("$hasattachment", "the message has one or more attachments"),
    ("$hasmemo", "a $memo for it exists in the same thread"),
    ("$hasnoattachment", "the message explicitly has no attachments"),
    ("$imported", "imported from another system, not delivered"),
    ("$istrusted", "the server verified the sender's identity"),
    ("$MailFlagBit0", "one bit of a three-bit flag colour"),
    ("$MailFlagBit1", "one bit of a three-bit flag colour"),
    ("$MailFlagBit2", "one bit of a three-bit flag colour"),
    ("$maskedemail", "received through a masked email address"),
    ("$memo", "a note-to-self about another message in the thread"),
    ("$muted", "the user wants no more of this conversation"),
    ("$new", "to be made prominent after a recent system action"),
    ("$notify", "the client should raise a notification for it"),
    ("$unsubscribed", "the user tried to leave this mailing list"),
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

/// The spellings of "this is junk", and of "this is not".
///
/// One meaning, five names, because nobody agreed: the registry has
/// `$Junk`/`$NotJunk`, Thunderbird writes `Junk`/`NonJunk` (*Non*,
/// where Apple writes *Not*), and Apple Mail writes both its own and
/// the registry's. A message can therefore end up carrying a junk and
/// a not-junk keyword at once, which is what `JunkState::Contradictory`
/// reports.
pub const JUNK: &[&str] = &["$Junk", "Junk"];
pub const NOT_JUNK: &[&str] = &["$NotJunk", "NotJunk", "NonJunk"];

/// What a message's keywords say about junk, across all five spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JunkState {
    Junk,
    NotJunk,
    /// Both at once — no rule says which wins, so neither does this.
    Contradictory,
    /// Neither family is present.
    Unsaid,
}

/// Read the junk family off a message's flags.
pub fn junk_state(flags: &[String]) -> JunkState {
    let has = |family: &[&str]| {
        flags
            .iter()
            .any(|f| family.iter().any(|k| k.eq_ignore_ascii_case(f)))
    };
    match (has(JUNK), has(NOT_JUNK)) {
        (true, true) => JunkState::Contradictory,
        (true, false) => JunkState::Junk,
        (false, true) => JunkState::NotJunk,
        (false, false) => JunkState::Unsaid,
    }
}

/// Every registered keyword usable in IMAP, in registry order, with
/// what it means.
pub fn registered() -> &'static [(&'static str, &'static str)] {
    REGISTERED
}

/// Every well-known but unregistered keyword, with what it means.
pub fn well_known() -> &'static [(&'static str, &'static str)] {
    WELL_KNOWN
}

/// Is this a name a reader cannot get anything out of?
///
/// `$label3` and `$MailFlagBit1` are identifiers: the number carries
/// the meaning and only the client that wrote it knows the mapping.
/// Every other registered name is English — `$hasattachment` says what
/// it is — and glossing those in a listing would be paraphrasing the
/// word next to it.
fn is_opaque(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("$label") || lower.starts_with("$mailflagbit")
}

/// What to put beside a name in a *listing*: only what the name itself
/// does not say. `tag known` is the reference and describes everything;
/// a listing is not the place to restate a word in other words.
pub fn listing_meaning(name: &str) -> Option<&'static str> {
    is_opaque(name).then(|| meaning(name)).flatten()
}

/// What a keyword means, when either table knows it.
pub fn meaning(name: &str) -> Option<&'static str> {
    WELL_KNOWN
        .iter()
        .chain(REGISTERED.iter())
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
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(k, _)| *k)
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
        for (_, desc) in REGISTERED {
            assert!(!desc.is_empty(), "every registered keyword is described");
            assert!(desc.len() < 55, "a description is one short line: {}", desc);
        }
        assert_eq!(JMAP_SPELLINGS.len(), 5);
        for (name, _) in REGISTERED {
            assert!(name.starts_with('$'), "{} lacks the $ prefix", name);
            assert!(
                jmap_spelling_of(name).is_none(),
                "{} is in both tables",
                name
            );
        }
        // No duplicates, under any casing.
        for (i, (a, _)) in REGISTERED.iter().enumerate() {
            for (b, _) in &REGISTERED[i + 1..] {
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

    fn canonical_in(
        table: &[(&'static str, &'static str)],
        name: &str,
    ) -> Option<&'static str> {
        table
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(k, _)| *k)
    }

    #[test]
    fn a_listing_explains_only_what_a_name_does_not_say() {
        // Identifiers need the gloss: the number is the meaning.
        assert!(listing_meaning("$label1").unwrap().contains("Important"));
        assert!(listing_meaning("$MailFlagBit0").is_some());
        // These say what they are; repeating it in other words is noise.
        for plain in ["NonJunk", "Junk", "$Junk", "$NotJunk", "$hasattachment",
                      "$Important", "$muted", "invoice"] {
            assert_eq!(listing_meaning(plain), None, "{} explains itself", plain);
        }
        // `tag known` still describes them all.
        assert!(meaning("$hasattachment").is_some());
        assert!(meaning("NonJunk").is_some());
    }

    #[test]
    fn well_known_keywords_are_spelled_and_explained() {
        assert_eq!(canonical("$LABEL1"), Some("$label1"));
        assert_eq!(canonical("junk"), Some("Junk"));
        assert!(meaning("$label1").unwrap().contains("Important"));
        assert!(meaning("$Label1").is_some(), "case-insensitive");
        assert!(meaning("Junk").unwrap().contains("$Junk"));
        // The registry is described too, now.
        assert_eq!(
            meaning("$Important"),
            Some("the user considers this message important")
        );
        assert_eq!(meaning("$important"), meaning("$Important"), "case-free");
        assert_eq!(meaning("invoice"), None, "an unregistered keyword is nobody's");
    }

    #[test]
    fn the_junk_family_is_read_across_every_spelling() {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(junk_state(&f(&["$Junk"])), JunkState::Junk);
        assert_eq!(junk_state(&f(&["Junk"])), JunkState::Junk);
        assert_eq!(junk_state(&f(&["$NotJunk"])), JunkState::NotJunk);
        assert_eq!(junk_state(&f(&["NotJunk"])), JunkState::NotJunk);
        assert_eq!(junk_state(&f(&["NonJunk"])), JunkState::NotJunk);
        assert_eq!(junk_state(&f(&["nonjunk"])), JunkState::NotJunk, "case-free");
        // The documented real-world mess: Thunderbird sets Junk without
        // clearing what Apple Mail left behind.
        assert_eq!(
            junk_state(&f(&["\\Seen", "Junk", "$NotJunk", "NotJunk"])),
            JunkState::Contradictory
        );
        assert_eq!(junk_state(&f(&["\\Seen", "invoice"])), JunkState::Unsaid);
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
