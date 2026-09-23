//! Sort criteria for `search` / `unread` (`-S` / `--sort`).
//!
//! The spec is a comma-separated list of criteria, the first one being
//! primary; a `-` prefix reverses (descends) that criterion, mirroring
//! RFC 5256's `REVERSE` sort key — e.g. `-S "-date,uid"`.
//!
//! On a server that advertises `SORT` (RFC 5256) the criteria are passed
//! straight to `UID SORT`; otherwise they are applied client-side to the
//! fetched page (see [`sort_results`]).
//!
//! `date` and `arrival` are different keys on both paths: `date` is the
//! message's own `Date:` header (`SearchResult::sent`), `arrival` the
//! mailbox's internaldate (`SearchResult::date`). Both arrive with the
//! metadata already fetched — the sent date is ENVELOPE's first field —
//! so telling them apart costs no extra round trip.

use crate::imap::SearchResult;
use anyhow::{bail, Result};

/// One sortable field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Message UID (client-side only: RFC 5256 has no UID sort criterion).
    Uid,
    /// Sent date (IMAP `DATE`): the message's own `Date:` header.
    Date,
    /// Internal/arrival date-time (IMAP `ARRIVAL`): when the mailbox
    /// received it. Not the same quantity as [`SortKey::Date`], and
    /// client-side it used to be ordered by the same field.
    Arrival,
    /// Message size in octets.
    Size,
    /// Base subject text.
    Subject,
    /// First `From` address.
    From,
    /// First `To` address (server-side `SORT` only).
    To,
    /// First `Cc` address (server-side `SORT` only).
    Cc,
}

/// A parsed `--sort` spec: criteria in priority order, each with its
/// direction (`true` = descending).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortCriteria {
    pub keys: Vec<(SortKey, bool)>,
}

impl SortCriteria {
    /// True when every criterion can be handed to the server-side
    /// `UID SORT` command (i.e. none is `Uid`).
    pub fn server_sortable(&self) -> bool {
        self.keys.iter().all(|(k, _)| *k != SortKey::Uid)
    }
}

/// Parse a `--sort` spec, e.g. `"-date"` or `"subject,-size,uid"`.
pub fn parse_sort(spec: &str) -> Result<SortCriteria> {
    let mut keys = Vec::new();
    for token in spec.split(',') {
        let token = token.trim();
        if token.is_empty() {
            bail!("empty criterion in sort spec '{}'", spec);
        }
        let (name, reverse) = match token.strip_prefix('-') {
            Some(rest) if !rest.is_empty() => (rest, true),
            Some(_) => bail!("bare '-' in sort spec '{}'", spec),
            None => (token, false),
        };
        let key = match name.to_ascii_lowercase().as_str() {
            "uid" => SortKey::Uid,
            "date" => SortKey::Date,
            "arrival" => SortKey::Arrival,
            "size" => SortKey::Size,
            "subject" => SortKey::Subject,
            "from" => SortKey::From,
            "to" => SortKey::To,
            "cc" => SortKey::Cc,
            other => bail!(
                "unknown sort criterion '{}' (valid: uid, date, arrival, size, subject, from, to, cc; prefix '-' to reverse)",
                other
            ),
        };
        keys.push((key, reverse));
    }
    if keys.is_empty() {
        bail!("empty sort spec");
    }
    Ok(SortCriteria { keys })
}

/// Client-side fallback: order already-fetched results by `spec`,
/// first criterion primary. `To`/`Cc` cannot be ordered from the fetched
/// metadata and are left as-is.
pub fn sort_results(results: &mut [SearchResult], spec: &SortCriteria) {
    for (key, reverse) in spec.keys.iter().rev() {
        results.sort_by(|a, b| {
            let ord = compare(a, b, *key);
            if *reverse {
                ord.reverse()
            } else {
                ord
            }
        });
    }
}

/// The instant a `SearchResult`'s internaldate string names, for
/// ordering.
///
/// `None` sorts before everything, which is where a message with no
/// internaldate already sat when this was a string comparison. A date
/// that will not parse falls back to the same place rather than to a
/// wrong instant: refusing to guess is the only honest option, and it
/// is at worst the behaviour this had before.
fn instant(date: &Option<String>) -> Option<i64> {
    let text = date.as_deref()?;
    chrono::DateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S %z")
        .ok()
        .map(|d| d.timestamp())
}

/// The instant a message's own `Date:` header names, for ordering.
///
/// The header is carried verbatim, so it is in RFC 5322 form rather
/// than the internaldate's. Same rule as [`instant`] for what cannot
/// be read: `None`, which sorts first.
///
/// It deliberately does *not* fall back to the internaldate. Filling a
/// missing sent date with an arrival date is precisely the confusion
/// this key exists to end — it would put a message in an order neither
/// quantity justifies, and silently, which is worse than admitting the
/// message has no sent date to sort by.
fn sent_instant(sent: &Option<String>) -> Option<i64> {
    let text = sent.as_deref()?;
    chrono::DateTime::parse_from_rfc2822(text)
        .ok()
        .map(|d| d.timestamp())
}

fn compare(a: &SearchResult, b: &SearchResult, key: SortKey) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match key {
        SortKey::Uid => a.uid.cmp(&b.uid),
        // Two different quantities, two different fields. One arm
        // covered both until now, ordering `date` by the internaldate
        // -- so on any server without SORT, where the two are distinct
        // keys on the wire, `--sort date` silently gave arrival order.
        //
        // Both are compared as instants, not as the strings they are
        // printed as, because each carries its own UTC offset: across
        // a DST change or between two senders' zones, "01:30 -0400"
        // sorts after "01:15 -0500" although it happened 45 minutes
        // EARLIER. Silent, and invisible to the reader, who sees only
        // a list in the wrong order. (The mock stamps every message
        // "+0000", which is exactly why neither the offline suite nor
        // the wire tests could catch that one.)
        SortKey::Date => sent_instant(&a.sent).cmp(&sent_instant(&b.sent)),
        SortKey::Arrival => instant(&a.date).cmp(&instant(&b.date)),
        SortKey::Size => a.size.cmp(&b.size),
        SortKey::Subject => a.subject.to_lowercase().cmp(&b.subject.to_lowercase()),
        SortKey::From => a.from.to_lowercase().cmp(&b.from.to_lowercase()),
        // Not carried in SearchResult metadata: stable no-op.
        SortKey::To | SortKey::Cc => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sort_keys_and_reverse_prefix() {
        let spec = parse_sort("subject,-size, uid").expect("parse");
        assert_eq!(
            spec.keys,
            vec![
                (SortKey::Subject, false),
                (SortKey::Size, true),
                (SortKey::Uid, false),
            ]
        );
        assert!(!spec.server_sortable());
        assert!(parse_sort("-date").expect("parse").server_sortable());
        assert_eq!(parse_sort("-date").expect("parse").keys, vec![(SortKey::Date, true)]);
    }

    #[test]
    fn parse_sort_rejects_bad_input() {
        assert!(parse_sort("bogus").is_err());
        assert!(parse_sort("").is_err());
        assert!(parse_sort("-").is_err());
        assert!(parse_sort("date,").is_err());
    }

    fn result(uid: u32, subject: &str, from: &str, size: u32) -> SearchResult {
        SearchResult {
            uid,
            folder: "INBOX".to_string(),
            subject: subject.to_string(),
            from: from.to_string(),
            date: Some(format!("2026-09-2{} 12:00:00 +0000", (uid % 9) + 1)),
            sent: None,
            size: Some(size),
            flags: Vec::new(),
            parts: 1,
        }
    }

    fn uids(results: &[SearchResult]) -> Vec<u32> {
        results.iter().map(|r| r.uid).collect()
    }

    #[test]
    fn sort_results_by_single_key_both_directions() {
        let base = || vec![result(1, "beta", "zoe@x", 30), result(2, "alpha", "amy@x", 10), result(3, "gamma", "bob@x", 20)];
        let mut asc = base();
        sort_results(&mut asc, &parse_sort("subject").unwrap());
        assert_eq!(uids(&asc), vec![2, 1, 3]);
        let mut desc = base();
        sort_results(&mut desc, &parse_sort("-subject").unwrap());
        assert_eq!(uids(&desc), vec![3, 1, 2]);

        let mut by_size = base();
        sort_results(&mut by_size, &parse_sort("-size").unwrap());
        assert_eq!(uids(&by_size), vec![1, 3, 2]);
    }

    #[test]
    fn sort_results_first_key_is_primary() {
        // Same subject on two messages: the second key breaks the tie.
        let mut results = vec![
            result(5, "same", "amy@x", 10),
            result(2, "same", "amy@x", 10),
            result(9, "other", "amy@x", 10),
        ];
        sort_results(&mut results, &parse_sort("subject,-uid").unwrap());
        assert_eq!(uids(&results), vec![9, 5, 2]);
    }

    #[test]
    fn sort_results_case_insensitive_subject() {
        let mut results = vec![result(1, "Zebra", "a@x", 1), result(2, "apple", "a@x", 1)];
        sort_results(&mut results, &parse_sort("subject").unwrap());
        assert_eq!(uids(&results), vec![2, 1]);
    }

    #[test]
    fn arrivals_are_ordered_as_instants_not_as_printed_strings() {
        // The "fall back" hour, which every long-lived mailbox has:
        // 01:30 -0400 is 05:30 UTC, and 01:15 -0500 is 06:15 UTC --
        // so the SECOND one happened later, though its printed string
        // sorts first. A string comparison got this exactly backwards.
        let mut rs = vec![
            result_dated(2, "2026-11-01 01:15:00 -0500", None), // 06:15 UTC, later
            result_dated(1, "2026-11-01 01:30:00 -0400", None), // 05:30 UTC, earlier
        ];
        let spec = parse_sort("arrival").expect("parse");
        sort_results(&mut rs, &spec);
        assert_eq!(
            rs.iter().map(|r| r.uid).collect::<Vec<_>>(),
            vec![1, 2],
            "ascending by arrival must put the earlier INSTANT first"
        );
    }

    #[test]
    fn sent_dates_are_ordered_as_instants_too() {
        // The same trap in the sent date's own format, which carries
        // its offset the RFC 5322 way.
        let mut rs = vec![
            result_dated(2, ARRIVED, Some("Sun, 1 Nov 2026 01:15:00 -0500")),
            result_dated(1, ARRIVED, Some("Sun, 1 Nov 2026 01:30:00 -0400")),
        ];
        sort_results(&mut rs, &parse_sort("date").expect("parse"));
        assert_eq!(
            rs.iter().map(|r| r.uid).collect::<Vec<_>>(),
            vec![1, 2],
            "ascending by date must put the earlier INSTANT first"
        );
    }

    #[test]
    fn date_reads_the_sent_header_and_arrival_the_internaldate() {
        // The defect this pair exists for: one arm served both keys,
        // so `date` ordered by the internaldate and the two criteria
        // were the same ordering on every server without SORT. Here
        // they are deliberately opposite, so a fallback from one to the
        // other cannot pass as the other.
        let mut rs = vec![
            // Arrived first, written last.
            result_dated(1, "2026-09-20 09:00:00 +0000", Some("Fri, 18 Sep 2026 12:00:00 +0000")),
            // Arrived last, written first.
            result_dated(2, "2026-09-20 10:00:00 +0000", Some("Thu, 17 Sep 2026 12:00:00 +0000")),
        ];
        sort_results(&mut rs, &parse_sort("arrival").expect("parse"));
        assert_eq!(uids(&rs), vec![1, 2], "arrival is the internaldate");
        sort_results(&mut rs, &parse_sort("date").expect("parse"));
        assert_eq!(uids(&rs), vec![2, 1], "date is the message's own Date:");
        // And reversing says the same thing the other way round, so a
        // key that silently ignored its field could not hide in a tie.
        sort_results(&mut rs, &parse_sort("-date").expect("parse"));
        assert_eq!(uids(&rs), vec![1, 2]);
    }

    #[test]
    fn a_sent_date_that_cannot_be_read_sorts_first_and_borrows_nothing() {
        // No Date: header, and an unreadable one, both land where a
        // missing internaldate already sat -- rather than falling back
        // to the arrival date, which would put the message in an order
        // neither quantity justifies. UID 3 arrived EARLIEST, so an
        // arrival fallback would sort it first among the readable ones
        // and this test would see 3 in the middle.
        let mut rs = vec![
            result_dated(1, "2026-09-20 12:00:00 +0000", Some("Fri, 18 Sep 2026 12:00:00 +0000")),
            result_dated(2, "2026-09-20 13:00:00 +0000", Some("not a date at all")),
            result_dated(3, "2026-09-20 08:00:00 +0000", None),
        ];
        sort_results(&mut rs, &parse_sort("date").expect("parse"));
        let order = uids(&rs);
        assert_eq!(order.len(), 3);
        assert_eq!(order[2], 1, "the only readable sent date sorts last");
        assert!(order[..2].contains(&2) && order[..2].contains(&3));
    }

    #[test]
    fn a_date_header_with_a_trailing_zone_comment_still_parses() {
        // RFC 5322's obsolete form, still emitted: "-0700 (PDT)".
        // If this stopped parsing, every such message would quietly
        // join the unreadable pile at the front of a date sort.
        let mut rs = vec![
            result_dated(2, ARRIVED, Some("Wed, 17 Jul 1996 02:23:25 -0700 (PDT)")),
            result_dated(1, ARRIVED, Some("Wed, 17 Jul 1996 01:23:25 -0700 (PDT)")),
        ];
        sort_results(&mut rs, &parse_sort("date").expect("parse"));
        assert_eq!(uids(&rs), vec![1, 2]);
    }

    /// One arrival date, for tests about the other one.
    const ARRIVED: &str = "2026-09-20 12:00:00 +0000";

    fn result_dated(uid: u32, date: &str, sent: Option<&str>) -> SearchResult {
        SearchResult {
            uid,
            folder: "INBOX".to_string(),
            subject: String::new(),
            from: String::new(),
            date: Some(date.to_string()),
            sent: sent.map(str::to_string),
            size: None,
            flags: Vec::new(),
            parts: 0,
        }
    }
}
