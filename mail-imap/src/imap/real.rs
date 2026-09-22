use crate::config::Config;
use crate::imap::mime;
use crate::imap::{
    Permanent,
    sort_results, thread_component, FolderInfo, ImapBackend, Mailbox, PartInfo, RawMessage,
    SearchResult, SortCriteria, SortKey, ThreadRefs,
};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, FixedOffset};
use imap::extensions::sort::{SortCharset, SortCriterion};
use imap::extensions::thread::{ThreadAlgorithm, ThreadCharset};
use imap::{ClientBuilder, Connection, ConnectionMode, Session};
use imap_proto::NameAttribute;
use imap::types::Flag;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::ToSocketAddrs;

pub struct RealClient {
    session: Session<Connection>,
    config: Config,
    debug: bool,
    /// Server capability atoms (uppercased), fetched lazily with a single
    /// `CAPABILITY` command and cached; printed in debug mode.
    capabilities: Option<BTreeSet<String>>,
    /// Guards against double logout: `ImapClient`'s Drop and `RealClient`'s
    /// own Drop both call `close()`, and a second `LOGOUT` on the
    /// server-closed connection would fail with `ConnectionLost`.
    closed: bool,
}

/// What fetch items `search` requests from the server, from richest to
/// most robust. Some servers emit FETCH responses (raw 8-bit bytes inside
/// quoted strings in ENVELOPE/BODYSTRUCTURE) that `imap-proto` cannot
/// parse; the `imap` crate surfaces such a line as a fabricated
/// `Error::Bye` and leaves the stream desynced. The ladder degrades the
/// request until the response parses again.
const ITEMS_FULL: &str = "(UID ENVELOPE FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE)";
const ITEMS_ENVELOPE: &str = "(UID ENVELOPE FLAGS INTERNALDATE RFC822.SIZE)";
const ITEMS_HEADERS: &str = "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER.FIELDS (SUBJECT FROM DATE)])";

enum Attempt {
    Success(imap::types::Fetches),
    /// Response could not be parsed (or the connection was lost while
    /// trying); a narrower fetch item set may still work.
    Unparseable,
    /// Hard failure (server NO/BAD, or reconnect failed).
    Fatal(anyhow::Error),
}

/// Errors that leave the connection stream desynced or otherwise unusable.
fn is_poisoned(e: &imap::Error) -> bool {
    matches!(
        e,
        imap::Error::Bye(_)
            | imap::Error::TagMismatch(_)
            | imap::Error::ConnectionLost
            | imap::Error::Io(_)
    )
}

impl RealClient {
    pub fn connect(config: &Config, debug: bool) -> Result<Self> {
        let session = Self::establish_session(config, debug)?;
        let client = RealClient {
            session,
            config: config.clone(),
            debug,
            capabilities: None,
            closed: false,
        };
        Ok(client)
    }

    /// Log in and return a fresh session. Used for the initial connect and
    /// to recover after a poisoned response desyncs the stream.
    fn establish_session(config: &Config, debug: bool) -> Result<Session<Connection>> {
        let addr = (config.server.as_str(), config.port);
        let socket_addr = addr
            .to_socket_addrs()
            .with_context(|| format!("could not resolve {}", config.server))?
            .next()
            .with_context(|| format!("no addresses found for {}", config.server))?;
        if debug {
            eprintln!("Connecting to {}:{}...", config.server, config.port);
        }
        let tcp = std::net::TcpStream::connect_timeout(&socket_addr, std::time::Duration::from_secs(10))
            .with_context(|| format!("could not connect to {} at {}", config.server, socket_addr))?;
        tcp.set_nodelay(true)?;

        let mut builder = ClientBuilder::new(config.server.as_str(), config.port);
        if config.ssl {
            if debug {
                eprintln!("Starting TLS handshake with {}...", config.server);
            }
            builder = builder.mode(ConnectionMode::Tls);
        } else if config.starttls {
            if debug {
                eprintln!("Starting STARTTLS upgrade with {}...", config.server);
            }
            builder = builder.mode(ConnectionMode::StartTls);
        } else {
            builder = builder.mode(ConnectionMode::Plaintext);
        }

        if config.insecure {
            builder = builder.danger_skip_tls_verify(true);
        }

        let client = builder.connect()
            .with_context(|| format!("could not connect to {}:{}", config.server, config.port))?;

        if debug {
            eprintln!("Logging in as '{}'...", config.username);
        }

        let session = client
            .login(config.username.as_str(), config.password.as_str())
            .map_err(|(e, _)| e)
            .with_context(|| format!("login as '{}' failed (check credentials / server)", config.username))?;

        Ok(session)
    }

    /// Re-establish the session and re-select `folder`.
    fn reconnect(&mut self, folder: &str) -> Result<()> {
        self.session = Self::establish_session(&self.config, self.debug)
            .with_context(|| format!("reconnecting to {}", self.config.server))?;
        self.capabilities = None;
        self.session
            .select(folder)
            .with_context(|| format!("re-selecting '{}' after reconnect", folder))?;
        Ok(())
    }

    /// Fetch (once) and cache the server capabilities; in debug mode print
    /// them. A `CAPABILITY` failure is not fatal: the cached list stays
    /// empty and no extension requiring it will be used.
    fn ensure_capabilities(&mut self) {
        if self.capabilities.is_some() {
            return;
        }
        let caps = self
            .session
            .capabilities()
            .map(|c| {
                c.iter()
                    .map(capability_to_string)
                    .map(|s| s.to_uppercase())
                    .collect::<BTreeSet<String>>()
            })
            .unwrap_or_default();
        // The list itself is `info`'s to report, on its `advertises`
        // line -- a debug trace repeating it is noise in front of the
        // exchange it exists to show. A failed fetch still gets a word,
        // because that is the one thing `info` cannot distinguish: an
        // empty `advertises` looks the same whether the server named
        // nothing or the CAPABILITY command failed.
        if self.debug && caps.is_empty() {
            eprintln!("Could not fetch server capabilities");
        }
        self.capabilities = Some(caps);
    }

    /// True when the server advertises the given capability atom
    /// (case-insensitive).
    fn has_capability(&mut self, cap: &str) -> bool {
        self.ensure_capabilities();
        self.capabilities
            .as_ref()
            .map(|caps| caps.contains(cap))
            .unwrap_or(false)
    }

    /// Server-side threading via the RFC 5256 `UID THREAD REFERENCES`
    /// extension, tried before the client-side reconstruction. Returns
    /// `Ok(None)` (fall back to the client-side path) when the server does
    /// not advertise `THREAD=REFERENCES`, the command fails, or the target
    /// is not among the returned threads. `folder` must already be
    /// selected.
    fn native_thread_uids(&mut self, folder: &str, uid: u32) -> Result<Option<Vec<u32>>> {
        if !self.has_capability("THREAD=REFERENCES") {
            if self.debug {
                eprintln!("server does not advertise THREAD=REFERENCES");
            }
            return Ok(None);
        }
        let threads = match self.session.uid_thread(
            ThreadAlgorithm::References,
            ThreadCharset::Utf8,
            "ALL",
        ) {
            Ok(threads) => threads,
            Err(e) => {
                if is_poisoned(&e) {
                    self.reconnect(folder)?;
                }
                if self.debug {
                    eprintln!("UID THREAD failed: {}", e);
                }
                return Ok(None);
            }
        };
        for top in &threads {
            let mut uids = top.all_message_numbers();
            if uids.contains(&uid) {
                uids.sort_unstable();
                if self.debug {
                    eprintln!(
                        "Threading method: server-side UID THREAD REFERENCES (RFC 5256), {} message(s) in thread of UID {}",
                        uids.len(),
                        uid
                    );
                }
                return Ok(Some(uids));
            }
        }
        if self.debug {
            eprintln!("UID THREAD: no thread returned for UID {}", uid);
        }
        Ok(None)
    }

    /// Fetch the raw RFC822 bytes of a message without setting `\Seen`
    /// (`BODY.PEEK[]`).
    fn fetch_raw(&mut self, folder: &str, uid: u32) -> Result<Vec<u8>> {
        self.session.select(folder)?;
        let fetches = self.session
            .uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")
            .with_context(|| format!("UID FETCH of UID {} in '{}'", uid, folder))?;
        let data = fetches.iter().find_map(|f| f.body().map(|b| b.to_vec()));
        match data {
            Some(d) => Ok(d),
            None => Err(anyhow::anyhow!(
                "no email with UID {} in folder '{}'",
                uid,
                folder
            )),
        }
    }

    /// Search one selected mailbox: `UID SEARCH`, then a batched fetch of
    /// the metadata for the most-recent UIDs, capped at `cap`.
    fn search_in_folder(
        &mut self,
        folder: &str,
        query: &str,
        cap: usize,
        sort: Option<&SortCriteria>,
    ) -> Result<Vec<SearchResult>> {
        self.session.select(folder)?;

        // Desired UID order: server-side `UID SORT` when a SORT-capable
        // server can carry every criterion, otherwise most-recent first.
        let mut server_sorted = false;
        let mut uids: Vec<u32> = Vec::new();
        if let Some(spec) = sort {
            if spec.server_sortable() && self.has_capability("SORT") {
                let bases: Vec<SortCriterion<'static>> = spec
                    .keys
                    .iter()
                    .map(|(key, _)| match key {
                        SortKey::Date => SortCriterion::Date,
                        SortKey::Arrival => SortCriterion::Arrival,
                        SortKey::Size => SortCriterion::Size,
                        SortKey::Subject => SortCriterion::Subject,
                        SortKey::From => SortCriterion::From,
                        SortKey::To => SortCriterion::To,
                        SortKey::Cc => SortCriterion::Cc,
                        SortKey::Uid => unreachable!("guarded by server_sortable()"),
                    })
                    .collect();
                let crits: Vec<SortCriterion> = spec
                    .keys
                    .iter()
                    .zip(&bases)
                    .map(|((_, reverse), base)| {
                        if *reverse {
                            SortCriterion::Reverse(base)
                        } else {
                            *base
                        }
                    })
                    .collect();
                match self.session.uid_sort(&crits, SortCharset::Utf8, query) {
                    Ok(list) => {
                        if self.debug {
                            eprintln!(
                                "search: server-side UID SORT ({})",
                                crits
                                    .iter()
                                    .map(|c| c.to_string())
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            );
                        }
                        uids = list;
                        server_sorted = true;
                    }
                    Err(e) => {
                        if self.debug {
                            eprintln!(
                                "UID SORT failed ({}); falling back to client-side sort",
                                e
                            );
                        }
                        if is_poisoned(&e) {
                            self.reconnect(folder)?;
                        }
                    }
                }
            }
            if !server_sorted
                && spec
                    .keys
                    .iter()
                    .any(|(k, _)| matches!(k, SortKey::To | SortKey::Cc))
            {
                bail!(
                    "cannot sort by 'to'/'cc': the server does not advertise SORT (RFC 5256)"
                );
            }
        }
        if !server_sorted {
            uids = self
                .uid_search_charset(query)?
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            // Without a server sort, fetch the newest messages: UIDs
            // increase over time, and any client-side sort wants recency.
            uids.reverse();
        }
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        if cap > 0 {
            uids.truncate(cap);
        }

        let mut out: Vec<SearchResult> = Vec::new();
        const BATCH: usize = 25;
        for chunk in uids.chunks(BATCH) {
            let list = chunk
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<String>>()
                .join(",");
            match self.fetch_chunk(folder, &list) {
                Ok(results) => out.extend(results),
                Err(e) => {
                    if out.is_empty() {
                        return Err(e);
                    }
                    // Partial results beat none at all.
                    eprintln!(
                        "warning: search stopped early; reporting {} result(s) fetched so far: {:?}",
                        out.len(),
                        e
                    );
                    break;
                }
            }
        }

        // FETCH responses come back in sequence-number order; restore the
        // UID order computed above.
        let mut by_uid: HashMap<u32, SearchResult> =
            out.into_iter().map(|r| (r.uid, r)).collect();
        let mut ordered: Vec<SearchResult> = Vec::with_capacity(by_uid.len());
        for uid in &uids {
            if let Some(r) = by_uid.remove(uid) {
                ordered.push(r);
            }
        }
        ordered.extend(by_uid.into_values());
        out = ordered;
        // Fallback path: sort the fetched (most-recent-capped) page
        // client-side. Selection can therefore differ from a true
        // sorted-and-capped result set for non-date criteria.
        if !server_sorted {
            if let Some(spec) = sort {
                sort_results(&mut out, spec);
            }
        }
        Ok(out)
    }

    /// `query` with `CHARSET UTF-8 ` prepended, or `None` when `query` is
    /// pure ASCII and belongs on the wire unchanged. Split out of
    /// `uid_search_charset` because this half needs no session to test.
    fn charset_prefixed(query: &str) -> Option<String> {
        if query.is_ascii() {
            None
        } else {
            Some(format!("CHARSET UTF-8 {}", query))
        }
    }

    /// `UID SEARCH`, declaring `CHARSET UTF-8` only when the query is not
    /// pure ASCII: RFC 3501 lets a server refuse a non-ASCII search when
    /// no charset is declared, and a pure-ASCII query still goes on the
    /// wire unchanged, so servers that dislike `CHARSET` never see it. If
    /// the declared-charset form comes back `NO`/`BAD`, retries once
    /// without it -- a server refusing the charset is not a failed
    /// search, it just cannot do more than ASCII.
    fn uid_search_charset(&mut self, query: &str) -> imap::Result<HashSet<u32>> {
        let Some(charset_query) = Self::charset_prefixed(query) else {
            if self.debug {
                eprintln!("search: UID SEARCH (ASCII)");
            }
            return self.session.uid_search(query);
        };
        if self.debug {
            eprintln!("search: UID SEARCH CHARSET UTF-8");
        }
        match self.session.uid_search(charset_query) {
            Err(imap::Error::Bad(_)) | Err(imap::Error::No(_)) => {
                if self.debug {
                    eprintln!("UID SEARCH CHARSET UTF-8 refused; retrying without it");
                }
                self.session.uid_search(query)
            }
            other => other,
        }
    }

    /// Fetch metadata for one comma-separated UID list, degrading the
    /// request until it parses: batch with BODYSTRUCTURE, batch without
    /// it, then per-message, finally per-message via a literal
    /// HEADER.FIELDS fetch whose contents `imap-proto` never parses.
    fn fetch_chunk(&mut self, folder: &str, list: &str) -> Result<Vec<SearchResult>> {
        for items in [ITEMS_FULL, ITEMS_ENVELOPE] {
            match self.attempt_fetch(folder, list, items) {
                Attempt::Success(fs) => {
                    return Ok(fs.iter().map(|f| self.fetch_to_result(f, folder)).collect())
                }
                Attempt::Unparseable => continue,
                Attempt::Fatal(e) => return Err(e),
            }
        }
        let mut out = Vec::new();
        for uid in list.split(',') {
            match self.attempt_fetch(folder, uid, ITEMS_ENVELOPE) {
                Attempt::Success(fs) => {
                    out.extend(fs.iter().map(|f| self.fetch_to_result(f, folder)));
                    continue;
                }
                Attempt::Unparseable => {}
                Attempt::Fatal(e) => return Err(e),
            }
            match self.attempt_fetch(folder, uid, ITEMS_HEADERS) {
                Attempt::Success(fs) => out.extend(
                    fs.iter()
                        .map(|f| self.fetch_to_result_from_headers(f, folder)),
                ),
                Attempt::Unparseable => {
                    eprintln!(
                        "warning: skipping UID {} in '{}': server response could not be parsed",
                        uid, folder
                    );
                }
                Attempt::Fatal(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// One `UID FETCH` attempt with recovery: on a connection-poisoning
    /// error, reconnect once and retry; anything the parser still rejects
    /// comes back as `Attempt::Unparseable` so the caller can narrow the
    /// requested items.
    fn attempt_fetch(&mut self, folder: &str, list: &str, items: &str) -> Attempt {
        let mut err = match self.session.uid_fetch(list, items) {
            Ok(fs) => return Attempt::Success(fs),
            Err(e) => e,
        };
        if is_poisoned(&err) {
            if self.debug {
                eprintln!("Connection desynced ({}); reconnecting...", err);
            }
            if let Err(e) = self.reconnect(folder) {
                return Attempt::Fatal(e);
            }
            err = match self.session.uid_fetch(list, items) {
                Ok(fs) => return Attempt::Success(fs),
                Err(e) => e,
            };
            if is_poisoned(&err) {
                // Even a fresh connection cannot carry this item set: the
                // offending bytes are in the data itself.
                return Attempt::Unparseable;
            }
        }
        match err {
            imap::Error::Parse(_) | imap::Error::Unexpected(_) => Attempt::Unparseable,
            e => Attempt::Fatal(e.into()),
        }
    }

    /// One `UID STORE <list> <mode>FLAGS (...)` round trip with the same
    /// reconnect-once recovery as fetches (`mode` is `'+'` or `'-'`).
    fn store_one(&mut self, folder: &str, list: &str, mode: char, flags: &[String]) -> Result<()> {
        let items = format!("{}FLAGS ({})", mode, flags.join(" "));
        let mut err = match self.session.uid_store(list, &items) {
            Ok(_) => return Ok(()),
            Err(e) => e,
        };
        if is_poisoned(&err) {
            if self.debug {
                eprintln!("Connection desynced ({}); reconnecting...", err);
            }
            self.reconnect(folder)?;
            match self.session.uid_store(list, &items) {
                Ok(_) => return Ok(()),
                Err(e2) => err = e2,
            }
        }
        Err(anyhow::Error::from(err).context(format!(
            "UID STORE {} {} in '{}' failed",
            list, items, folder
        )))
    }

    /// Of `uids`, the ones the server currently reports as carrying
    /// `\Deleted`. `folder` must already be selected.
    fn deleted_among(&mut self, folder: &str, uids: &[u32]) -> Result<Vec<u32>> {
        let mut out = Vec::new();
        const BATCH: usize = 50;
        for chunk in uids.chunks(BATCH) {
            let list = chunk
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let fetches = match self.attempt_fetch(folder, &list, "(UID FLAGS)") {
                Attempt::Success(fs) => fs,
                Attempt::Unparseable => bail!(
                    "could not read flags for UID(s) {} in '{}': server response could not \
                     be parsed",
                    list,
                    folder
                ),
                Attempt::Fatal(e) => return Err(e),
            };
            for f in fetches.iter() {
                if let Some(uid) = f.uid {
                    if f.flags().iter().any(|fl| matches!(fl, Flag::Deleted)) {
                        out.push(uid);
                    }
                }
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    fn fetch_to_result(&self, f: &imap::types::Fetch<'_>, folder: &str) -> SearchResult {
        let env = f.envelope();
        let subject = env
            .and_then(|e| e.subject.as_ref())
            .map(|s| decode_rfc2047(bytes_to_string(s)))
            .unwrap_or_else(|| "(no subject)".to_string());
        let from = env
            .and_then(|e| e.from.as_ref())
            .and_then(|addrs| addrs.first())
            .map(format_address)
            .unwrap_or_else(|| "(unknown)".to_string());
        let date = f
            .internal_date()
            .map(|d| d.format("%Y-%m-%d %H:%M:%S %z").to_string());
        let flags: Vec<String> = f
            .flags()
            .iter()
            .filter(|fl| !matches!(fl, Flag::Recent))
            .map(|fl| fl.to_string())
            .collect();
        SearchResult {
            uid: f.uid.unwrap_or(0),
            folder: folder.to_string(),
            subject,
            from,
            date,
            size: f.size,
            flags,
            parts: f.bodystructure().map(count_leaf_parts).unwrap_or(0),
        }
    }

    /// Same as `fetch_to_result`, for messages whose ENVELOPE the
    /// `imap-proto` parser rejects: metadata is rebuilt from a literal
    /// `BODY.PEEK[HEADER.FIELDS ...]` fetch, which carries raw bytes that
    /// are never parsed as quoted strings.
    fn fetch_to_result_from_headers(
        &self,
        f: &imap::types::Fetch<'_>,
        folder: &str,
    ) -> SearchResult {
        let raw = f.header().unwrap_or(&[]);
        let subject = header_value(raw, b"Subject")
            .map(|v| decode_rfc2047(bytes_to_string(&v)))
            .unwrap_or_else(|| "(no subject)".to_string());
        let from = header_value(raw, b"From")
            .map(|v| address_from_header(&v))
            .unwrap_or_else(|| "(unknown)".to_string());
        let date = f
            .internal_date()
            .map(|d| d.format("%Y-%m-%d %H:%M:%S %z").to_string())
            .or_else(|| header_value(raw, b"Date").map(|v| bytes_to_string(&v)));
        let flags: Vec<String> = f
            .flags()
            .iter()
            .filter(|fl| !matches!(fl, Flag::Recent))
            .map(|fl| fl.to_string())
            .collect();
        SearchResult {
            uid: f.uid.unwrap_or(0),
            folder: folder.to_string(),
            subject,
            from,
            date,
            size: f.size,
            flags,
            parts: 0,
        }
    }
}

/// The RFC 6154 special use a `LIST` attribute names, if it is one.
fn special_use_attr(attr: &NameAttribute<'_>) -> Option<&'static str> {
    match attr {
        NameAttribute::All => Some("\\All"),
        NameAttribute::Archive => Some("\\Archive"),
        NameAttribute::Drafts => Some("\\Drafts"),
        NameAttribute::Flagged => Some("\\Flagged"),
        NameAttribute::Junk => Some("\\Junk"),
        NameAttribute::Sent => Some("\\Sent"),
        NameAttribute::Trash => Some("\\Trash"),
        _ => None,
    }
}

/// Turn a `LIST`/`LSUB` response into the [`FolderInfo`]s this tool
/// reports, shared so the two wire commands cannot drift apart.
fn folder_infos(names: &imap::types::Names) -> Vec<FolderInfo> {
    names
        .iter()
        .filter(|n| !n.attributes().contains(&NameAttribute::NoSelect))
        .map(|n| {
            let mut attrs = Vec::new();
            if n.attributes().contains(&NameAttribute::Marked) {
                attrs.push("\\Marked".to_string());
            }
            // RFC 6154: the special uses are what says which mailbox is
            // the Trash on an account that does not call it "Trash".
            for attr in n.attributes() {
                if let Some(name) = special_use_attr(attr) {
                    attrs.push(name.to_string());
                }
            }
            FolderInfo {
                name: n.name().to_string(),
                delimiter: n.delimiter().map(str::to_string),
                no_inferiors: n.attributes().contains(&NameAttribute::NoInferiors),
                attrs,
            }
        })
        .collect()
}

impl ImapBackend for RealClient {
    fn capabilities(&mut self) -> Result<Vec<String>> {
        self.ensure_capabilities();
        Ok(self
            .capabilities
            .as_ref()
            .map(|caps| caps.iter().cloned().collect())
            .unwrap_or_default())
    }

    fn list_folders(&mut self) -> Result<Vec<FolderInfo>> {
        Ok(folder_infos(&self.session.list(None, Some("*"))?))
    }

    fn list_subscribed_folders(&mut self) -> Result<Vec<FolderInfo>> {
        Ok(folder_infos(&self.session.lsub(None, Some("*"))?))
    }

    fn search_folders(
        &mut self,
        folders: &[String],
        query: &str,
        max_results: usize,
        sort: Option<&SortCriteria>,
    ) -> Result<Vec<SearchResult>> {
        let mut out: Vec<SearchResult> = Vec::new();
        for folder in folders {
            if max_results > 0 && out.len() >= max_results {
                break;
            }
            let cap = if max_results > 0 {
                max_results - out.len()
            } else {
                usize::MAX
            };
            out.extend(self.search_in_folder(folder, query, cap, sort)?);
        }
        Ok(out)
    }

    fn get_email(&mut self, folder: &str, uid: u32) -> Result<String> {
        self.session.select(folder)?;
        // BODY.PEEK[] keeps the server from setting \Seen on the message.
        let uid_s = uid.to_string();
        const READ_ITEMS: &str = "(UID ENVELOPE FLAGS INTERNALDATE BODY.PEEK[])";
        const READ_ITEMS_NOENV: &str = "(UID FLAGS INTERNALDATE BODY.PEEK[])";
        let fetches = match self.attempt_fetch(folder, &uid_s, READ_ITEMS) {
            Attempt::Success(fs) => fs,
            Attempt::Unparseable => {
                // The server's ENVELOPE for this message carries bytes
                // `imap-proto` cannot parse (e.g. raw 8-bit): fetch the
                // raw message and build the summary from it instead.
                match self.attempt_fetch(folder, &uid_s, READ_ITEMS_NOENV) {
                    Attempt::Success(fs) => fs,
                    Attempt::Unparseable => bail!(
                        "could not fetch UID {} in '{}': server response was unparseable",
                        uid,
                        folder
                    ),
                    Attempt::Fatal(e) => return Err(e),
                }
            }
            Attempt::Fatal(e) => {
                return Err(e)
                    .with_context(|| format!("UID FETCH of UID {} in '{}'", uid, folder));
            }
        };
        let fetches: Vec<&imap::types::Fetch> = fetches.iter().collect();
        if fetches.is_empty() {
            bail!("no email with UID {} in folder '{}'", uid, folder);
        }
        let f = &fetches[0];
        let mut out = String::new();

        if let Some(env) = f.envelope() {
            if let Some(subject) = env.subject.as_ref() {
                out.push_str(&format!(
                    "Subject: {}\n",
                    decode_rfc2047(bytes_to_string(subject))
                ));
            }
            if let Some(from) = env.from.as_ref().and_then(|a| a.first()) {
                out.push_str(&format!("From: {}\n", format_address(from)));
            }
            if let Some(to) = env.to.as_ref().and_then(|a| a.first()) {
                out.push_str(&format!("To: {}\n", format_address(to)));
            }
            if let Some(date) = env.date.as_ref() {
                out.push_str(&format!("Date: {}\n", bytes_to_string(date)));
            }
        } else if let Some(raw) = f.body() {
            // No ENVELOPE (parser rejected it): summarize the raw header
            // block of the RFC822 literal ourselves.
            if let Some(v) = header_value(raw, b"Subject") {
                out.push_str(&format!(
                    "Subject: {}\n",
                    decode_rfc2047(bytes_to_string(&v))
                ));
            }
            if let Some(v) = header_value(raw, b"From") {
                out.push_str(&format!("From: {}\n", bytes_to_string(&v)));
            }
            if let Some(v) = header_value(raw, b"To") {
                out.push_str(&format!("To: {}\n", bytes_to_string(&v)));
            }
            if let Some(v) = header_value(raw, b"Date") {
                out.push_str(&format!("Date: {}\n", bytes_to_string(&v)));
            }
        }
        if let Some(d) = f.internal_date() {
            out.push_str(&format!("InternalDate: {}\n", d.format("%Y-%m-%d %H:%M:%S %z")));
        }
        let flags: Vec<String> = f.flags().iter().map(|fl| fl.to_string()).collect();
        if !flags.is_empty() {
            out.push_str(&format!("Flags: {}\n", flags.join(", ")));
        }
        out.push('\n');

        if let Some(body) = f.body() {
            out.push_str(&bytes_to_string(body));
        } else {
            out.push_str("(no message body)\n");
        }
        Ok(out)
    }

    fn mailbox_counts(&mut self, folder: Option<&str>) -> Result<Vec<Mailbox>> {
        let names: Vec<String> = match folder {
            Some(f) => vec![f.to_string()],
            None => self.session
                .list(None, Some("*"))?
                .iter()
                .filter(|n| !n.attributes().contains(&NameAttribute::NoSelect))
                .map(|n| n.name().to_string())
                .collect(),
        };
        let mut out = Vec::new();
        for name in names {
            let m = self.session
                .status(&name, "(MESSAGES UNSEEN RECENT UIDNEXT UIDVALIDITY)")
                .with_context(|| format!("STATUS for mailbox '{}'", name))?;
            out.push(Mailbox {
                name,
                messages: m.exists,
                unseen: m.unseen.unwrap_or(0),
                recent: m.recent,
                uid_next: m.uid_next.unwrap_or(0),
                uid_validity: m.uid_validity.unwrap_or(0),
            });
        }
        Ok(out)
    }

    fn folder_uids(&mut self, folder: &str) -> Result<Vec<u32>> {
        self.session.select(folder)?;
        // `ALL` is the standard key for "every message in the mailbox".
        let uids: Vec<u32> = self.session.uid_search("ALL")?.into_iter().collect();
        Ok(uids)
    }

    /// Thread reconstruction, in order of preference:
    ///
    /// 1. server-side `UID THREAD REFERENCES` (RFC 5256) when the server
    ///    advertises `THREAD=REFERENCES` — a single round trip;
    /// 2. client-side reconstruction from Message-ID / In-Reply-To /
    ///    References headers fetched as a literal
    ///    `BODY[HEADER.FIELDS ...]` block — raw bytes we parse ourselves,
    ///    so the response cannot break the `imap-proto` quoted-string parser
    ///    the way ENVELOPE can (see `ITEMS_HEADERS`). Works on any IMAP
    ///    server, no THREAD extension needed.
    fn thread_uids(&mut self, folder: &str, uid: u32) -> Result<Vec<u32>> {
        const THREAD_ITEMS: &str =
            "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID REFERENCES IN-REPLY-TO)])";
        self.session
            .select(folder)
            .with_context(|| format!("could not select '{}'", folder))?;
        if let Some(uids) = self.native_thread_uids(folder, uid)? {
            return Ok(uids);
        }
        if self.debug {
            eprintln!(
                "Threading method: client-side reconstruction from Message-ID / In-Reply-To / References headers"
            );
        }
        let all: Vec<u32> = self
            .session
            .uid_search("ALL")
            .with_context(|| format!("UID SEARCH ALL in '{}'", folder))?
            .into_iter()
            .collect();
        if !all.contains(&uid) {
            bail!("no email with UID {} in folder '{}'", uid, folder);
        }
        let mut all = all;
        all.sort_unstable();
        let mut msgs: Vec<ThreadRefs> = Vec::new();
        const BATCH: usize = 100;
        for chunk in all.chunks(BATCH) {
            let list = chunk
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<String>>()
                .join(",");
            match self.attempt_fetch(folder, &list, THREAD_ITEMS) {
                Attempt::Success(fs) => {
                    for f in fs.iter() {
                        if let Some(u) = f.uid {
                            msgs.push(thread_refs_of_fetch(u, f.header().unwrap_or(&[])));
                        }
                    }
                }
                Attempt::Unparseable => {
                    eprintln!(
                        "warning: could not parse threading headers of {} message(s) in '{}'; thread may be incomplete",
                        chunk.len(),
                        folder
                    );
                }
                Attempt::Fatal(e) => return Err(e),
            }
        }
        thread_component(uid, &msgs)
            .with_context(|| format!("threading UID {} in '{}'", uid, folder))
    }

    fn list_parts(&mut self, folder: &str, uid: u32) -> Result<Vec<PartInfo>> {
        let raw = self.fetch_raw(folder, uid)?;
        let root = mime::parse_message(&raw)
            .with_context(|| format!("parsing MIME structure of UID {}", uid))?;
        root.leaves()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                Ok(PartInfo {
                    part: (i + 1) as u32,
                    content_type: p.content_type.clone(),
                    filename: p.filename.clone(),
                    size: p.decoded()?.len() as u64,
                })
            })
            .collect()
    }

    fn fetch_part(&mut self, folder: &str, uid: u32, part: u32) -> Result<Vec<u8>> {
        let raw = self.fetch_raw(folder, uid)?;
        let root = mime::parse_message(&raw)
            .with_context(|| format!("parsing MIME structure of UID {}", uid))?;
        let leaves = root.leaves();
        let idx = match part.checked_sub(1) {
            Some(i) => i as usize,
            None => bail!("part number must be >= 1 (got {})", part),
        };
        let leaf = leaves.get(idx).ok_or_else(|| {
            anyhow::anyhow!(
                "no part {} in message UID {} (message has {} part(s))",
                part,
                uid,
                leaves.len()
            )
        })?;
        leaf.decoded()
    }

    fn store_flags(
        &mut self,
        folder: &str,
        uids: &[u32],
        add: &[String],
        remove: &[String],
    ) -> Result<()> {
        self.session
            .select(folder)
            .with_context(|| format!("could not select '{}'", folder))?;
        const BATCH: usize = 50;
        for chunk in uids.chunks(BATCH) {
            let list = chunk
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            if !remove.is_empty() {
                self.store_one(folder, &list, '-', remove)?;
            }
            if !add.is_empty() {
                self.store_one(folder, &list, '+', add)?;
            }
        }
        Ok(())
    }

    fn permanent_flags(&mut self, folder: &str) -> Result<Permanent> {
        let mailbox = self
            .session
            .select(folder)
            .with_context(|| format!("could not select '{}'", folder))?;
        let mut out = Permanent {
            unstated: mailbox.permanent_flags.is_empty(),
            ..Permanent::default()
        };
        for flag in &mailbox.permanent_flags {
            match flag {
                Flag::MayCreate => out.any_keyword = true,
                other => out.flags.push(other.to_string()),
            }
        }
        if self.debug {
            eprintln!(
                "PERMANENTFLAGS of '{}': {} {}",
                folder,
                if out.unstated {
                    "(none stated; every flag is permanent)".to_string()
                } else {
                    out.flags.join(" ")
                },
                if out.any_keyword { "\\* (new keywords allowed)" } else { "" }
            );
        }
        Ok(out)
    }

    fn move_messages(&mut self, folder: &str, uids: &[u32], to: &str) -> Result<()> {
        self.session
            .select(folder)
            .with_context(|| format!("could not select '{}'", folder))?;
        let native = self.has_capability("MOVE");
        let uidplus = self.has_capability("UIDPLUS");
        if !native && !uidplus {
            // The only remaining route is COPY + \Deleted + EXPUNGE, and
            // a plain EXPUNGE removes every \Deleted message in the
            // mailbox, not just these -- including ones somebody else
            // marked. Refusing beats quietly deleting a stranger's mail.
            bail!(
                "the server advertises neither MOVE (RFC 6851) nor UIDPLUS (RFC 4315), \
                 so moving mail would have to end in a plain EXPUNGE -- which removes \
                 every message marked \\Deleted in '{}', not only these. Refusing",
                folder
            );
        }
        if self.debug {
            eprintln!(
                "Moving {} message(s) to '{}' via {}",
                uids.len(),
                to,
                if native { "UID MOVE" } else { "UID COPY + UID EXPUNGE" }
            );
        }
        const BATCH: usize = 50;
        for chunk in uids.chunks(BATCH) {
            let list = chunk
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            if native {
                self.session
                    .uid_mv(&list, to)
                    .with_context(|| format!("UID MOVE {} to '{}'", list, to))?;
                continue;
            }
            // Copy first: if this fails nothing has been marked, and if
            // a later step fails the messages exist in both places.
            self.session
                .uid_copy(&list, to)
                .with_context(|| format!("UID COPY {} to '{}'", list, to))?;
            self.store_one(folder, &list, '+', &["\\Deleted".to_string()])?;
            self.session
                .uid_expunge(&list)
                .with_context(|| {
                    format!(
                        "UID EXPUNGE {} in '{}' (the copies in '{}' are safe; the \
                         originals are still there, marked \\Deleted)",
                        list, folder, to
                    )
                })?;
        }
        Ok(())
    }

    fn copy_messages(&mut self, folder: &str, uids: &[u32], to: &str) -> Result<()> {
        self.session
            .select(folder)
            .with_context(|| format!("could not select '{}'", folder))?;
        if self.debug {
            eprintln!("Copying {} message(s) from '{}' to '{}' via UID COPY", uids.len(), folder, to);
        }
        const BATCH: usize = 50;
        for chunk in uids.chunks(BATCH) {
            let list = chunk
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            self.session
                .uid_copy(&list, to)
                .with_context(|| format!("UID COPY {} to '{}'", list, to))?;
        }
        Ok(())
    }

    fn expunge_messages(&mut self, folder: &str, uids: &[u32]) -> Result<Vec<u32>> {
        self.session
            .select(folder)
            .with_context(|| format!("could not select '{}'", folder))?;
        // A plain EXPUNGE removes every \Deleted message in the
        // mailbox, not only these -- including ones somebody else
        // marked. UID EXPUNGE (RFC 4315) is the only way to name which
        // ones, and it needs UIDPLUS.
        if !self.has_capability("UIDPLUS") {
            bail!(
                "the server does not advertise UIDPLUS (RFC 4315), so there is no UID \
                 EXPUNGE: a plain EXPUNGE would remove every message marked \\Deleted in \
                 '{}', not only these. Refusing",
                folder
            );
        }
        let eligible = self.deleted_among(folder, uids)?;
        if eligible.is_empty() {
            bail!(
                "none of the given message(s) are marked \\Deleted: 'expunge' only removes \
                 messages already marked for removal -- 'flag add <selection> deleted' is \
                 what marks them"
            );
        }
        if self.debug {
            eprintln!(
                "Expunging {} of {} given message(s) in '{}' (only the ones marked \\Deleted)",
                eligible.len(),
                uids.len(),
                folder
            );
        }
        const BATCH: usize = 50;
        for chunk in eligible.chunks(BATCH) {
            let list = chunk
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            self.session
                .uid_expunge(&list)
                .with_context(|| format!("UID EXPUNGE {} in '{}'", list, folder))?;
        }
        Ok(eligible)
    }

    fn append_message(
        &mut self,
        folder: &str,
        content: &[u8],
        flags: &[String],
        internal_date: Option<DateTime<FixedOffset>>,
    ) -> Result<Option<u32>> {
        // APPEND does not need the mailbox selected -- it names the
        // destination itself (RFC 3501 §6.3.11).
        if self.debug {
            eprintln!(
                "Appending {} byte(s) to '{}'{}{}",
                content.len(),
                folder,
                if flags.is_empty() {
                    String::new()
                } else {
                    format!(" with flags ({})", flags.join(" "))
                },
                match internal_date {
                    Some(d) => format!(", INTERNALDATE {}", d.to_rfc3339()),
                    None => String::new(),
                }
            );
        }
        let mut cmd = self.session.append(folder, content);
        if !flags.is_empty() {
            cmd.flags(flags.iter().map(|f| Flag::from(f.as_str())));
        }
        if let Some(date) = internal_date {
            cmd.internal_date(date);
        }
        let appended = match cmd.finish() {
            Ok(a) => a,
            // The mailbox does not exist: RFC 3501 has the server
            // answer NO with a TRYCREATE code rather than creating it.
            // 'append' does not create mailboxes, so this is where that
            // refusal turns into something naming the command that does.
            Err(imap::Error::No(no))
                if matches!(no.code, Some(imap_proto::types::ResponseCode::TryCreate)) =>
            {
                bail!(
                    "no mailbox '{}' to append into: 'append' does not create it -- \
                     'folder create {}' first",
                    folder,
                    folder
                );
            }
            Err(e) => {
                return Err(anyhow::Error::from(e).context(format!("APPEND to '{}' failed", folder)));
            }
        };
        Ok(match appended.uids.as_ref().and_then(|v| v.first()) {
            Some(imap_proto::types::UidSetMember::Uid(u)) => Some(*u),
            Some(imap_proto::types::UidSetMember::UidRange(r)) => Some(*r.start()),
            None => None,
        })
    }

    fn fetch_raw_message(&mut self, folder: &str, uid: u32) -> Result<RawMessage> {
        self.session.select(folder)?;
        let fetches = self
            .session
            .uid_fetch(uid.to_string(), "(UID FLAGS INTERNALDATE BODY.PEEK[])")
            .with_context(|| format!("UID FETCH of UID {} in '{}'", uid, folder))?;
        let f = fetches
            .iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no email with UID {} in folder '{}'", uid, folder))?;
        let bytes = f
            .body()
            .map(|b| b.to_vec())
            .ok_or_else(|| anyhow::anyhow!("UID {} in '{}' returned no body", uid, folder))?;
        Ok(RawMessage {
            bytes,
            // `\Recent` is the server's to set and is refused on the way
            // back in, so carrying it to the copy would only produce a
            // rejected APPEND.
            flags: f
                .flags()
                .iter()
                .filter(|fl| !matches!(fl, Flag::Recent))
                .map(|fl| fl.to_string())
                .collect(),
            internal_date: f.internal_date(),
        })
    }

    fn can_expunge_by_uid(&mut self) -> Result<bool> {
        Ok(self.has_capability("UIDPLUS"))
    }

    fn create_folder(&mut self, name: &str, use_attr: Option<&str>) -> Result<()> {
        match use_attr {
            None => self
                .session
                .create(name)
                .with_context(|| format!("CREATE '{}'", name)),
            Some(attr) => {
                // RFC 6154: the USE parameter is the only moment a client
                // may set a special-use attribute, and only when the
                // server says it takes one.
                if !self.has_capability("CREATE-SPECIAL-USE") {
                    bail!(
                        "the server does not advertise CREATE-SPECIAL-USE, so it will \
                         not take a special-use attribute at creation; create '{}' \
                         without --use",
                        name
                    );
                }
                self.session
                    .run_command_and_check_ok(format!(
                        "CREATE \"{}\" (USE ({}))",
                        name.replace('\\', "\\\\").replace('"', "\\\""),
                        attr
                    ))
                    .with_context(|| format!("CREATE '{}' (USE ({}))", name, attr))
            }
        }
    }

    fn rename_folder(&mut self, from: &str, to: &str) -> Result<()> {
        self.session
            .rename(from, to)
            .with_context(|| format!("RENAME '{}' to '{}'", from, to))
    }

    fn set_subscribed(&mut self, name: &str, subscribed: bool) -> Result<()> {
        if subscribed {
            self.session
                .subscribe(name)
                .with_context(|| format!("SUBSCRIBE '{}'", name))
        } else {
            self.session
                .unsubscribe(name)
                .with_context(|| format!("UNSUBSCRIBE '{}'", name))
        }
    }

    fn delete_folder(&mut self, name: &str, _force: bool) -> Result<()> {
        // Every check `force` governs (access level, INBOX, non-empty)
        // has already run in `ImapClient`; by the time this is reached
        // the answer is always "proceed".
        self.session
            .delete(name)
            .with_context(|| format!("DELETE '{}'", name))
    }

    fn message_flags(&mut self, folder: &str, uid: u32) -> Result<Vec<String>> {
        self.session
            .select(folder)
            .with_context(|| format!("could not select '{}'", folder))?;
        let uid_s = uid.to_string();
        let fetches = match self.attempt_fetch(folder, &uid_s, "(UID FLAGS)") {
            Attempt::Success(fs) => fs,
            Attempt::Unparseable => bail!(
                "could not fetch flags for UID {} in '{}': server response could not be parsed",
                uid,
                folder
            ),
            Attempt::Fatal(e) => return Err(e),
        };
        let f = fetches
            .iter()
            .find(|f| f.uid == Some(uid))
            .ok_or_else(|| anyhow::anyhow!("no email with UID {} in '{}'", uid, folder))?;
        Ok(f.flags()
            .iter()
            .filter(|fl| !matches!(fl, Flag::Recent))
            .map(|fl| fl.to_string())
            .collect())
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        // Do not send LOGOUT here. The `imap` crate v3.0 has a bug where
        // `logout()` can panic with a tag mismatch assertion failure after many
        // commands (more mails / higher `-M`). Since this is a read-only tool
        // and the connection is being closed anyway, just drop the session and
        // let the server close the connection when the TCP stream is dropped.
    }
}

impl Drop for RealClient {
    fn drop(&mut self) {
        self.close();
    }
}

/// Count the leaf MIME parts of a server-reported body structure.
/// `message/rfc822` and all non-multipart parts count as a single part,
/// consistent with the local MIME parser used by `parts list`.
fn count_leaf_parts(bs: &imap_proto::types::BodyStructure) -> u32 {
    match bs {
        imap_proto::types::BodyStructure::Multipart { bodies, .. } => {
            bodies.iter().map(count_leaf_parts).sum()
        }
        _ => 1,
    }
}

/// Wire text of an `imap_proto` capability atom.
fn capability_to_string(c: &imap_proto::Capability<'_>) -> String {
    match c {
        imap_proto::Capability::Imap4rev1 => "IMAP4rev1".to_string(),
        imap_proto::Capability::Auth(mech) => format!("AUTH={}", mech),
        imap_proto::Capability::Atom(a) => a.to_string(),
    }
}

fn bytes_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
}

/// First value of header `name` (case-insensitive) in a raw — possibly
/// 8-bit — header block, with folded continuation lines unfolded.
fn header_value(raw: &[u8], name: &[u8]) -> Option<Vec<u8>> {
    let lname = name.to_ascii_lowercase();
    let mut out: Option<Vec<u8>> = None;
    for line in raw.split(|b| *b == b'\n') {
        let line = if line.ends_with(b"\r") {
            &line[..line.len() - 1]
        } else {
            line
        };
        if line.is_empty() {
            break; // end of the header block
        }
        if line[0] == b' ' || line[0] == b'\t' {
            if let Some(v) = out.as_mut() {
                v.push(b' ');
                v.extend(line.iter().copied().skip_while(|b| *b == b' ' || *b == b'\t'));
            }
            continue;
        }
        if out.is_some() {
            break; // scanning past the header we were looking for
        }
        if let Some(pos) = line.iter().position(|b| *b == b':') {
            if line[..pos].to_ascii_lowercase() == lname {
                out = Some(
                    line[pos + 1..]
                        .iter()
                        .copied()
                        .skip_while(|b| *b == b' ' || *b == b'\t')
                        .collect(),
                );
            }
        }
    }
    out
}

/// Extract the first e-mail address from a raw From header value
/// (`"Name" <a@b>`, `a@b`, or `a@b, c@d`).
fn address_from_header(raw: &[u8]) -> String {
    let s = bytes_to_string(raw);
    if let Some(start) = s.find('<') {
        let rest = &s[start + 1..];
        if let Some(end) = rest.find('>') {
            return rest[..end].trim().to_string();
        }
    }
    s.split(['(', ','])
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Build a message's threading headers from a raw
/// `BODY[HEADER.FIELDS (MESSAGE-ID REFERENCES IN-REPLY-TO)]` block.
fn thread_refs_of_fetch(uid: u32, raw: &[u8]) -> ThreadRefs {
    let message_ids = header_value(raw, b"Message-ID")
        .map(|v| extract_message_ids(&v))
        .unwrap_or_default();
    let mut references = Vec::new();
    for name in [
        b"References".as_slice(),
        b"In-Reply-To".as_slice(),
    ] {
        if let Some(v) = header_value(raw, name) {
            references.extend(extract_message_ids(&v));
        }
    }
    ThreadRefs {
        uid,
        message_ids,
        references,
    }
}

/// Collect every `<...>` Message-ID token in a header value (References
/// is a space-separated list of them). A value without angle brackets is
/// kept as a single bare token when non-empty.
fn extract_message_ids(v: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = v;
    while let Some(start) = rest.iter().position(|b| *b == b'<') {
        let after = &rest[start + 1..];
        match after.iter().position(|b| *b == b'>') {
            Some(end) => {
                out.push(format!("<{}>", String::from_utf8_lossy(&after[..end]).trim()));
                rest = &after[end + 1..];
            }
            None => break, // malformed tail; stop here
        }
    }
    if out.is_empty() {
        let t = bytes_to_string(v);
        let t = t.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
    }
    out
}

/// Decode RFC 2047 encoded-words wherever they appear in a header value
/// (e.g. `=?utf-8?Q?Votre=20facture?=`). Plain (unencoded) text is kept as-is.
/// ENVELOPE already returns the unfolded logical value, so no de-folding is done.
fn decode_rfc2047(input: String) -> String {
    if !input.contains("=?") {
        return input;
    }
    let s = input.as_str();
    let mut out = String::new();
    let mut i = 0usize;
    while i < s.len() {
        if s[i..].starts_with("=?") {
            if let Some((decoded, consumed)) = try_decode_encoded_word(&s[i..]) {
                out.push_str(&decoded);
                i += consumed;
                continue;
            }
        }
        let ch_len = s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Attempt to parse an RFC 2047 encoded word of the form `=?charset?enc?data?=`
/// at the start of `s`. Returns the decoded value and the number of bytes consumed.
fn try_decode_encoded_word(s: &str) -> Option<(String, usize)> {
    // Layout: =?charset?enc?data?=
    let after_prefix = s.strip_prefix("=?")?; // charset?enc?data?=
    let q1 = after_prefix.find('?')?;
    let charset = &after_prefix[..q1];
    if charset.is_empty() || q1 == 0 {
        return None;
    }
    let after_q1 = &after_prefix[q1 + 1..]; // enc?data?=
    let enc = *after_q1.as_bytes().first()?;
    if !matches!(enc, b'B' | b'b' | b'Q' | b'q') {
        return None;
    }
    let after_enc = after_q1.get(1..)?; // ?data?=
    if !after_enc.starts_with('?') {
        return None;
    }
    let data_and_term = after_enc.get(1..)?; // data?=
    let q2 = data_and_term.find('?')?;
    let data = &data_and_term[..q2];
    let tail = &data_and_term[q2 + 1..];
    if !tail.starts_with('=') || data.contains('?') {
        return None;
    }
    // =?(2) + charset(q1) + ?(1) + enc(1) + ?(1) + data + ?(1) + =(1)
    let word_len = 2 + q1 + 1 + 1 + 1 + data.len() + 1 + 1;
    let decoded = decode_data(data, enc, charset)?;
    Some((decoded, word_len))
}

fn decode_data(data: &str, enc: u8, charset: &str) -> Option<String> {
    let bytes: Vec<u8> = match enc {
        b'B' | b'b' => {
            let trimmed: String = data.chars().filter(|c| !c.is_whitespace()).collect();
            let pad = (4 - (trimmed.len() % 4)) % 4;
            let padded = format!("{}{}", trimmed, "=".repeat(pad));
            base64::decode(padded).ok()?
        }
        b'Q' | b'q' => {
            let q = data.replace('_', " ");
            let mut buf = Vec::new();
            let b = q.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'=' && i + 2 < b.len() {
                    let hi = (b[i + 1] as char).to_digit(16)?;
                    let lo = (b[i + 2] as char).to_digit(16)?;
                    buf.push((hi * 16 + lo) as u8);
                    i += 3;
                } else {
                    buf.push(b[i]);
                    i += 1;
                }
            }
            buf
        }
        _ => return None,
    };
    Some(decode_bytes(bytes, charset))
}

fn decode_bytes(bytes: Vec<u8>, charset: &str) -> String {
    match charset.to_lowercase().as_str() {
        "utf-8" | "utf8" => String::from_utf8_lossy(&bytes).to_string(),
        "iso-8859-1" | "latin1" | "latin-1" | "windows-1252" => bytes
            .iter()
            .map(|b| *b as char)
            .collect::<String>(),
        _ => String::from_utf8_lossy(&bytes).to_string(),
    }
}

fn format_address(addr: &imap_proto::types::Address) -> String {
    let mailbox: String = addr
        .mailbox
        .as_ref()
        .map(|s| bytes_to_string(s))
        .filter(|m: &String| !m.is_empty())
        .unwrap_or_default();
    let host: String = addr
        .host
        .as_ref()
        .map(|s| bytes_to_string(s))
        .filter(|h: &String| !h.is_empty())
        .unwrap_or_default();
    if host.is_empty() {
        mailbox
    } else {
        format!("{}@{}", mailbox, host)
    }
}

#[cfg(test)]
mod tests {
    use super::{address_from_header, decode_rfc2047, header_value, RealClient};

    #[test]
    fn header_value_basic_case_insensitive() {
        let raw = b"Subject: =?utf-8?Q?caf=E9?=\r\nFrom: A User <a@example.com>\r\n\r\nbody";
        assert_eq!(
            header_value(raw, b"subject").unwrap(),
            b"=?utf-8?Q?caf=E9?="
        );
        assert!(header_value(raw, b"X-Missing").is_none());
    }

    #[test]
    fn header_value_unfolds_continuations() {
        let raw = b"Subject: first\r\n second\r\n\tthird\r\nTo: x@y.z\r\n";
        assert_eq!(header_value(raw, b"Subject").unwrap(), b"first second third");
    }

    #[test]
    fn header_value_keeps_raw_8bit_bytes() {
        // Exactly the kind of bytes that break imap-proto's ENVELOPE
        // parser and would abort the whole search with a fake "Bye".
        let raw = b"Subject: Votre facture \xe9!\r\nFrom: a@b.c\r\n";
        assert_eq!(
            header_value(raw, b"Subject").unwrap(),
            b"Votre facture \xe9!"
        );
    }

    #[test]
    fn address_from_header_forms() {
        assert_eq!(
            address_from_header(b"A User <a@example.com>"),
            "a@example.com"
        );
        assert_eq!(address_from_header(b"plain@example.com"), "plain@example.com");
        assert_eq!(address_from_header(b"a@x.y, b@z.w"), "a@x.y");
        assert_eq!(
            address_from_header(b"Jean Dupont <jd@example.fr> (work)"),
            "jd@example.fr"
        );
    }

    #[test]
    fn plain_string_unchanged() {
        assert_eq!(decode_rfc2047("Hello there".into()), "Hello there");
    }

    #[test]
    fn q_encoding_utf8() {
        assert_eq!(
            decode_rfc2047("=?utf-8?Q?Votre=20facture?=".into()),
            "Votre facture"
        );
    }

    #[test]
    fn q_encoding_with_underscore() {
        // In Q encoding, '_' means a space.
        assert_eq!(
            decode_rfc2047("=?utf-8?Q?bonjour_le_monde?=".into()),
            "bonjour le monde"
        );
    }

    #[test]
    fn b_encoding_utf8() {
        // "Renouvellement" base64-encoded.
        let b64 = base64::encode("Renouvellement".as_bytes());
        let word = format!("=?utf-8?B?{}?=", b64);
        assert_eq!(decode_rfc2047(word), "Renouvellement");
    }

    #[test]
    fn latin1_charset() {
        // é is 0xE9 in iso-8859-1, so Q-encode it as =E9.
        assert_eq!(
            decode_rfc2047("=?iso-8859-1?Q?caf=E9?=".into()),
            "café"
        );
    }

    #[test]
    fn mixed_text_and_encoded_word() {
        assert_eq!(
            decode_rfc2047("Re: =?utf-8?Q?Votre=20facture?=".into()),
            "Re: Votre facture"
        );
    }

    #[test]
    fn charset_prefixed_leaves_ascii_query_on_the_wire_unchanged() {
        assert_eq!(RealClient::charset_prefixed("SUBJECT hello"), None);
    }

    #[test]
    fn charset_prefixed_declares_utf8_for_a_non_ascii_query() {
        assert_eq!(
            RealClient::charset_prefixed("SUBJECT caf\u{e9}"),
            Some("CHARSET UTF-8 SUBJECT café".to_string())
        );
    }
}
