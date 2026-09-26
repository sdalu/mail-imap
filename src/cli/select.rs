//! Message selections and folder patterns — the two things the CLI
//! takes as parameters.
//!
//! A **selection** names messages: `[FOLDER::]UIDSPEC`. It is the single
//! positional argument of every command that works on messages (`read`,
//! `thread`, `part`, `flag`, `tag`), so one invocation can reach
//! messages in several folders:
//!
//! ```text
//! 12345            the default folder (-f, or "folder" from the config)
//! 1,4,7            a list
//! 1-9              a UID range
//! 3,9-12           a mix
//! *                every message of the folder (and '*' is only this)
//! last:20          the 20 newest messages (a count, not an interval)
//! first:5          the 5 oldest
//! 9-               from UID 9 to the end of the mailbox
//! Archive::1-5     a folder-qualified selection ('::' binds the folder,
//!                  so a single ':' is free to introduce a count)
//! ```
//!
//! A **folder pattern** names mailboxes: a literal name, or an IMAP
//! `LIST` pattern (`*` crosses the hierarchy delimiter, `%` does not).
//! `-f 'Archive/*'` and `-A` (which is `-f '*'`) go through here.

use anyhow::{bail, Result};

/// One element of a UID spec. Ranges and `*` stay symbolic until they
/// are resolved against the folder's actual UID list, because UIDs are
/// sparse: `1-9` is an interval of the UID space, not nine messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UidItem {
    /// A single UID, named explicitly.
    One(u32),
    /// An inclusive interval, endpoints normalized so low <= high.
    Range(u32, u32),
    /// `n-`: from `n` to the highest UID in the folder.
    From(u32),
    /// `*`: every message in the folder.
    All,
    /// `last:N`: the N newest messages, i.e. the N highest UIDs.
    /// A count, not an interval — the thing no UID range can express.
    Last(u32),
    /// `first:N`: the N oldest messages, i.e. the N lowest UIDs.
    First(u32),
}

/// One parsed selection token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The folder named by the token, or `None` for the default folder.
    pub folder: Option<String>,
    pub items: Vec<UidItem>,
    /// The token as the user wrote it, for error messages.
    pub source: String,
}

impl Selection {
    /// Whether resolving this selection needs the folder's UID list
    /// (true as soon as it holds a range, a count or `*`).
    pub fn needs_uid_list(&self) -> bool {
        !self.items.iter().all(|i| matches!(i, UidItem::One(_)))
    }

    /// Whether every item is a count (`last:20`, `first:5`).
    ///
    /// A count names no UID, so it is not ambiguous across folders the
    /// way a bare UID is: it means N *per folder*, and an unqualified
    /// one applies to each folder `-f`/`-A` selected.
    pub fn is_count_only(&self) -> bool {
        !self.items.is_empty()
            && self
                .items
                .iter()
                .all(|i| matches!(i, UidItem::Last(_) | UidItem::First(_)))
    }

    /// [`Selection::resolve_in`] with only the folder the token names,
    /// which is all a test resolving a bare token has.
    #[cfg(test)]
    pub fn resolve(&self, available: Option<&[u32]>) -> Result<Vec<u32>> {
        self.resolve_in(self.folder.as_deref(), available)
    }

    /// Resolve to concrete UIDs.
    ///
    /// `available` is the folder's UID list, required when
    /// [`Selection::needs_uid_list`] is true. Explicitly named UIDs are
    /// kept whether or not they appear in `available` (an unknown UID
    /// is then reported by the server, which says so better than we
    /// could); ranges and `*` match only messages that exist. `folder`
    /// is where `available` came from -- which a selection resolved
    /// against the default folder does not carry itself -- so a refusal
    /// can say where it looked.
    pub fn resolve_in(&self, folder: Option<&str>, available: Option<&[u32]>) -> Result<Vec<u32>> {
        let mut out: Vec<u32> = Vec::new();
        // The order UIDs were named in is part of the contract, so the
        // result stays a Vec; the set is only there to answer "seen
        // already" in constant time. It used to be a `Vec::contains`
        // scan of everything collected so far, which is quadratic --
        // and `*` on a large archive folder pushes every UID the
        // mailbox has, so `read '*'` or `move '*'` on a few hundred
        // thousand messages spent minutes deciding what it had already
        // seen. Nothing caps this path: `--max` is a search budget and
        // never reaches here.
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let push = |uid: u32, out: &mut Vec<u32>, seen: &mut std::collections::HashSet<u32>| {
            if seen.insert(uid) {
                out.push(uid);
            }
        };
        for item in &self.items {
            match item {
                UidItem::One(uid) => push(*uid, &mut out, &mut seen),
                _ => {
                    let all = match available {
                        Some(a) => a,
                        None => bail!(
                            "selection '{}' needs the folder's UID list to be resolved",
                            self.source
                        ),
                    };
                    let mut matched: Vec<u32> = match item {
                        UidItem::Range(low, high) => {
                            all.iter().copied().filter(|u| u >= low && u <= high).collect()
                        }
                        UidItem::From(low) => {
                            all.iter().copied().filter(|u| u >= low).collect()
                        }
                        UidItem::All => all.to_vec(),
                        // UIDs ascend with arrival, so the N highest are
                        // the N most recently delivered. Nothing is
                        // sorted by Date: here — that is what -S does.
                        UidItem::Last(n) => {
                            let mut sorted = all.to_vec();
                            sorted.sort_unstable();
                            sorted.split_off(sorted.len().saturating_sub(*n as usize))
                        }
                        UidItem::First(n) => {
                            let mut sorted = all.to_vec();
                            sorted.sort_unstable();
                            sorted.truncate(*n as usize);
                            sorted
                        }
                        UidItem::One(_) => unreachable!(),
                    };
                    if matched.is_empty() {
                        // Doing silently less than asked is the worst
                        // outcome on a mutating command, so every item
                        // has to match something.
                        bail!("{}", self.no_match(item, folder));
                    }
                    matched.sort_unstable();
                    for uid in matched {
                        push(uid, &mut out, &mut seen);
                    }
                }
            }
        }
        Ok(out)
    }

    /// The refusal for `item` matching nothing in `folder`. The term is
    /// named once: the whole selection when the item is all of it, the
    /// item and then its selection when it is only part (`'4-7' of
    /// selection '1,4-7'`). The folder is added unless the selection
    /// already spells it (`Archive::99-`), which is when a reader could
    /// not otherwise tell -- a count spread over `-f`, a default folder.
    fn no_match(&self, item: &UidItem, folder: Option<&str>) -> String {
        let term = if self.items.len() == 1 {
            format!("selection '{}'", self.source)
        } else {
            format!("'{}' of selection '{}'", show_item(item), self.source)
        };
        match folder {
            Some(folder) if !self.source.contains("::") => {
                format!("{} matched no message in '{}'", term, folder)
            }
            _ => format!("{} matched no message", term),
        }
    }
}

/// One item as the user would have written it, for error messages.
fn show_item(item: &UidItem) -> String {
    match item {
        UidItem::One(uid) => uid.to_string(),
        UidItem::Range(low, high) => format!("{}-{}", low, high),
        UidItem::From(low) => format!("{}-", low),
        UidItem::All => "*".to_string(),
        UidItem::Last(n) => format!("last:{}", n),
        UidItem::First(n) => format!("first:{}", n),
    }
}

/// Parse a UID spec: a comma-separated list of UIDs, ranges (`4-7`),
/// ranges running to the end of the mailbox (`9-`), counts (`last:20`,
/// `first:5`) and `*` for every message.
fn parse_uid_items(spec: &str) -> Result<Vec<UidItem>> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("empty UID spec");
    }
    let mut items = Vec::new();
    for token in spec.split(',') {
        let token = token.trim();
        if token.is_empty() {
            bail!("empty element in UID spec '{}'", spec);
        }
        items.push(parse_uid_item(token, spec)?);
    }
    Ok(items)
}

fn parse_uid_item(token: &str, spec: &str) -> Result<UidItem> {
    // '*' is every message, and that is all it is: it is not a range
    // endpoint. "From 9 to the end" is '9-'.
    if token == "*" {
        return Ok(UidItem::All);
    }
    if token.contains('*') {
        bail!(
            "invalid item '{}' in '{}': '*' means every message and stands alone; \
             for a range running to the end of the mailbox write 9-",
            token,
            spec
        );
    }
    // A count, not an interval. ':' is this and nothing else — it is
    // not IMAP's range operator here, and '::' binds the folder.
    if let Some((word, n)) = token.split_once(':') {
        if word.eq_ignore_ascii_case("last") || word.eq_ignore_ascii_case("first") {
            let count: u32 = n.parse().map_err(|_| {
                anyhow::anyhow!("'{}' needs a count, as in {}:20 (in '{}')", word, word, spec)
            })?;
            if count == 0 {
                bail!("'{}' asks for no messages (in '{}')", token, spec);
            }
            return Ok(if word.eq_ignore_ascii_case("last") {
                UidItem::Last(count)
            } else {
                UidItem::First(count)
            });
        }
        bail!(
            "invalid item '{}' in '{}': ':' introduces a count (last:20, first:5); \
             a folder is bound with '::' and a range written with '-'",
            token,
            spec
        );
    }
    let Some(sep) = token.find('-') else {
        return Ok(UidItem::One(parse_uid(token, spec)?));
    };
    let (low, high) = (&token[..sep], &token[sep + 1..]);
    if high.contains('-') {
        bail!(
            "invalid range '{}' in UID spec '{}': a range is LOW-HIGH (4-7) or LOW- \
             (4 to the end)",
            token,
            spec
        );
    }
    match (low, high) {
        // A missing lower end would have to be written '-20', which the
        // argument parser reads as an option anyway. For "the first N
        // messages" there is first:N.
        ("", _) => bail!(
            "invalid range '{}' in UID spec '{}': a range needs a lower end \
             (write 1-{}, or first:{} for a count)",
            token,
            spec,
            high,
            high
        ),
        // '9-' is 9 to the end of the mailbox.
        (n, "") => Ok(UidItem::From(parse_uid(n, spec)?)),
        (a, b) => {
            let (a, b) = (parse_uid(a, spec)?, parse_uid(b, spec)?);
            Ok(UidItem::Range(a.min(b), a.max(b)))
        }
    }
}

fn parse_uid(token: &str, spec: &str) -> Result<u32> {
    let uid: u32 = token
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid UID '{}' in selection '{}'", token, spec))?;
    if uid == 0 {
        bail!("invalid UID '0' in selection '{}': UIDs start at 1", spec);
    }
    Ok(uid)
}

/// Parse one selection token, `[FOLDER::]UIDSPEC`.
pub fn parse_selection(token: &str) -> Result<Selection> {
    let token = token.trim();
    if token.is_empty() {
        bail!("empty message selection");
    }
    let source = token.to_string();

    // '::' binds a folder to a UID spec, and it is the only thing that
    // does. A single ':' therefore belongs entirely to the UID spec,
    // where it introduces a count — so 'last:3' is the 3 newest,
    // '2026::5' is UID 5 of a folder named 2026, and neither has to be
    // guessed at. Split at the LAST '::', so a folder whose own name
    // contains '::' is still writable.
    let Some(sep) = token.rfind("::") else {
        if let Ok(items) = parse_uid_items(token) {
            return Ok(Selection {
                folder: None,
                items,
                source,
            });
        }
        return Ok(Selection {
            folder: None,
            items: describe_uid_error(parse_uid_items(token))?,
            source,
        });
    };
    let (left, right) = (&token[..sep], &token[sep + 2..]);
    if left.is_empty() {
        bail!("empty folder name in selection '{}'", token);
    }
    if left.contains(['*', '%']) {
        bail!(
            "folder wildcards are not allowed in a message selection ('{}'); \
             name one folder, or use -f to search several",
            token
        );
    }
    Ok(Selection {
        folder: Some(canonical_folder(left)),
        items: describe_uid_error(parse_uid_items(right))?,
        source,
    })
}

/// Turn a UID-spec error into one about the whole selection, adding
/// what a selection looks like.
fn describe_uid_error(result: Result<Vec<UidItem>>) -> Result<Vec<UidItem>> {
    result.map_err(|e| {
        anyhow::anyhow!(
            "{} (a selection is [FOLDER::]UIDS, e.g. 5, 1,4,7, 1-9, 9-, '*', \
             last:20, Archive::1-5)",
            e
        )
    })
}

/// Parse every selection token of a command.
pub fn parse_selections(tokens: &[String]) -> Result<Vec<Selection>> {
    if tokens.is_empty() {
        bail!("no message selection given");
    }
    tokens.iter().map(|t| parse_selection(t)).collect()
}

/// Trim a folder argument, and fold the one mailbox name IMAP defines
/// as case-insensitive so that `-f inbox,INBOX` is one folder and not
/// two. Everything below INBOX, and every other name, is left alone:
/// case matters there and is the server's business.
pub fn canonical_folder(name: &str) -> String {
    let name = name.trim();
    if name.eq_ignore_ascii_case("INBOX") {
        "INBOX".to_string()
    } else {
        name.to_string()
    }
}

/// Does `name` match the IMAP `LIST` pattern `pattern`?
///
/// `*` matches any sequence of characters, `%` any sequence that does
/// not cross `delimiter`. Matching is case-sensitive except for the
/// special mailbox INBOX, which IMAP defines as case-insensitive.
///
/// `delimiter` is `None` when the `LIST` entry reported `NIL`, which
/// RFC 3501 defines as "this name has no hierarchy" — not as "the
/// usual separator". There is then no boundary for `%` to stop at, so
/// it matches whatever `*` would. Substituting a plausible `/` instead
/// would refuse RFC 3501's own example, the mailbox literally named
/// `extended/notes`, whose slash is part of the name.
pub fn folder_matches(pattern: &str, name: &str, delimiter: Option<char>) -> bool {
    if pattern.eq_ignore_ascii_case("INBOX") && name.eq_ignore_ascii_case("INBOX") {
        return true;
    }
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    matches_at(&p, &n, 0, 0, delimiter)
}

fn matches_at(p: &[char], n: &[char], pi: usize, ni: usize, delim: Option<char>) -> bool {
    if pi == p.len() {
        return ni == n.len();
    }
    match p[pi] {
        '*' | '%' => {
            let crosses = p[pi] == '*';
            let mut i = ni;
            loop {
                if matches_at(p, n, pi + 1, i, delim) {
                    return true;
                }
                if i == n.len() {
                    return false;
                }
                if !crosses && delim.is_some_and(|d| n[i] == d) {
                    return false;
                }
                i += 1;
            }
        }
        c => ni < n.len() && n[ni] == c && matches_at(p, n, pi + 1, ni + 1, delim),
    }
}

/// Does this folder argument need the mailbox list to be expanded?
pub fn is_pattern(folder: &str) -> bool {
    folder.contains(['*', '%'])
}

/// Expand folder patterns against the mailbox list, keeping the order
/// patterns were given in and deduplicating. Literal names are passed
/// through untouched, so a folder the `LIST` reply does not mention
/// still reaches the server (which reports it better than we could).
pub fn expand_folders(
    patterns: &[String],
    known: &[(String, Option<String>)],
) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for pattern in patterns {
        let pattern = &canonical_folder(pattern);
        if pattern.is_empty() {
            bail!("empty folder name");
        }
        if !is_pattern(pattern) {
            if !out.contains(pattern) {
                out.push(pattern.clone());
            }
            continue;
        }
        let before = out.len();
        for (name, delimiter) in known {
            // NIL (and the empty string some servers send for it) is
            // "no hierarchy", not "the usual separator": see
            // `folder_matches`.
            let delim = delimiter.as_ref().and_then(|d| d.chars().next());
            if folder_matches(pattern, name, delim) && !out.contains(name) {
                out.push(name.clone());
            }
        }
        if out.len() == before {
            bail!("no mailbox matches the folder pattern '{}'", pattern);
        }
    }
    if out.is_empty() {
        bail!("no folder given");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(token: &str) -> Selection {
        parse_selection(token).expect(token)
    }

    #[test]
    fn plain_uid_uses_the_default_folder() {
        let s = sel("5");
        assert_eq!(s.folder, None);
        assert_eq!(s.items, vec![UidItem::One(5)]);
        assert!(!s.needs_uid_list());
        assert_eq!(s.resolve(None).unwrap(), vec![5]);
    }

    #[test]
    fn comma_list_keeps_order_and_deduplicates() {
        let s = sel("3,1,3,2");
        assert_eq!(s.resolve(None).unwrap(), vec![3, 1, 2]);
    }

    #[test]
    fn ranges_use_a_hyphen_and_normalize() {
        assert_eq!(sel("4-7").items, vec![UidItem::Range(4, 7)]);
        assert_eq!(sel("7-4").items, vec![UidItem::Range(4, 7)]);
    }

    #[test]
    fn imaps_colon_range_is_not_taken() {
        // ':' is last:N / first:N here, and '::' binds the folder.
        // Accepting IMAP's own 4:7 as well would give one concept two
        // spellings and put ':' back to doing two jobs.
        let err = parse_selection("4:7").expect_err("':' is not a range operator");
        assert!(err.to_string().contains("count"), "{}", err);
        // The message names the form to use, not the one refused.
        assert!(!err.to_string().contains("4::7"), "{}", err);
        assert!(parse_selection("9:").is_err());
        assert!(parse_selection("1:*").is_err());
    }

    #[test]
    fn star_is_every_message_and_only_that() {
        assert_eq!(sel("*").items, vec![UidItem::All]);
        assert_eq!(sel("INBOX::*").items, vec![UidItem::All]);
        // Not a range endpoint, in either position or any spelling.
        for bad in ["9-*", "*-9", "*:*", "*-*", "1,*-9", "INBOX::9-*"] {
            let err = parse_selection(bad).expect_err(bad);
            assert!(
                err.to_string().contains("stands alone")
                    || err.to_string().contains("wildcards are not allowed"),
                "{}: {}",
                bad,
                err
            );
        }
    }

    #[test]
    fn a_trailing_hyphen_runs_to_the_end_of_the_mailbox() {
        assert_eq!(sel("9-").items, vec![UidItem::From(9)]);
        assert_eq!(sel("1-").items, vec![UidItem::From(1)]);
        assert_eq!(sel("3,20-").items, vec![UidItem::One(3), UidItem::From(20)]);
        assert_eq!(sel("Archive::20-").items, vec![UidItem::From(20)]);
    }

    #[test]
    fn a_range_with_no_lower_end_is_refused() {
        // '-20' would be read as an option by the argument parser, and
        // '*-20' already means the opposite ("20 to the end").
        let err = parse_selection("-20").expect_err("must be refused");
        assert!(err.to_string().contains("lower end"), "{}", err);
        assert!(parse_selection("1,-20").is_err());
    }

    #[test]
    fn mixed_spec_keeps_singles_and_matches_ranges() {
        let s = sel("3,9-12,40");
        assert!(s.needs_uid_list());
        let available = [1, 3, 9, 11, 30];
        // 3 and 40 are explicit, so both survive; the range matches only
        // the messages that exist.
        assert_eq!(s.resolve(Some(&available)).unwrap(), vec![3, 9, 11, 40]);
    }

    #[test]
    fn open_range_matches_only_existing_messages() {
        // IMAP would fold 99:* onto the last message; the tool errors
        // instead, so a mutation aimed past the end of the mailbox
        // neither hits the newest message nor passes silently.
        assert!(sel("99-").resolve(Some(&[1, 2, 3])).is_err());
        assert_eq!(sel("2-").resolve(Some(&[1, 2, 3])).unwrap(), vec![2, 3]);
        assert_eq!(sel("*").resolve(Some(&[1, 2, 3])).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn recency_counts_take_the_highest_and_lowest_uids() {
        let available = [3, 7, 8, 40, 41];
        let last = Selection {
            folder: None,
            items: vec![UidItem::Last(2)],
            source: "last:2".into(),
        };
        assert!(last.needs_uid_list());
        assert_eq!(last.resolve(Some(&available)).unwrap(), vec![40, 41]);
        let first = Selection {
            folder: None,
            items: vec![UidItem::First(2)],
            source: "first:2".into(),
        };
        assert_eq!(first.resolve(Some(&available)).unwrap(), vec![3, 7]);
    }

    #[test]
    fn a_recency_count_larger_than_the_mailbox_takes_what_is_there() {
        let sel = Selection {
            folder: None,
            items: vec![UidItem::Last(99)],
            source: "last:99".into(),
        };
        assert_eq!(sel.resolve(Some(&[3, 7])).unwrap(), vec![3, 7]);
        // ... but an empty mailbox matches nothing, which is an error
        // like any other item that matched nothing.
        assert!(sel.resolve(Some(&[])).is_err());
    }

    #[test]
    fn counts_are_spellable_because_the_single_colon_is_free() {
        assert_eq!(sel("last:20").items, vec![UidItem::Last(20)]);
        assert_eq!(sel("first:5").items, vec![UidItem::First(5)]);
        assert_eq!(sel("LAST:3").items, vec![UidItem::Last(3)]);
        assert!(sel("last:20").is_count_only());
        assert!(sel("last:3,first:3").is_count_only());
        assert!(!sel("1,last:3").is_count_only(), "a UID is in there too");
        assert!(!sel("1-5").is_count_only());
        // Folder-qualified, which is what '::' freed the colon for.
        let s = sel("Archive::last:5");
        assert_eq!(s.folder.as_deref(), Some("Archive"));
        assert_eq!(s.items, vec![UidItem::Last(5)]);
    }

    #[test]
    fn a_count_needs_a_number() {
        assert!(parse_selection("last:").is_err());
        assert!(parse_selection("last:x").is_err());
        assert!(parse_selection("last:0").is_err());
        assert!(parse_selection("~20").is_err(), "not a spelling we take");
    }

    #[test]
    fn folder_qualified_selection() {
        let s = sel("Archive::1-5");
        assert_eq!(s.folder.as_deref(), Some("Archive"));
        assert_eq!(s.items, vec![UidItem::Range(1, 5)]);
        let s = sel("Archive/2026::7");
        assert_eq!(s.folder.as_deref(), Some("Archive/2026"));
        assert_eq!(s.items, vec![UidItem::One(7)]);
        let s = sel("Sent Items::3,4");
        assert_eq!(s.folder.as_deref(), Some("Sent Items"));
    }

    #[test]
    fn the_double_colon_frees_the_single_one() {
        // Everything the single-colon binding had to refuse or guess at
        // is now plain, because ':' belongs to the UID spec alone.
        assert_eq!(sel("INBOX::1-5").folder.as_deref(), Some("INBOX"));
        assert_eq!(sel("INBOX::1-5").items, vec![UidItem::Range(1, 5)]);
        // a folder whose name is a UID spec:
        assert_eq!(sel("2026::5").folder.as_deref(), Some("2026"));
        assert_eq!(sel("2026::5").items, vec![UidItem::One(5)]);
        // ... while a single ':' is not a range at all:
        assert!(parse_selection("5:2026").is_err());
        // a folder whose name contains '::' (split at the last one):
        assert_eq!(sel("A::B::5").folder.as_deref(), Some("A::B"));
    }

    #[test]
    fn a_range_needs_a_uid_list() {
        assert!(sel("1-5").resolve(None).is_err());
    }

    #[test]
    fn an_item_matching_nothing_is_an_error_even_beside_one_that_matched() {
        // The whole point: 'flag add 5 999-*' must not quietly become
        // 'flag add 5'.
        let s = sel("5,999-");
        let err = s.resolve(Some(&[1, 5, 9])).expect_err("must not drop the item");
        assert!(err.to_string().contains("999-"), "{}", err);
    }

    #[test]
    fn inbox_folds_to_one_spelling_and_other_names_do_not() {
        assert_eq!(canonical_folder("inbox"), "INBOX");
        assert_eq!(canonical_folder(" InBoX "), "INBOX");
        assert_eq!(canonical_folder("Trash"), "Trash");
        assert_eq!(canonical_folder(" Sent Items "), "Sent Items");
        assert_eq!(canonical_folder("inbox/sub"), "inbox/sub");
        assert_eq!(sel("inbox::5").folder.as_deref(), Some("INBOX"));
        assert_eq!(sel("Sent Items ::3").folder.as_deref(), Some("Sent Items"));
    }

    #[test]
    fn folder_arguments_are_trimmed_and_inbox_deduplicated() {
        let known = vec![("INBOX".to_string(), Some("/".to_string()))];
        // '-f INBOX, Trash' splits on the comma without trimming.
        assert_eq!(
            expand_folders(&["INBOX".into(), " Trash".into()], &known).unwrap(),
            vec!["INBOX", "Trash"]
        );
        assert_eq!(
            expand_folders(&["inbox".into(), "INBOX".into()], &known).unwrap(),
            vec!["INBOX"]
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_selection("").is_err());
        assert!(parse_selection(",").is_err());
        assert!(parse_selection("1,,3").is_err());
        assert!(parse_selection("abc").is_err());
        assert!(parse_selection("-1").is_err());
        assert!(parse_selection("1.5").is_err());
        assert!(parse_selection("-5").is_err());
        assert!(parse_selection("1-2-3").is_err());
        assert!(parse_selection(":5").is_err());
        assert!(parse_selection("1:5").is_err());
    }

    #[test]
    fn rejects_uid_zero() {
        assert!(parse_selection("0").is_err());
        assert!(parse_selection("0-5").is_err());
        assert!(parse_selection("INBOX:0").is_err());
    }

    #[test]
    fn rejects_wildcards_in_a_selection_folder() {
        assert!(parse_selection("Arch*:5").is_err());
        assert!(parse_selection("Archive/%:5").is_err());
    }

    #[test]
    fn parse_selections_rejects_an_empty_list() {
        assert!(parse_selections(&[]).is_err());
    }

    #[test]
    fn a_matchless_item_is_named_in_the_refusal_as_it_was_written() {
        // The refusal exists because doing silently less than asked is
        // the worst outcome on a mutating command -- so it has to say
        // WHICH item found nothing, in the spelling that was typed, or
        // a `move 4-7,9-11 Archive` that half-matched leaves the reader
        // guessing at which half. Nothing asserted the text, and
        // mutation testing found it: replacing `show_item`'s whole body
        // with the empty string survived the suite.
        // Every selection here carries a matching item beside the
        // matchless one, so the item's own spelling is not also the
        // whole selection's -- otherwise an empty `show_item` still
        // leaves the right characters in the message, by way of the
        // source, and the assertion passes for the wrong reason. (It
        // did, on the first attempt at this test.)
        let err = |token: &str, available: &[u32]| {
            sel(token)
                .resolve(Some(available))
                .expect_err(token)
                .to_string()
        };
        // A count matches whatever exists, so it is only matchless
        // when the mailbox is empty; a range or a `from` needs only to
        // fall outside what is there.
        let cases: [(&str, &str, &[u32]); 4] = [
            ("1,4-7", "4-7", &[1]),
            ("1,99-", "99-", &[1]),
            ("1,last:2", "last:2", &[]),
            ("1,first:2", "first:2", &[]),
        ];
        for (token, item, available) in cases {
            let text = err(token, available);
            assert_eq!(
                text,
                format!("'{}' of selection '{}' matched no message", item, token),
                "the refusal must name the item, then the selection"
            );
        }
        // `*` cannot share a selection with anything (it is the whole
        // mailbox), so it is checked on its own against no messages.
        assert_eq!(err("*", &[]), "selection '*' matched no message");
    }

    #[test]
    fn a_refusal_names_the_term_once_and_the_folder_it_was_not_in() {
        // It used to say "'last:5' of selection 'last:5' matched no
        // message": the item and the selection are the same string
        // when the item is all there is, and where it looked -- the
        // thing a count spread over several `-f` folders needs -- was
        // not said at all.
        let refusal = |token: &str, folder: Option<&str>| {
            sel(token)
                .resolve_in(folder, Some(&[]))
                .expect_err(token)
                .to_string()
        };
        assert_eq!(
            refusal("last:5", Some("Archives")),
            "selection 'last:5' matched no message in 'Archives'"
        );
        assert_eq!(
            refusal("LAST:5", Some("Archives")),
            "selection 'LAST:5' matched no message in 'Archives'",
            "the selection is named as it was written"
        );
        assert_eq!(
            refusal("1,4-7", Some("INBOX")),
            "'4-7' of selection '1,4-7' matched no message in 'INBOX'"
        );
        // A selection that spells its folder is not told it again.
        assert_eq!(
            refusal("Archives::last:5", Some("Archives")),
            "selection 'Archives::last:5' matched no message"
        );
        // `resolve` knows only the folder the token carries.
        assert_eq!(
            sel("Archives::9-").resolve(Some(&[1])).unwrap_err().to_string(),
            "selection 'Archives::9-' matched no message"
        );
    }

    #[test]
    fn folder_patterns_follow_imap_wildcards() {
        assert!(folder_matches("*", "Archive/2026", Some('/')));
        assert!(folder_matches("Archive/*", "Archive/2026/Q1", Some('/')));
        assert!(folder_matches("Archive/%", "Archive/2026", Some('/')));
        assert!(!folder_matches("Archive/%", "Archive/2026/Q1", Some('/')));
        assert!(folder_matches("Arch*", "Archive", Some('/')));
        assert!(!folder_matches("Arch", "Archive", Some('/')));
        assert!(folder_matches("inbox", "INBOX", Some('/')));
        assert!(!folder_matches("Sent", "Sent Items", Some('/')));
        // A pattern LONGER than the name runs the matcher off the end
        // of it. Nothing tested that direction -- every case above has
        // the name at least as long as the pattern -- and the bound
        // that stops it is one character of one comparison: mutation
        // testing turned `ni < n.len()` into `ni <= n.len()`, which
        // indexes one past the end and panics, and the suite passed.
        assert!(!folder_matches("Archive", "Arch", Some('/')));
        assert!(!folder_matches("Archive/2026", "Archive/", Some('/')));
        assert!(!folder_matches("A", "", Some('/')));
        // The wildcards take their own route to the same edge.
        assert!(!folder_matches("Arch*ive", "Arch", Some('/')));
        assert!(!folder_matches("Arch%ive", "Arch", Some('/')));
    }

    #[test]
    fn a_nil_delimiter_leaves_percent_nothing_to_stop_at() {
        // RFC 3501's own LIST example is a mailbox named
        // `extended/notes` with a NIL delimiter: the slash is part of
        // the name, not a boundary. Substituting '/' made `%` stop at
        // it and `-f 'extended/%'` match nothing.
        assert!(folder_matches("extended/%", "extended/notes", None));
        assert!(folder_matches("%", "extended/notes", None));
        assert!(folder_matches("*", "extended/notes", None));
        // Without a hierarchy, `%` and `*` agree everywhere.
        for name in ["INBOX", "extended/notes", "a/b/c", ""] {
            assert_eq!(
                folder_matches("%", name, None),
                folder_matches("*", name, None),
                "{name}"
            );
        }
        // A NIL delimiter widens `%`; it does not make it match a
        // literal that was never there.
        assert!(!folder_matches("Arch%", "Sent", None));
    }

    #[test]
    fn a_nil_delimiter_expands_per_entry() {
        // The delimiter is per mailbox on the wire, so one NIL entry
        // must not widen `%` for the entries that reported one -- and
        // the empty string some servers send for NIL reads as NIL.
        let known = vec![
            ("extended/notes".to_string(), None),
            ("Archive/2026".to_string(), Some("/".to_string())),
            ("flat/name".to_string(), Some(String::new())),
        ];
        assert_eq!(
            expand_folders(&["%".to_string()], &known).unwrap(),
            vec!["extended/notes", "flat/name"]
        );
        assert_eq!(
            expand_folders(&["extended/%".to_string()], &known).unwrap(),
            vec!["extended/notes"]
        );
        assert_eq!(
            expand_folders(&["*".to_string()], &known).unwrap(),
            vec!["extended/notes", "Archive/2026", "flat/name"]
        );
    }

    #[test]
    fn expansion_keeps_order_and_rejects_a_pattern_matching_nothing() {
        let known = vec![
            ("INBOX".to_string(), Some("/".to_string())),
            ("Archive/2026".to_string(), Some("/".to_string())),
            ("Archive/2025".to_string(), Some("/".to_string())),
        ];
        let patterns = vec!["INBOX".to_string(), "Archive/*".to_string()];
        assert_eq!(
            expand_folders(&patterns, &known).unwrap(),
            vec!["INBOX", "Archive/2026", "Archive/2025"]
        );
        // A literal name is passed through even when LIST did not report it.
        assert_eq!(
            expand_folders(&["Nowhere".to_string()], &known).unwrap(),
            vec!["Nowhere"]
        );
        assert!(expand_folders(&["Nope/*".to_string()], &known).is_err());
        assert!(expand_folders(&["".to_string()], &known).is_err());
        assert!(expand_folders(&["  ".to_string()], &known).is_err());
    }

    #[test]
    fn expansion_deduplicates_across_patterns() {
        let known = vec![
            ("INBOX".to_string(), Some("/".to_string())),
            ("Archive".to_string(), Some("/".to_string())),
        ];
        let patterns = vec!["*".to_string(), "INBOX".to_string()];
        assert_eq!(
            expand_folders(&patterns, &known).unwrap(),
            vec!["INBOX", "Archive"]
        );
    }

    #[test]
    fn a_whole_large_mailbox_resolves_without_a_quadratic_scan() {
        // `*` pushes every UID the mailbox has, and deduplication used
        // to be a linear scan of everything already collected -- so
        // this is the shape that made `read '*'` or `move '*'` on a
        // large archive folder take minutes. There is no timing
        // assertion here, because one would be flaky; the guard is
        // cruder and more reliable: reintroduce the scan and this test
        // alone makes the suite unbearable to run.
        let available: Vec<u32> = (1..=200_000).collect();
        let sel = parse_selection("*").expect("'*' parses");
        let got = sel.resolve(Some(&available)).expect("resolve");
        assert_eq!(got.len(), 200_000);
        assert_eq!(got.first(), Some(&1));
        assert_eq!(got.last(), Some(&200_000));
    }

    #[test]
    fn deduplication_keeps_the_order_uids_were_named_in() {
        // The dedup went from a Vec scan to a HashSet, and a set has no
        // order -- so this pins the thing that could quietly have been
        // lost in that change: the output follows the input, and the
        // FIRST mention of a repeated UID is the one that survives.
        let available: Vec<u32> = (1..=10).collect();
        let sel = parse_selection("7,3,7,1,3").expect("parses");
        assert_eq!(sel.resolve(Some(&available)).expect("resolve"), vec![7, 3, 1]);
    }
}
