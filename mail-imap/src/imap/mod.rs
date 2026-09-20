//! IMAP access layer.
//!
//! Operations are exposed through the [`ImapBackend`] trait. Two backends exist:
//!
//! * [`real::RealClient`] — talks to a real IMAP server over TCP/TLS using the
//!   `imap` crate. This is what the released binary uses.
//! * [`mock::MockClient`] — an in-memory mock (the original mockup), kept so the
//!   tool can be built, tested, and demoed without a live server.
//!
//! Which backend is used is chosen by `Config::mock` (and the `--mock` flag).

mod mock;
mod real;

pub use mock::MockClient;
pub use real::RealClient;

use crate::config::Config;
use anyhow::Result;

/// A single mailbox as reported by `LIST`.
#[derive(Debug, Clone)]
pub struct FolderInfo {
    pub name: String,
    pub delimiter: Option<String>,
    pub no_inferiors: bool,
    pub attrs: Vec<String>,
}

/// A single search hit.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub uid: u32,
    pub subject: String,
    pub from: String,
    pub date: Option<String>,
    pub size: Option<u32>,
    pub flags: Vec<String>,
}

/// The operations the CLI needs from an IMAP account.
pub trait ImapBackend {
    fn list_folders(&mut self) -> Result<Vec<FolderInfo>>;
    fn search_emails(&mut self, folder: &str, query: &str) -> Result<Vec<SearchResult>>;
    fn get_email(&mut self, folder: &str, uid: u32) -> Result<String>;
    fn move_email(&mut self, folder: &str, uid: u32, target: &str) -> Result<()>;
    fn tag_email(&mut self, folder: &str, uid: u32, tags: &[String]) -> Result<()>;
    fn close(&mut self);
}

/// Concrete backend selected at connect time.
pub enum ImapClient {
    Real(RealClient),
    Mock(MockClient),
}

impl ImapClient {
    /// Connect using the backend requested by `config` (`mock` flag).
    pub fn connect(config: &Config) -> Result<Self> {
        if config.mock {
            Ok(ImapClient::Mock(MockClient::connect(config)?))
        } else {
            Ok(ImapClient::Real(RealClient::connect(config)?))
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
    fn search_emails(&mut self, folder: &str, query: &str) -> Result<Vec<SearchResult>> {
        match self {
            ImapClient::Real(c) => c.search_emails(folder, query),
            ImapClient::Mock(c) => c.search_emails(folder, query),
        }
    }
    fn get_email(&mut self, folder: &str, uid: u32) -> Result<String> {
        match self {
            ImapClient::Real(c) => c.get_email(folder, uid),
            ImapClient::Mock(c) => c.get_email(folder, uid),
        }
    }
    fn move_email(&mut self, folder: &str, uid: u32, target: &str) -> Result<()> {
        match self {
            ImapClient::Real(c) => c.move_email(folder, uid, target),
            ImapClient::Mock(c) => c.move_email(folder, uid, target),
        }
    }
    fn tag_email(&mut self, folder: &str, uid: u32, tags: &[String]) -> Result<()> {
        match self {
            ImapClient::Real(c) => c.tag_email(folder, uid, tags),
            ImapClient::Mock(c) => c.tag_email(folder, uid, tags),
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
