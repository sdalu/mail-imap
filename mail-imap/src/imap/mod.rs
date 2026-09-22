//! IMAP access layer.
//!
//! Reads never change anything: they use `BODY.PEEK[]`, so even
//! fetching a message does not set `\Seen`. The only mutating operation
//! is [`ImapBackend::store_flags`] (`UID STORE`), which the explicit
//! `flag` / `tag` commands use — and which [`ImapClient`] gates on the
//! configured [`AccessLevel`].
//!
//! Operations are exposed through the [`ImapBackend`] trait. Two backends exist:
//!
//! * [`real::RealClient`] — talks to a real IMAP server over TCP/TLS using the
//!   `imap` crate. This is what the released binary uses.
//! * [`mock::MockClient`] — an in-memory mock (the original mockup), kept so the
//!   tool can be built, tested, and demoed without a live server.
//!
//! Which backend is used is chosen by `Config::mock` (and the `--mock` flag).

mod mime;
mod mock;
mod real;
mod sort;

pub use mock::MockClient;
pub use real::RealClient;
pub use sort::{parse_sort, sort_results, SortCriteria, SortKey};

use crate::config::{AccessLevel, Config};
use anyhow::{bail, Result};
use std::path::Path;

/// A single mailbox as reported by `LIST`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FolderInfo {
    pub name: String,
    pub delimiter: Option<String>,
    pub no_inferiors: bool,
    pub attrs: Vec<String>,
}

/// A single search hit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    pub uid: u32,
    /// The mailbox the message was found in.
    pub folder: String,
    pub subject: String,
    pub from: String,
    pub date: Option<String>,
    pub size: Option<u32>,
    pub flags: Vec<String>,
    /// Number of MIME leaf parts of the message (0 when unknown).
    pub parts: u32,
}

/// Per-mailbox counters as reported by `STATUS`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Mailbox {
    pub name: String,
    pub messages: u32,
    pub unseen: u32,
    pub recent: u32,
    pub uid_next: u32,
    pub uid_validity: u32,
}

/// A single MIME part of a message, numbered in document order (1-based)
/// across all leaf parts of the message.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PartInfo {
    pub part: u32,
    pub content_type: String,
    pub filename: Option<String>,
    /// Decoded size in bytes.
    pub size: u64,
}

/// The operations the CLI needs from an IMAP account. All operations are
/// read-only except [`ImapBackend::store_flags`], which the explicit
/// `flag` / `tag` commands use (nothing ever moves or deletes mail).
pub trait ImapBackend {
    fn list_folders(&mut self) -> Result<Vec<FolderInfo>>;
    /// Run `query` against each of `folders` in order (IMAP can only search
    /// one selected mailbox at a time, so this is iteration + aggregation).
    /// Results are grouped per folder and the total result count is capped
    /// by `max_results` (0 = unlimited). Each result's `folder` names the
    /// mailbox it came from. Default order within a folder is most-recent
    /// (UID) first; `sort` (see [`parse_sort`]) orders by the given
    /// criteria instead — server-side `UID SORT` (RFC 5256) when the
    /// server advertises `SORT`, else client-side over the fetched page.
    fn search_folders(
        &mut self,
        folders: &[String],
        query: &str,
        max_results: usize,
        sort: Option<&SortCriteria>,
    ) -> Result<Vec<SearchResult>>;
    fn get_email(&mut self, folder: &str, uid: u32) -> Result<String>;
    /// Per-mailbox counters via `STATUS`. `folder = None` means all
    /// selectable mailboxes of the account.
    fn mailbox_counts(&mut self, folder: Option<&str>) -> Result<Vec<Mailbox>>;
    /// All message UIDs of a folder.
    fn folder_uids(&mut self, folder: &str) -> Result<Vec<u32>>;
    /// All UIDs of the conversation thread containing message `uid` in
    /// `folder`. Uses the server-side THREAD extension (`UID THREAD
    /// REFERENCES`, RFC 5256) when the server advertises
    /// `THREAD=REFERENCES`; otherwise reconstructs the thread client-side
    /// from the Message-ID / In-Reply-To / References headers (works on any
    /// IMAP server, no THREAD extension needed).
    fn thread_uids(&mut self, folder: &str, uid: u32) -> Result<Vec<u32>>;
    /// The MIME parts of one message (document order, 1-based part numbers).
    fn list_parts(&mut self, folder: &str, uid: u32) -> Result<Vec<PartInfo>>;
    /// Add (`add`) and/or remove (`remove`) flags — system flags like
    /// `\Seen` or keywords like `invoice` — on the given messages via
    /// `UID STORE ±FLAGS`. The tool's only mutating operation.
    fn store_flags(
        &mut self,
        folder: &str,
        uids: &[u32],
        add: &[String],
        remove: &[String],
    ) -> Result<()>;
    /// Create a mailbox. `use_attr` is an RFC 6154 special-use
    /// attribute (`\Archive`, `\Sent`, ...) to declare at creation —
    /// the only moment IMAP lets a client set one — and needs the
    /// server to advertise `CREATE-SPECIAL-USE`.
    fn create_folder(&mut self, name: &str, use_attr: Option<&str>) -> Result<()>;
    /// Rename a mailbox.
    fn rename_folder(&mut self, from: &str, to: &str) -> Result<()>;
    /// Subscribe to a mailbox, or unsubscribe from it.
    fn set_subscribed(&mut self, name: &str, subscribed: bool) -> Result<()>;
    /// The flags (system flags + keywords) of one message, as strings.
    /// `\Recent` is omitted (transient, server-managed), matching the
    /// `flags` of search results.
    fn message_flags(&mut self, folder: &str, uid: u32) -> Result<Vec<String>>;
    /// Decode MIME part `part` of message `uid` and write it to `dest`.
    /// Returns the number of bytes written.
    fn save_part(
        &mut self,
        folder: &str,
        uid: u32,
        part: u32,
        dest: &Path,
    ) -> Result<u64>;
    fn close(&mut self);
}

/// Concrete backend selected at connect time.
// The real client carries a whole TLS session, the mock a handful of
// vectors. Exactly one client exists per run, so the size difference
// buys nothing worth boxing for.
#[allow(clippy::large_enum_variant)]
enum Backend {
    Real(RealClient),
    Mock(MockClient),
}

/// The backend, plus the access level it is held to.
///
/// Every operation the CLI performs goes through here, which is why the
/// level is checked here and not in the handlers: a handler can forget,
/// and the rule that lives in one place cannot be forgotten by nine.
pub struct ImapClient {
    backend: Backend,
    access: AccessLevel,
}

impl ImapClient {
    /// Connect using the backend requested by `config` (`mock` flag).
    pub fn connect(config: &Config, debug: bool) -> Result<Self> {
        if debug {
            eprintln!(
                "Backend: {}, access level: {}",
                if config.mock { "mock" } else { "real" },
                config.access.as_str()
            );
        }
        let backend = if config.mock {
            Backend::Mock(MockClient::connect(config)?)
        } else {
            Backend::Real(RealClient::connect(config, debug)?)
        };
        Ok(ImapClient {
            backend,
            access: config.access,
        })
    }

    /// Whether this client is the in-memory mock rather than a server.
    // Used by the backend-selection test in lib.rs; the binary never
    // asks, because it is the config that decides.
    #[allow(dead_code)]
    pub fn is_mock(&self) -> bool {
        matches!(self.backend, Backend::Mock(_))
    }

    /// Refuse a flag change the access level does not allow. Clearing a
    /// flag is unrestricted above `ReadOnly`: taking `\Deleted` off a
    /// message rescues it, and taking any other flag off loses an
    /// annotation, not a message.
    fn check_flag_change(&self, add: &[String], remove: &[String]) -> Result<()> {
        if !self.access.may_store_flags() {
            bail!(
                "access level '{}' allows no changes, and {} is one: raise \
                 \"access-level\" to 'organize' in the config",
                self.access.as_str(),
                if add.is_empty() { "clearing a flag" } else { "setting a flag" }
            );
        }
        for flag in add {
            if !self.access.may_set(flag) {
                bail!(
                    "access level '{}' will not set {}: it marks the message for \
                     removal, which is what 'full' is for (clearing it is allowed here)",
                    self.access.as_str(),
                    flag
                );
            }
        }
        let _ = remove;
        Ok(())
    }

    /// Refuse a change to the folder tree the access level does not
    /// allow. `organize` stops here on purpose: it files mail into
    /// folders that exist and leaves the tree as it found it.
    fn check_folder_change(&self, what: &str) -> Result<()> {
        if !self.access.may_change_folders() {
            bail!(
                "access level '{}' leaves the folder tree alone, so it will not {}: \
                 raise \"access-level\" to 'restructure' in the config",
                self.access.as_str(),
                what
            );
        }
        Ok(())
    }
}

impl ImapBackend for ImapClient {
    fn list_folders(&mut self) -> Result<Vec<FolderInfo>> {
        match &mut self.backend {
            Backend::Real(c) => c.list_folders(),
            Backend::Mock(c) => c.list_folders(),
        }
    }
    fn search_folders(
        &mut self,
        folders: &[String],
        query: &str,
        max_results: usize,
        sort: Option<&SortCriteria>,
    ) -> Result<Vec<SearchResult>> {
        match &mut self.backend {
            Backend::Real(c) => c.search_folders(folders, query, max_results, sort),
            Backend::Mock(c) => c.search_folders(folders, query, max_results, sort),
        }
    }
    fn get_email(&mut self, folder: &str, uid: u32) -> Result<String> {
        match &mut self.backend {
            Backend::Real(c) => c.get_email(folder, uid),
            Backend::Mock(c) => c.get_email(folder, uid),
        }
    }
    fn mailbox_counts(&mut self, folder: Option<&str>) -> Result<Vec<Mailbox>> {
        match &mut self.backend {
            Backend::Real(c) => c.mailbox_counts(folder),
            Backend::Mock(c) => c.mailbox_counts(folder),
        }
    }
    fn folder_uids(&mut self, folder: &str) -> Result<Vec<u32>> {
        match &mut self.backend {
            Backend::Real(c) => c.folder_uids(folder),
            Backend::Mock(c) => c.folder_uids(folder),
        }
    }
    fn thread_uids(&mut self, folder: &str, uid: u32) -> Result<Vec<u32>> {
        match &mut self.backend {
            Backend::Real(c) => c.thread_uids(folder, uid),
            Backend::Mock(c) => c.thread_uids(folder, uid),
        }
    }
    fn list_parts(&mut self, folder: &str, uid: u32) -> Result<Vec<PartInfo>> {
        match &mut self.backend {
            Backend::Real(c) => c.list_parts(folder, uid),
            Backend::Mock(c) => c.list_parts(folder, uid),
        }
    }
    fn store_flags(
        &mut self,
        folder: &str,
        uids: &[u32],
        add: &[String],
        remove: &[String],
    ) -> Result<()> {
        self.check_flag_change(add, remove)?;
        match &mut self.backend {
            Backend::Real(c) => c.store_flags(folder, uids, add, remove),
            Backend::Mock(c) => c.store_flags(folder, uids, add, remove),
        }
    }
    fn create_folder(&mut self, name: &str, use_attr: Option<&str>) -> Result<()> {
        self.check_folder_change("create a mailbox")?;
        match &mut self.backend {
            Backend::Real(c) => c.create_folder(name, use_attr),
            Backend::Mock(c) => c.create_folder(name, use_attr),
        }
    }
    fn rename_folder(&mut self, from: &str, to: &str) -> Result<()> {
        self.check_folder_change("rename a mailbox")?;
        // RFC 3501 §6.3.5 gives RENAME INBOX a special meaning: it moves
        // every message out into the new mailbox and leaves INBOX empty.
        // Nothing is destroyed, but nobody means it, so it is refused
        // outright rather than gated by a level.
        if from.eq_ignore_ascii_case("INBOX") {
            bail!(
                "renaming INBOX does not rename it: RFC 3501 has the server move every \
                 message into '{}' and leave INBOX empty. Create '{}' and move the mail \
                 explicitly if that is what you want",
                to,
                to
            );
        }
        match &mut self.backend {
            Backend::Real(c) => c.rename_folder(from, to),
            Backend::Mock(c) => c.rename_folder(from, to),
        }
    }
    fn set_subscribed(&mut self, name: &str, subscribed: bool) -> Result<()> {
        self.check_folder_change(if subscribed {
            "subscribe to a mailbox"
        } else {
            "unsubscribe from a mailbox"
        })?;
        match &mut self.backend {
            Backend::Real(c) => c.set_subscribed(name, subscribed),
            Backend::Mock(c) => c.set_subscribed(name, subscribed),
        }
    }
    fn message_flags(&mut self, folder: &str, uid: u32) -> Result<Vec<String>> {
        match &mut self.backend {
            Backend::Real(c) => c.message_flags(folder, uid),
            Backend::Mock(c) => c.message_flags(folder, uid),
        }
    }
    fn save_part(
        &mut self,
        folder: &str,
        uid: u32,
        part: u32,
        dest: &Path,
    ) -> Result<u64> {
        match &mut self.backend {
            Backend::Real(c) => c.save_part(folder, uid, part, dest),
            Backend::Mock(c) => c.save_part(folder, uid, part, dest),
        }
    }
    fn close(&mut self) {
        match &mut self.backend {
            Backend::Real(c) => c.close(),
            Backend::Mock(c) => c.close(),
        }
    }
}

impl Drop for ImapClient {
    fn drop(&mut self) {
        self.close();
    }
}

/// The threading-relevant headers of one message: its own Message-ID(s)
/// and every Message-ID it references (References + In-Reply-To).
#[derive(Debug, Clone)]
pub struct ThreadRefs {
    pub uid: u32,
    pub message_ids: Vec<String>,
    pub references: Vec<String>,
}

/// Client-side thread reconstruction: union-find over Message-ID
/// references. Returns the sorted UIDs of the connected component
/// containing `target`, or `None` when `target` is not among `msgs`.
pub fn thread_component(target: u32, msgs: &[ThreadRefs]) -> Option<Vec<u32>> {
    if !msgs.iter().any(|m| m.uid == target) {
        return None;
    }
    let mut parent: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for m in msgs {
        parent.entry(m.uid).or_insert(m.uid);
    }
    fn find(parent: &mut std::collections::HashMap<u32, u32>, mut x: u32) -> u32 {
        while parent[&x] != x {
            let g = parent[&parent[&x]];
            parent.insert(x, g);
            x = parent[&x];
        }
        x
    }
    fn union(parent: &mut std::collections::HashMap<u32, u32>, a: u32, b: u32) {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent.insert(rb, ra);
        }
    }
    // Register every message's own IDs, merging duplicates (crossed
    // copies of the same message share a Message-ID).
    let mut by_id: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for m in msgs {
        for id in &m.message_ids {
            match by_id.get(id) {
                Some(&other) if other != m.uid => union(&mut parent, m.uid, other),
                _ => {
                    by_id.insert(id.clone(), m.uid);
                }
            }
        }
    }
    // Link each message to everything it references.
    for m in msgs {
        for r in &m.references {
            if let Some(&other) = by_id.get(r) {
                union(&mut parent, m.uid, other);
            }
        }
    }
    let root = find(&mut parent, target);
    let mut out: Vec<u32> = msgs
        .iter()
        .filter(|m| find(&mut parent, m.uid) == root)
        .map(|m| m.uid)
        .collect();
    out.sort_unstable();
    Some(out)
}

#[cfg(test)]
mod thread_tests {
    use super::{thread_component, ThreadRefs};

    fn m(uid: u32, ids: &[&str], refs: &[&str]) -> ThreadRefs {
        ThreadRefs {
            uid,
            message_ids: ids.iter().map(|s| s.to_string()).collect(),
            references: refs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn reply_chain_forms_one_thread() {
        let msgs = vec![
            m(10, &["<a@x>"], &[]),
            m(20, &["<b@x>"], &["<a@x>"]),
            m(30, &["<c@x>"], &["<a@x>", "<b@x>"]),
        ];
        assert_eq!(thread_component(10, &msgs).unwrap(), vec![10, 20, 30]);
        assert_eq!(thread_component(30, &msgs).unwrap(), vec![10, 20, 30]);
    }

    #[test]
    fn separate_threads_stay_separate() {
        let msgs = vec![
            m(1, &["<a@x>"], &[]),
            m(2, &["<b@x>"], &[]),
            m(3, &["<c@x>"], &["<b@x>"]),
        ];
        assert_eq!(thread_component(1, &msgs).unwrap(), vec![1]);
        assert_eq!(thread_component(2, &msgs).unwrap(), vec![2, 3]);
    }

    #[test]
    fn references_merge_two_threads() {
        let msgs = vec![
            m(1, &["<a@x>"], &[]),
            m(2, &["<b@x>"], &[]),
            m(3, &["<c@x>"], &["<a@x>", "<b@x>"]),
        ];
        assert_eq!(thread_component(2, &msgs).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn duplicate_message_ids_are_merged() {
        let msgs = vec![m(1, &["<a@x>"], &[]), m(2, &["<a@x>"], &[])];
        assert_eq!(thread_component(1, &msgs).unwrap(), vec![1, 2]);
    }

    #[test]
    fn unknown_target_returns_none() {
        let msgs = vec![m(1, &["<a@x>"], &[])];
        assert_eq!(thread_component(99, &msgs), None);
    }

    #[test]
    fn message_without_ids_is_its_own_thread() {
        let msgs = vec![m(1, &[], &[]), m(2, &["<b@x>"], &["<missing@x>"])];
        assert_eq!(thread_component(1, &msgs).unwrap(), vec![1]);
        assert_eq!(thread_component(2, &msgs).unwrap(), vec![2]);
    }
}
