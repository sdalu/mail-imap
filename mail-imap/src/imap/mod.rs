//! IMAP access layer.
//!
//! This is a **read-only** tool: it never moves, deletes, or modifies
//! messages, tags, or flags on the server. Reads use `BODY.PEEK[]` so even
//! fetching a message does not set `\Seen`.
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

pub use mock::MockClient;
pub use real::RealClient;

use crate::config::Config;
use anyhow::Result;
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
/// read-only.
pub trait ImapBackend {
    fn list_folders(&mut self) -> Result<Vec<FolderInfo>>;
    /// Run `query` against each of `folders` in order (IMAP can only search
    /// one selected mailbox at a time, so this is iteration + aggregation).
    /// Results are grouped per folder, most-recent first within each folder,
    /// and the total result count is capped by `max_results` (0 = unlimited).
    /// Each result's `folder` names the mailbox it came from.
    fn search_folders(
        &mut self,
        folders: &[String],
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>>;
    fn get_email(&mut self, folder: &str, uid: u32) -> Result<String>;
    /// Per-mailbox counters via `STATUS`. `folder = None` means all
    /// selectable mailboxes of the account.
    fn mailbox_counts(&mut self, folder: Option<&str>) -> Result<Vec<Mailbox>>;
    /// All message UIDs of a folder.
    fn folder_uids(&mut self, folder: &str) -> Result<Vec<u32>>;
    /// All UIDs of the conversation thread containing message `uid` in
    /// `folder`, reconstructed client-side from the Message-ID /
    /// In-Reply-To / References headers (works on any IMAP server, no
    /// THREAD extension needed).
    fn thread_uids(&mut self, folder: &str, uid: u32) -> Result<Vec<u32>>;
    /// The MIME parts of one message (document order, 1-based part numbers).
    fn list_parts(&mut self, folder: &str, uid: u32) -> Result<Vec<PartInfo>>;
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
pub enum ImapClient {
    Real(RealClient),
    Mock(MockClient),
}

impl ImapClient {
    /// Connect using the backend requested by `config` (`mock` flag).
    pub fn connect(config: &Config, debug: bool) -> Result<Self> {
        if config.mock {
            Ok(ImapClient::Mock(MockClient::connect(config)?))
        } else {
            Ok(ImapClient::Real(RealClient::connect(config, debug)?))
        }
    }
}

impl ImapBackend for ImapClient {
    fn list_folders(&mut self) -> Result<Vec<FolderInfo>> {
        match self {
            ImapClient::Real(c) => c.list_folders(),
            ImapClient::Mock(c) => c.list_folders(),
        }
    }
    fn search_folders(
        &mut self,
        folders: &[String],
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>> {
        match self {
            ImapClient::Real(c) => c.search_folders(folders, query, max_results),
            ImapClient::Mock(c) => c.search_folders(folders, query, max_results),
        }
    }
    fn get_email(&mut self, folder: &str, uid: u32) -> Result<String> {
        match self {
            ImapClient::Real(c) => c.get_email(folder, uid),
            ImapClient::Mock(c) => c.get_email(folder, uid),
        }
    }
    fn mailbox_counts(&mut self, folder: Option<&str>) -> Result<Vec<Mailbox>> {
        match self {
            ImapClient::Real(c) => c.mailbox_counts(folder),
            ImapClient::Mock(c) => c.mailbox_counts(folder),
        }
    }
    fn folder_uids(&mut self, folder: &str) -> Result<Vec<u32>> {
        match self {
            ImapClient::Real(c) => c.folder_uids(folder),
            ImapClient::Mock(c) => c.folder_uids(folder),
        }
    }
    fn thread_uids(&mut self, folder: &str, uid: u32) -> Result<Vec<u32>> {
        match self {
            ImapClient::Real(c) => c.thread_uids(folder, uid),
            ImapClient::Mock(c) => c.thread_uids(folder, uid),
        }
    }
    fn list_parts(&mut self, folder: &str, uid: u32) -> Result<Vec<PartInfo>> {
        match self {
            ImapClient::Real(c) => c.list_parts(folder, uid),
            ImapClient::Mock(c) => c.list_parts(folder, uid),
        }
    }
    fn save_part(
        &mut self,
        folder: &str,
        uid: u32,
        part: u32,
        dest: &Path,
    ) -> Result<u64> {
        match self {
            ImapClient::Real(c) => c.save_part(folder, uid, part, dest),
            ImapClient::Mock(c) => c.save_part(folder, uid, part, dest),
        }
    }
    fn close(&mut self) {
        match self {
            ImapClient::Real(c) => c.close(),
            ImapClient::Mock(c) => c.close(),
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
