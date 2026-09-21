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
