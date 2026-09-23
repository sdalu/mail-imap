//! IMAP access layer.
//!
//! Reads never change anything: they use `BODY.PEEK[]`, so even
//! fetching a message does not set `\Seen`. The mutating operations are
//! [`ImapBackend::store_flags`] (`flag`, `tag`),
//! [`ImapBackend::move_messages`] (`move`), [`ImapBackend::copy_messages`]
//! (`copy`), [`ImapBackend::expunge_messages`] (`expunge`),
//! [`ImapBackend::append_message`] (`append`), and the
//! folder-tree four — [`ImapBackend::create_folder`],
//! [`ImapBackend::rename_folder`], [`ImapBackend::set_subscribed`] and
//! [`ImapBackend::delete_folder`]. Every one of them is gated by
//! [`ImapClient`] on the configured [`AccessLevel`], never by its
//! caller.
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
#[cfg(feature = "mock")]
mod mock;
mod real;
mod sort;

#[cfg(feature = "mock")]
pub use mock::MockClient;
pub use mime::StrippedPart;
pub use real::RealClient;
pub use sort::{parse_sort, sort_results, SortCriteria, SortKey};

use crate::config::{AccessLevel, Config};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, FixedOffset};
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

/// What a server says it will keep in a mailbox, from the
/// `PERMANENTFLAGS` of `SELECT` (RFC 3501 §7.1).
///
/// A keyword stored outside this set is, in the spec's words, either
/// ignored or kept for the session only — so writing one and reporting
/// success would be a lie waiting to be found out at the next refresh.
#[derive(Debug, Clone, Default)]
pub struct Permanent {
    /// The names the server listed, as written.
    pub flags: Vec<String>,
    /// `\*` was among them: new keywords may be created.
    pub any_keyword: bool,
    /// The server said nothing at all, in which case RFC 3501 has the
    /// client assume every flag is permanent.
    pub unstated: bool,
}

impl Permanent {
    /// Will this mailbox keep a keyword of this name?
    pub fn keeps(&self, name: &str) -> bool {
        self.unstated
            || self.any_keyword
            || self.flags.iter().any(|f| f.eq_ignore_ascii_case(name))
    }

    /// Of `wanted`, the spellings this mailbox will actually keep.
    pub fn keepable<'a>(&self, wanted: &[&'a str]) -> Vec<&'a str> {
        wanted.iter().copied().filter(|n| self.keeps(n)).collect()
    }
}

/// A message as it sits on the server: the exact bytes, plus the two
/// things a rewrite has to carry over to the copy it puts back. Losing
/// either would be silent — the mail would still be there, dated wrong
/// or unread again.
#[derive(Debug, Clone)]
pub struct RawMessage {
    pub bytes: Vec<u8>,
    pub flags: Vec<String>,
    pub internal_date: Option<DateTime<FixedOffset>>,
}

/// What `part strip` did: where the message went, and what was taken
/// out of it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StripOutcome {
    pub folder: String,
    /// The UID that was rewritten. It no longer exists.
    pub old_uid: u32,
    /// The UID of the message that replaced it, where the server
    /// reported one.
    pub new_uid: Option<u32>,
    pub stripped: Vec<mime::StrippedPart>,
    pub bytes_before: u64,
    pub bytes_after: u64,
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

/// The operations the CLI needs from an IMAP account.
///
/// Most are reads. The ones that change the server are `store_flags`
/// (`flag`, `tag`), `move_messages` (`move`), `copy_messages` (`copy`),
/// `expunge_messages` (`expunge`), `append_message` (`append`), and
/// `create_folder`, `rename_folder`, `set_subscribed` and
/// `delete_folder` (`folder`).
/// `save_part` writes a local file and touches nothing on the server.
///
/// None of them gates itself. `access-level` is enforced in
/// [`ImapClient`], the wrapper every command goes through, so a second
/// backend cannot forget a check by implementing this trait directly.
pub trait ImapBackend {
    fn list_folders(&mut self) -> Result<Vec<FolderInfo>>;
    /// Only the mailboxes subscribed to (`LSUB`), in the same shape as
    /// [`ImapBackend::list_folders`] (`LIST`). A filter, not a separate
    /// field: every row here is subscribed by construction, so there is
    /// nothing a `subscribed` flag on [`FolderInfo`] would say that
    /// calling the right method does not already say.
    fn list_subscribed_folders(&mut self) -> Result<Vec<FolderInfo>>;
    /// Everything the server advertises, upper-cased. Empty when the
    /// server was not asked or said nothing — the mock advertises
    /// none, which is what makes it stand in for a bare server.
    fn capabilities(&mut self) -> Result<Vec<String>>;
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
    /// What the server will keep in this mailbox (`PERMANENTFLAGS`).
    fn permanent_flags(&mut self, folder: &str) -> Result<Permanent>;
    /// File messages into another mailbox: `UID MOVE` (RFC 6851) when
    /// the server has it, else `UID COPY` + `\Deleted` + `UID EXPUNGE`
    /// (RFC 4315). The copy always happens first, so a failure part-way
    /// leaves a duplicate rather than a hole.
    fn move_messages(&mut self, folder: &str, uids: &[u32], to: &str) -> Result<()>;
    /// Copy messages into another mailbox, leaving the originals where
    /// they are: `UID COPY` (RFC 3501 §6.4.7). The target must already
    /// exist, same as [`ImapBackend::move_messages`].
    fn copy_messages(&mut self, folder: &str, uids: &[u32], to: &str) -> Result<()>;
    /// Permanently remove messages already marked `\Deleted`: `UID
    /// EXPUNGE` (RFC 4315 §2.1), which needs the server to advertise
    /// `UIDPLUS` — without it a plain `EXPUNGE` is the only route, and
    /// that removes every `\Deleted` message in the mailbox rather than
    /// only these. Never sets `\Deleted` itself: of `uids`, only the
    /// ones that already carry it are removed, and the ones that were
    /// are returned. `flag add <selection> deleted` is what marks a
    /// message for removal.
    fn expunge_messages(&mut self, folder: &str, uids: &[u32]) -> Result<Vec<u32>>;
    /// Put one message into a mailbox: `APPEND` (RFC 3501 §6.3.11).
    /// `content` is the exact bytes to send -- CRLF-normalized and
    /// checked for an RFC 5322 shape by [`ImapClient`] before either
    /// backend ever sees it, so neither has to repeat that. `flags` are
    /// wire-form system flags (`cli::parse_flag_names`) to set on the
    /// message as it is created. `internal_date`, when given, sets
    /// INTERNALDATE (`--date`); `None` leaves it to the server, which
    /// RFC 3501 has default to now -- this is never taken from the
    /// message's own `Date:` header, which is the sender's clock, not
    /// when this mailbox received it. The target mailbox must already
    /// exist; this does not create it. Returns the UID the server
    /// assigned when it advertises `UIDPLUS` and reports one
    /// (`APPENDUID`), else `None`.
    fn append_message(
        &mut self,
        folder: &str,
        content: &[u8],
        flags: &[String],
        internal_date: Option<DateTime<FixedOffset>>,
    ) -> Result<Option<u32>>;
    /// One message's exact bytes, with its flags and internaldate.
    fn fetch_raw_message(&mut self, folder: &str, uid: u32) -> Result<RawMessage>;
    /// Can this backend remove named messages and leave the rest — the
    /// question `UIDPLUS` answers for a real server?
    ///
    /// Asked rather than inferred from `capabilities()` so that each
    /// backend answers for itself: the mock advertises no extensions
    /// and can still remove exactly the messages it is given, and a
    /// `part strip` that refused there would be refusing a thing that
    /// works.
    fn can_expunge_by_uid(&mut self) -> Result<bool>;
    /// Create a mailbox. `use_attr` is an RFC 6154 special-use
    /// attribute (`\Archive`, `\Sent`, ...) to declare at creation —
    /// the only moment IMAP lets a client set one — and needs the
    /// server to advertise `CREATE-SPECIAL-USE`.
    fn create_folder(&mut self, name: &str, use_attr: Option<&str>) -> Result<()>;
    /// Rename a mailbox.
    fn rename_folder(&mut self, from: &str, to: &str) -> Result<()>;
    /// Subscribe to a mailbox, or unsubscribe from it.
    fn set_subscribed(&mut self, name: &str, subscribed: bool) -> Result<()>;
    /// Delete a mailbox (`DELETE`), permanently and with everything it
    /// holds. `force` is passed through for signature parity but is not
    /// this method's to act on: the access-level, INBOX and non-empty
    /// checks (which `force` bypasses) live in [`ImapClient`], above
    /// every backend, and run before this is ever reached.
    fn delete_folder(&mut self, name: &str, force: bool) -> Result<()>;
    /// The flags (system flags + keywords) of one message, as strings.
    /// `\Recent` is omitted (transient, server-managed), matching the
    /// `flags` of search results.
    fn message_flags(&mut self, folder: &str, uid: u32) -> Result<Vec<String>>;
    /// The decoded bytes of one MIME part.
    fn fetch_part(&mut self, folder: &str, uid: u32, part: u32) -> Result<Vec<u8>>;

    /// Write one MIME part to a file, returning its size.
    ///
    /// Provided in terms of [`ImapBackend::fetch_part`] rather than the
    /// other way round, because bytes are the primitive and a file is
    /// one thing to do with them. A caller that wants the part on
    /// stdout (`part save -o -`) takes `fetch_part` and never touches
    /// the filesystem — which matters: the temporary file the other
    /// direction would need lands in a world-readable directory under a
    /// guessable name, and the content here is somebody's mail.
    fn save_part(&mut self, folder: &str, uid: u32, part: u32, dest: &Path) -> Result<u64> {
        let data = self.fetch_part(folder, uid, part)?;
        std::fs::write(dest, &data)
            .with_context(|| format!("writing part to {}", dest.display()))?;
        Ok(data.len() as u64)
    }

    fn close(&mut self);
}

#[cfg(test)]
mod permanent_tests {
    use super::Permanent;

    fn stated(flags: &[&str], any: bool) -> Permanent {
        Permanent {
            flags: flags.iter().map(|s| s.to_string()).collect(),
            any_keyword: any,
            unstated: false,
        }
    }

    #[test]
    fn a_silent_server_keeps_everything() {
        // RFC 3501: if PERMANENTFLAGS is absent the client assumes
        // every flag is permanent.
        let p = Permanent {
            unstated: true,
            ..Permanent::default()
        };
        assert!(p.keeps("anything"));
        assert_eq!(p.keepable(&["$Junk", "Junk"]), vec!["$Junk", "Junk"]);
    }

    #[test]
    fn a_server_that_takes_new_keywords_keeps_everything() {
        let p = stated(&["\\Seen", "NonJunk"], true);
        assert!(p.keeps("Junk") && p.keeps("$Junk"));
    }

    #[test]
    fn a_closed_list_keeps_only_what_it_names() {
        // Yahoo and AOL: $Junk/$NotJunk listed, no \*, which is why a
        // hardcoded `Junk` silently fails to stick there.
        let p = stated(&["\\Seen", "$Junk", "$NotJunk"], false);
        assert!(p.keeps("$Junk"));
        assert!(p.keeps("$junk"), "flag names are case-insensitive");
        assert!(!p.keeps("Junk"));
        assert!(!p.keeps("NonJunk"));
        assert_eq!(p.keepable(&["$Junk", "Junk"]), vec!["$Junk"]);
        assert!(p.keepable(&["NonJunk", "NotJunk"]).is_empty());
    }
}

/// Concrete backend selected at connect time.
// The real client carries a whole TLS session, the mock a handful of
// vectors. Exactly one client exists per run, so the size difference
// buys nothing worth boxing for.
#[allow(clippy::large_enum_variant)]
enum Backend {
    Real(RealClient),
    #[cfg(feature = "mock")]
    Mock(MockClient),
}

/// Refuse a mailbox name that would break out of its own IMAP command.
///
/// A folder name reaches the server inside a quoted string, and the
/// quoting escapes `\` and `"` -- but not CR or LF, which end a command
/// line. A name carrying one splits the command in two, and the second
/// half is run as a command in its own right. Confirmed on a live
/// server: `folder subscribe $'Evil<CR><LF>A1 NOOP'` put `C: A1 NOOP`
/// on the wire and got back `S: A1 OK NOOP completed`.
///
/// The check lives here because coverage below is patchy and invisible
/// from the call site: the `imap` crate validates `CREATE` and `SELECT`
/// this way but not `RENAME`, `SUBSCRIBE` or `UID COPY`, and the
/// `CREATE ... (USE ...)` command is built by hand in `real.rs`. One
/// rule where every folder name passes beats six that have to be
/// remembered.
fn check_mailbox_name(name: &str, what: &str) -> Result<()> {
    if name.contains(['\r', '\n']) {
        bail!(
            "the mailbox name given to {} contains a line break, which would end the \
             IMAP command and start another: refusing to send it",
            what
        );
    }
    Ok(())
}

/// Turn a lone `LF` into `CRLF`, leaving an existing `CRLF` alone.
///
/// An IMAP literal carries exact bytes: RFC 3501/5322 lines are
/// CRLF-terminated, but a message file saved on this host (or piped
/// in) commonly has bare `\n` endings. Appended as-is, that puts
/// unterminated lines on the wire -- a message that is subtly wrong on
/// the server while looking fine in every local check, since nothing
/// on this side ever reads it back byte-for-byte. Run on the whole
/// message, headers and body alike: MIME requires any non-ASCII body
/// content to already be transfer-encoded to ASCII text lines, so this
/// is safe uniformly, the same way SMTP transport already assumes.
fn normalize_line_endings(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len());
    for &b in content {
        if b == b'\n' && out.last() != Some(&b'\r') {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// Refuse content that is not shaped like an RFC 5322 message: a
/// header block -- each line either `"Name: value"` or a folded
/// continuation (starts with a space or tab) -- terminated by a blank
/// line. Cheap, and far more legible than a server's own rejection of
/// garbage. `content` is expected to already be CRLF-normalized
/// (`normalize_line_endings`).
fn check_rfc5322_shape(content: &[u8]) -> Result<()> {
    let shape_error = || {
        anyhow::anyhow!(
            "the content given to 'append' is not shaped like an RFC 5322 message: an \
             RFC 5322 message is expected -- a header block (\"Name: value\" lines, \
             folded continuations indented) terminated by a blank line"
        )
    };
    let header_end = content
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(shape_error)?;
    let headers = &content[..header_end];
    if headers.is_empty() {
        return Err(shape_error());
    }
    for line in headers.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if line[0] == b' ' || line[0] == b'\t' {
            continue; // a folded continuation of the previous header
        }
        if !line.contains(&b':') {
            return Err(shape_error());
        }
    }
    Ok(())
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
            #[cfg(feature = "mock")]
            {
                Backend::Mock(MockClient::connect(config)?)
            }
            // A build without the mock has to say so rather than
            // quietly reaching for a real server the caller did not ask
            // for -- `--mock` means "do not touch my account".
            #[cfg(not(feature = "mock"))]
            {
                bail!(
                    "this build has no mock backend: it is a development aid, and the \
                     released binary is built without it. Drop --mock (and `mock` from \
                     the config) to use the configured account, or build one with it \
                     (make build RELEASE=no)"
                );
            }
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
        #[cfg(feature = "mock")]
        {
            matches!(self.backend, Backend::Mock(_))
        }
        #[cfg(not(feature = "mock"))]
        {
            false
        }
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
    /// allow. `organize` stops here on purpose: it moves mail into
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

    /// What `read` shows for one message: the raw bytes fetched once
    /// via [`ImapBackend::fetch_raw_message`], rendered by
    /// [`mime::render_message`] -- the one renderer both backends
    /// share, so the same bytes read the same whether the mock or a
    /// real server answered the `FETCH`. Neither `real.rs` nor
    /// `mock.rs` builds this text itself any more.
    ///
    /// `raw` restores the tool's original behaviour: the header
    /// summary followed by the exact bytes the server sent, with no
    /// MIME parsing at all -- see [`mime::render_message`] for why that
    /// path must not depend on the parser succeeding.
    pub fn read_message(&mut self, folder: &str, uid: u32, raw: bool) -> Result<mime::RenderedMessage> {
        let raw_msg = self.fetch_raw_message(folder, uid)?;
        mime::render_message(&raw_msg.bytes, &raw_msg.flags, raw_msg.internal_date, raw)
            .with_context(|| format!("rendering UID {} in '{}'", uid, folder))
    }

    /// Refuse deleting a mailbox unless the access level is `full`.
    /// `restructure` is not enough on purpose: creating and renaming
    /// leave every message where it was, but deleting a mailbox loses
    /// it and everything in it, so it gets its own, stricter check
    /// rather than sharing `check_folder_change`'s.
    /// Rewrite a message without the named parts: fetch it, rebuild it
    /// with each one replaced by a stub recording what was there,
    /// `APPEND` the result, and remove the original.
    ///
    /// IMAP cannot edit a message in place, so this is the only shape
    /// available, and the order is the one that fails safely: **write
    /// first, delete last**. If the append succeeds and the removal
    /// does not, the mailbox holds two copies and a person can choose
    /// between them; the other order loses the message whenever the
    /// rebuild is wrong. `move_messages` files its fallback the same
    /// way for the same reason.
    ///
    /// The new message carries the original's flags and internaldate.
    /// Losing either would be quiet: the mail would still be there,
    /// unread again or dated the day it was stripped, and an archive
    /// dated by when it was tidied is an archive with no history.
    pub fn strip_part(&mut self, folder: &str, uid: u32, parts: &[u32]) -> Result<StripOutcome> {
        self.check_strip_part()?;
        // Asked BEFORE anything is written. The removal is the last
        // step, and discovering there that it cannot happen would mean
        // having already appended a copy that nothing then cleans up.
        if !self.can_expunge_by_uid()? {
            bail!(
                "the server does not advertise UIDPLUS (RFC 4315), so the original could                  not be removed after the rewrite -- 'part strip' would leave two copies,                  the stripped one and the message it was made from. Refusing"
            );
        }
        let original = self.fetch_raw_message(folder, uid)?;
        let stripped_at: DateTime<FixedOffset> = chrono::Local::now().into();
        let (rebuilt, records) = mime::strip_parts(&original.bytes, parts, stripped_at)
            .with_context(|| format!("rebuilding UID {} in '{}' without those parts", uid, folder))?;

        let new_uid = self
            .append_message(folder, &rebuilt, &original.flags, original.internal_date)
            .with_context(|| {
                format!(
                    "appending the rewritten UID {} to '{}' (nothing was removed: the                      original is untouched)",
                    uid, folder
                )
            })?;

        // From here the original is the copy to lose, and any failure
        // has to say that both exist rather than imply the strip did
        // not happen.
        let existing = format!(
            "the stripped copy is in '{}'{} and the original UID {} is still there",
            folder,
            match new_uid {
                Some(n) => format!(" as UID {}", n),
                None => String::new(),
            },
            uid
        );
        self.store_flags(folder, &[uid], &["\\Deleted".to_string()], &[])
            .with_context(|| format!("marking the original UID {} \\Deleted -- {}", uid, existing))?;
        self.expunge_messages(folder, &[uid])
            .with_context(|| format!("removing the original UID {} -- {}", uid, existing))?;

        Ok(StripOutcome {
            folder: folder.to_string(),
            old_uid: uid,
            new_uid,
            stripped: records,
            bytes_before: original.bytes.len() as u64,
            bytes_after: rebuilt.len() as u64,
        })
    }

    /// May this run rewrite a message? `part strip` is the only thing
    /// that does, and what it takes out does not come back.
    fn check_strip_part(&self) -> Result<()> {
        if !self.access.may_strip_part() {
            bail!(
                "access level '{}' will not rewrite a message: raise \"access-level\" to \
                 'full' in the config",
                self.access.as_str()
            );
        }
        Ok(())
    }

    fn check_folder_delete(&self) -> Result<()> {
        // Asks `AccessLevel`, rather than comparing here, so this gate
        // and the `delete a folder` line `info` prints cannot drift
        // apart into a tool that refuses what it advertises.
        if !self.access.may_delete_folder() {
            bail!(
                "access level '{}' will not delete a mailbox: raise \"access-level\" to \
                 'full' in the config",
                self.access.as_str()
            );
        }
        Ok(())
    }

    /// Refuse `UID EXPUNGE` unless the access level is `full`. Setting
    /// `\Deleted` already needs `full` (`check_flag_change` via
    /// `AccessLevel::may_set`); removing a message so marked is the
    /// same destruction, so it is held to the same level rather than a
    /// lesser one.
    fn check_expunge(&self) -> Result<()> {
        // Asks `AccessLevel`, rather than comparing here, for the same
        // reason `check_folder_delete` does: so this gate and the
        // `remove a message` line `info` prints cannot drift apart.
        if !self.access.may_expunge() {
            bail!(
                "access level '{}' will not remove a message: raise \"access-level\" to \
                 'full' in the config",
                self.access.as_str()
            );
        }
        Ok(())
    }

    /// Refuse `APPEND` unless the access level is `full`. Putting a
    /// message into a mailbox is held to the same rung as
    /// `check_expunge`/setting `\Deleted`: nothing below `full` puts
    /// mail into an account any more than it takes mail out.
    fn check_append(&self) -> Result<()> {
        // Asks `AccessLevel`, rather than comparing here, for the same
        // reason `check_expunge` does: so this gate and the `put a
        // message into a mailbox` line `info` prints cannot drift apart.
        if !self.access.may_append() {
            bail!(
                "access level '{}' will not append a message: raise \"access-level\" to \
                 'full' in the config",
                self.access.as_str()
            );
        }
        Ok(())
    }
}

impl ImapBackend for ImapClient {
    fn list_folders(&mut self) -> Result<Vec<FolderInfo>> {
        match &mut self.backend {
            Backend::Real(c) => c.list_folders(),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.list_folders(),
        }
    }
    fn list_subscribed_folders(&mut self) -> Result<Vec<FolderInfo>> {
        match &mut self.backend {
            Backend::Real(c) => c.list_subscribed_folders(),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.list_subscribed_folders(),
        }
    }
    fn capabilities(&mut self) -> Result<Vec<String>> {
        match &mut self.backend {
            Backend::Real(c) => c.capabilities(),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.capabilities(),
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
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.search_folders(folders, query, max_results, sort),
        }
    }
    fn mailbox_counts(&mut self, folder: Option<&str>) -> Result<Vec<Mailbox>> {
        match &mut self.backend {
            Backend::Real(c) => c.mailbox_counts(folder),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.mailbox_counts(folder),
        }
    }
    fn folder_uids(&mut self, folder: &str) -> Result<Vec<u32>> {
        match &mut self.backend {
            Backend::Real(c) => c.folder_uids(folder),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.folder_uids(folder),
        }
    }
    fn thread_uids(&mut self, folder: &str, uid: u32) -> Result<Vec<u32>> {
        match &mut self.backend {
            Backend::Real(c) => c.thread_uids(folder, uid),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.thread_uids(folder, uid),
        }
    }
    fn list_parts(&mut self, folder: &str, uid: u32) -> Result<Vec<PartInfo>> {
        match &mut self.backend {
            Backend::Real(c) => c.list_parts(folder, uid),
            #[cfg(feature = "mock")]
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
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.store_flags(folder, uids, add, remove),
        }
    }
    fn permanent_flags(&mut self, folder: &str) -> Result<Permanent> {
        match &mut self.backend {
            Backend::Real(c) => c.permanent_flags(folder),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.permanent_flags(folder),
        }
    }
    fn move_messages(&mut self, folder: &str, uids: &[u32], to: &str) -> Result<()> {
        if !self.access.may_move() {
            bail!(
                "access level '{}' allows no changes, and moving mail to '{}' is one: \
                 raise \"access-level\" to 'organize' in the config",
                self.access.as_str(),
                to
            );
        }
        // UID COPY, the fallback route, does not validate its
        // destination in the `imap` crate at all.
        check_mailbox_name(to, "move")?;
        if folder.eq_ignore_ascii_case(to) {
            bail!("'{}' is where those messages already are", to);
        }
        match &mut self.backend {
            Backend::Real(c) => c.move_messages(folder, uids, to),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.move_messages(folder, uids, to),
        }
    }
    fn copy_messages(&mut self, folder: &str, uids: &[u32], to: &str) -> Result<()> {
        // Same rung as `move_messages`: the message keeps existing --
        // filing a copy of it elsewhere is `organize`'s own operation,
        // and cannot need more than moving one already needs.
        if !self.access.may_move() {
            bail!(
                "access level '{}' allows no changes, and copying mail into '{}' is one: \
                 raise \"access-level\" to 'organize' in the config",
                self.access.as_str(),
                to
            );
        }
        // UID COPY does not validate its destination in the `imap`
        // crate at all (see `check_mailbox_name`'s own doc comment).
        check_mailbox_name(to, "copy")?;
        match &mut self.backend {
            Backend::Real(c) => c.copy_messages(folder, uids, to),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.copy_messages(folder, uids, to),
        }
    }
    fn expunge_messages(&mut self, folder: &str, uids: &[u32]) -> Result<Vec<u32>> {
        self.check_expunge()?;
        match &mut self.backend {
            Backend::Real(c) => c.expunge_messages(folder, uids),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.expunge_messages(folder, uids),
        }
    }
    fn append_message(
        &mut self,
        folder: &str,
        content: &[u8],
        flags: &[String],
        internal_date: Option<DateTime<FixedOffset>>,
    ) -> Result<Option<u32>> {
        self.check_append()?;
        check_mailbox_name(folder, "append")?;
        // Normalized and shape-checked here, once, so neither backend
        // has to repeat it and a wire test calling this directly (not
        // through the CLI) still exercises it.
        let content = normalize_line_endings(content);
        check_rfc5322_shape(&content)?;
        match &mut self.backend {
            Backend::Real(c) => c.append_message(folder, &content, flags, internal_date),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.append_message(folder, &content, flags, internal_date),
        }
    }
    fn fetch_raw_message(&mut self, folder: &str, uid: u32) -> Result<RawMessage> {
        match &mut self.backend {
            Backend::Real(c) => c.fetch_raw_message(folder, uid),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.fetch_raw_message(folder, uid),
        }
    }
    fn can_expunge_by_uid(&mut self) -> Result<bool> {
        match &mut self.backend {
            Backend::Real(c) => c.can_expunge_by_uid(),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.can_expunge_by_uid(),
        }
    }
    fn create_folder(&mut self, name: &str, use_attr: Option<&str>) -> Result<()> {
        self.check_folder_change("create a mailbox")?;
        check_mailbox_name(name, "folder create")?;
        match &mut self.backend {
            Backend::Real(c) => c.create_folder(name, use_attr),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.create_folder(name, use_attr),
        }
    }
    fn rename_folder(&mut self, from: &str, to: &str) -> Result<()> {
        self.check_folder_change("rename a mailbox")?;
        check_mailbox_name(from, "folder rename")?;
        check_mailbox_name(to, "folder rename")?;
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
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.rename_folder(from, to),
        }
    }
    fn set_subscribed(&mut self, name: &str, subscribed: bool) -> Result<()> {
        check_mailbox_name(
            name,
            if subscribed { "folder subscribe" } else { "folder unsubscribe" },
        )?;
        self.check_folder_change(if subscribed {
            "subscribe to a mailbox"
        } else {
            "unsubscribe from a mailbox"
        })?;
        match &mut self.backend {
            Backend::Real(c) => c.set_subscribed(name, subscribed),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.set_subscribed(name, subscribed),
        }
    }
    fn delete_folder(&mut self, name: &str, force: bool) -> Result<()> {
        self.check_folder_delete()?;
        check_mailbox_name(name, "folder delete")?;
        // The `imap` crate refuses this too (`Session::delete`'s own doc
        // comment: "It is an error to attempt to delete INBOX", RFC 3501
        // §6.3.4), but checking here means the same message regardless
        // of backend, and before a STATUS round trip is spent below.
        if name.eq_ignore_ascii_case("INBOX") {
            bail!(
                "INBOX cannot be deleted: every server refuses it, at every access \
                 level. Empty it and leave it in place if that is what you want"
            );
        }
        // The server would delete a non-empty mailbox without complaint
        // -- this tool makes the caller say they meant it. `mailbox_counts`
        // is ungated, so this is the same STATUS `count` already uses.
        if !force {
            let counts = self.mailbox_counts(Some(name))?;
            if let Some(m) = counts.first() {
                if m.messages > 0 {
                    bail!(
                        "'{}' holds {} message{} that would be destroyed along with it: \
                         pass --force to delete it anyway, or move the messages out first",
                        name,
                        m.messages,
                        if m.messages == 1 { "" } else { "s" }
                    );
                }
            }
        }
        match &mut self.backend {
            Backend::Real(c) => c.delete_folder(name, force),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.delete_folder(name, force),
        }
    }
    fn message_flags(&mut self, folder: &str, uid: u32) -> Result<Vec<String>> {
        match &mut self.backend {
            Backend::Real(c) => c.message_flags(folder, uid),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.message_flags(folder, uid),
        }
    }
    fn fetch_part(&mut self, folder: &str, uid: u32, part: u32) -> Result<Vec<u8>> {
        match &mut self.backend {
            Backend::Real(c) => c.fetch_part(folder, uid, part),
            #[cfg(feature = "mock")]
            Backend::Mock(c) => c.fetch_part(folder, uid, part),
        }
    }
    fn close(&mut self) {
        match &mut self.backend {
            Backend::Real(c) => c.close(),
            #[cfg(feature = "mock")]
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
mod mailbox_name_tests {
    use super::*;
    use crate::config::Config;

    fn client() -> ImapClient {
        ImapClient::connect(
            &Config { mock: true, access: AccessLevel::Full, ..Config::default() },
            false,
        )
        .expect("connect")
    }

    #[test]
    fn a_line_break_in_a_mailbox_name_is_refused_on_every_path() {
        // Confirmed on a live server before this guard: the second line
        // was run as its own command (`S: A1 OK NOOP completed`).
        let evil = "Evil\r\nA1 NOOP";
        let mut c = client();
        for err in [
            c.create_folder(evil, None).err(),
            c.create_folder(evil, Some("\\Archive")).err(),
            c.rename_folder(evil, "Fine").err(),
            c.rename_folder("Spam", evil).err(),
            c.set_subscribed(evil, true).err(),
            c.set_subscribed(evil, false).err(),
            c.move_messages("INBOX", &[1], evil).err(),
            c.copy_messages("INBOX", &[1], evil).err(),
            c.delete_folder(evil, false).err(),
            c.append_message(evil, b"To: a@b\r\n\r\nbody\r\n", &[], None).err(),
        ] {
            let err = err.expect("a line break must never reach the wire");
            assert!(err.to_string().contains("line break"), "wrong reason: {}", err);
        }
        // A bare LF is the same hazard.
        assert!(c.set_subscribed("Evil\nX LOGOUT", true).is_err());
        // ... and an ordinary name is untouched.
        assert!(c.create_folder("Archive/2026", None).is_ok());
    }
}

#[cfg(test)]
mod append_tests {
    use super::*;
    use crate::config::Config;

    fn client() -> ImapClient {
        ImapClient::connect(
            &Config { mock: true, access: AccessLevel::Full, ..Config::default() },
            false,
        )
        .expect("connect")
    }

    #[test]
    fn a_lone_lf_becomes_crlf_and_an_existing_crlf_is_left_alone() {
        assert_eq!(normalize_line_endings(b"a\nb\r\nc\n"), b"a\r\nb\r\nc\r\n");
        // No line ending at all: untouched.
        assert_eq!(normalize_line_endings(b"abc"), b"abc");
        // Already all-CRLF: untouched, not doubled.
        assert_eq!(normalize_line_endings(b"a\r\nb\r\n"), b"a\r\nb\r\n");
    }

    #[test]
    fn rfc5322_shape_needs_a_header_block_ended_by_a_blank_line() {
        assert!(check_rfc5322_shape(b"To: a@b\r\nSubject: x\r\n\r\nbody\r\n").is_ok());
        // A folded continuation line is still a header line.
        assert!(check_rfc5322_shape(b"To: a@b\r\n  continued\r\n\r\nbody\r\n").is_ok());
        // No blank line at all: not a message.
        assert!(check_rfc5322_shape(b"To: a@b\r\nSubject: x\r\n").is_err());
        // No header block, only a blank line: not a message.
        assert!(check_rfc5322_shape(b"\r\n\r\nbody\r\n").is_err());
        // A "header" line with no colon: not a message.
        assert!(check_rfc5322_shape(b"not a header\r\n\r\nbody\r\n").is_err());
        // Plain garbage.
        assert!(check_rfc5322_shape(b"just some bytes").is_err());
    }

    #[test]
    fn append_needs_full_access() {
        let mut c = ImapClient::connect(
            &Config { mock: true, access: AccessLevel::Organize, ..Config::default() },
            false,
        )
        .expect("connect");
        let err = c
            .append_message("INBOX", b"To: a@b\r\n\r\nbody\r\n", &[], None)
            .expect_err("organize must not append");
        assert!(err.to_string().contains("'full'"), "wrong reason: {}", err);
    }

    #[test]
    fn append_refuses_a_missing_folder_and_garbage_content() {
        let mut c = client();
        assert!(c.append_message("Nowhere", b"To: a@b\r\n\r\nbody\r\n", &[], None).is_err());
        assert!(c.append_message("INBOX", b"not a message", &[], None).is_err());
        // A lone LF is accepted -- normalized before the shape check
        // ever sees it, not refused for carrying one.
        assert!(c
            .append_message("INBOX", b"To: a@b\nSubject: x\n\nbody\n", &[], None)
            .is_ok());
    }

    #[test]
    fn the_mock_does_not_refuse_a_date_a_real_server_accepts() {
        // The mock has nowhere to keep INTERNALDATE, but taking the
        // parameter without erroring is what CLAUDE.md's rule is about:
        // a fake that is *stricter* than the real thing turns a live
        // feature into an offline refusal.
        let mut c = client();
        let when = DateTime::parse_from_rfc3339("2020-01-02T03:04:05+00:00").expect("fixture");
        assert!(c
            .append_message("INBOX", b"To: a@b\r\n\r\nbody\r\n", &[], Some(when))
            .is_ok());
    }
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
