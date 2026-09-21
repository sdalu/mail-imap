//! Sort criteria for `search` / `unread` (`-S` / `--sort`).
//!
//! The spec is a comma-separated list of criteria, the first one being
//! primary; a `-` prefix reverses (descends) that criterion, mirroring
//! RFC 5256's `REVERSE` sort key — e.g. `-S "-date,uid"`.
//!
//! On a server that advertises `SORT` (RFC 5256) the criteria are passed
//! straight to `UID SORT`; otherwise they are applied client-side to the
//! fetched page (see [`sort_results`]).

use crate::imap::SearchResult;
use anyhow::{bail, Result};

/// One sortable field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Message UID (client-side only: RFC 5256 has no UID sort criterion).
    Uid,
    /// Sent date (IMAP `DATE`).
    Date,
    /// Internal/arrival date-time (IMAP `ARRIVAL`).
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

fn compare(a: &SearchResult, b: &SearchResult, key: SortKey) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match key {
        SortKey::Uid => a.uid.cmp(&b.uid),
        SortKey::Date | SortKey::Arrival => a.date.cmp(&b.date),
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
}
