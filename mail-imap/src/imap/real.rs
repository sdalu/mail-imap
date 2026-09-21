use crate::config::Config;
use crate::imap::mime;
use crate::imap::{PartInfo, FolderInfo, ImapBackend, Mailbox, SearchResult};
use anyhow::{bail, Context, Result};
use imap::types::NameAttribute;
use imap::Session;
use native_tls::TlsConnector;
use std::collections::BTreeSet;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

type TlsSession = Session<native_tls::TlsStream<TcpStream>>;
type PlainSession = Session<TcpStream>;

enum Backend {
    Tls(TlsSession),
    Plain(PlainSession),
}

// Runs a block of code against whichever IMAP session the client holds.
// The body must only return owned data (the imap crate's zero-copy
// results borrow from the session).
macro_rules! with_backend {
    ($backend:expr, |$sess:ident| $body:expr) => {{
        let r: Result<_> = match $backend {
            Backend::Tls($sess) => $body,
            Backend::Plain($sess) => $body,
        };
        r
    }};
}

pub struct RealClient {
    backend: Backend,
    /// Guards against double logout: `ImapClient`'s Drop and `RealClient`'s
    /// own Drop both call `close()`, and a second `LOGOUT` on the
    /// server-closed connection would fail with `ConnectionLost`.
    closed: bool,
}

impl RealClient {
    pub fn connect(config: &Config) -> Result<Self> {
        let addr = (config.server.as_str(), config.port);
        let socket_addr = addr
            .to_socket_addrs()
            .with_context(|| format!("could not resolve {}", config.server))?
            .next()
            .with_context(|| format!("no addresses found for {}", config.server))?;
        let tcp = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(10))
            .with_context(|| format!("could not connect to {} at {}", config.server, socket_addr))?;
        tcp.set_nodelay(true)?;

        let connector = TlsConnector::builder()
            .danger_accept_invalid_certs(config.insecure)
            .danger_accept_invalid_hostnames(config.insecure)
            .build()?;

        let backend = if config.ssl {
            let tls_stream = TlsConnector::connect(&connector, &config.server, tcp)
                .with_context(|| format!("TLS handshake with {} failed", config.server))?;
            Backend::Tls(login(imap::Client::new(tls_stream), config)?)
        } else if config.starttls {
            let client = imap::Client::new(tcp);
            let client = client
                .secure(&config.server, &connector)
                .with_context(|| format!("STARTTLS upgrade with {} failed", config.server))?;
            Backend::Tls(login(client, config)?)
        } else {
            Backend::Plain(login(imap::Client::new(tcp), config)?)
        };

        Ok(RealClient {
            backend,
            closed: false,
        })
    }

    /// Fetch the raw RFC822 bytes of a message without setting `\Seen`
    /// (`BODY.PEEK[]`).
    fn fetch_raw(&mut self, folder: &str, uid: u32) -> Result<Vec<u8>> {
        with_backend!(&mut self.backend, |s| {
            s.select(folder)?;
            let fetches = s
                .uid_fetch(uid.to_string(), "(UID BODY.PEEK[RFC822])")
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
        })
    }

    /// Search one selected mailbox: `UID SEARCH`, then a batched fetch of
    /// the metadata for the most-recent UIDs, capped at `cap`.
    fn search_in_folder(
        &mut self,
        folder: &str,
        query: &str,
        cap: usize,
    ) -> Result<Vec<SearchResult>> {
        with_backend!(&mut self.backend, |s| {
            s.select(folder)?;
            let mut uids: Vec<u32> = s
                .uid_search(query)?
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if uids.is_empty() {
                return Ok(Vec::new());
            }

            // Show most-recent first (UIDs increase over time) and cap the count.
            uids.reverse();
            if cap > 0 {
                uids.truncate(cap);
            }

            let mut results: Vec<SearchResult> = Vec::new();
            const BATCH: usize = 25;
            for chunk in uids.chunks(BATCH) {
                let set: Vec<String> = chunk.iter().map(|u| u.to_string()).collect();
                let list = set.join(",");
                // BODYSTRUCTURE gives the MIME tree without transferring any
                // content, so the part count is cheap to fetch in a batch.
                // Some servers emit body structures this crate cannot parse,
                // which would abort the whole batch; in that case retry the
                // batch without it so the search still works (the affected
                // messages simply report 0 parts).
                let fetches = match s
                    .uid_fetch(
                        &list,
                        "(UID ENVELOPE FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE)",
                    ) {
                    Ok(f) => f,
                    Err(imap::Error::Parse(_)) => s.uid_fetch(
                        &list,
                        "(UID ENVELOPE FLAGS INTERNALDATE RFC822.SIZE)",
                    )?,
                    Err(e) => return Err(e.into()),
                };
                for f in fetches.iter() {
                    let env = f.envelope();
                    let subject = env
                        .and_then(|e| e.subject)
                        .map(bytes_to_string)
                        .map(decode_rfc2047)
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
                        .filter(|fl| !matches!(fl, imap::types::Flag::Recent))
                        .map(|fl| fl.to_string())
                        .collect();
                    results.push(SearchResult {
                        uid: f.uid.unwrap_or(0),
                        folder: folder.to_string(),
                        subject,
                        from,
                        date,
                        size: f.size,
                        flags,
                        parts: f.bodystructure().map(count_leaf_parts).unwrap_or(0),
                    });
                }
            }
            Ok(results)
        })
    }
}

impl ImapBackend for RealClient {
    fn list_folders(&mut self) -> Result<Vec<FolderInfo>> {
        with_backend!(&mut self.backend, |s| {
            let names = s
                .list(None, Some("*"))?
                .into_iter()
                .filter(|n| !n.attributes().contains(&NameAttribute::NoSelect))
                .map(|n| {
                    let mut attrs = Vec::new();
                    if n.attributes().contains(&NameAttribute::Marked) {
                        attrs.push("\\Marked".to_string());
                    }
                    FolderInfo {
                        name: n.name().to_string(),
                        delimiter: n.delimiter().map(str::to_string),
                        no_inferiors: n
                            .attributes()
                            .contains(&NameAttribute::NoInferiors),
                        attrs,
                    }
                })
                .collect();
            Ok(names)
        })
    }

    fn search_folders(
        &mut self,
        folders: &[String],
        query: &str,
        max_results: usize,
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
            out.extend(self.search_in_folder(folder, query, cap)?);
        }
        Ok(out)
    }

    fn get_email(&mut self, folder: &str, uid: u32) -> Result<String> {
        with_backend!(&mut self.backend, |s| {
            s.select(folder)?;
            // BODY.PEEK[] keeps the server from setting \Seen on the message.
            let fetches = s.uid_fetch(
                uid.to_string(),
                "(UID ENVELOPE FLAGS INTERNALDATE BODY.PEEK[RFC822])",
            )?;
            let fetches: Vec<&imap::types::Fetch> = fetches.iter().collect();
            if fetches.is_empty() {
                bail!("no email with UID {} in folder '{}'", uid, folder);
            }
            let f = &fetches[0];
            let mut out = String::new();

            if let Some(env) = f.envelope() {
                if let Some(subject) = env.subject {
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
                if let Some(date) = env.date {
                    out.push_str(&format!("Date: {}\n", bytes_to_string(date)));
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
        })
    }

    fn mailbox_counts(&mut self, folder: Option<&str>) -> Result<Vec<Mailbox>> {
        with_backend!(&mut self.backend, |s| {
            let names: Vec<String> = match folder {
                Some(f) => vec![f.to_string()],
                None => s
                    .list(None, Some("*"))?
                    .into_iter()
                    .filter(|n| !n.attributes().contains(&NameAttribute::NoSelect))
                    .map(|n| n.name().to_string())
                    .collect(),
            };
            let mut out = Vec::new();
            for name in names {
                let m = s
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
        })
    }

    fn folder_uids(&mut self, folder: &str) -> Result<Vec<u32>> {
        with_backend!(&mut self.backend, |s| {
            s.select(folder)?;
            // `ALL` is the standard key for "every message in the mailbox".
            let uids: Vec<u32> = s.uid_search("ALL")?.into_iter().collect();
            Ok(uids)
        })
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

    fn save_part(
        &mut self,
        folder: &str,
        uid: u32,
        part: u32,
        dest: &Path,
    ) -> Result<u64> {
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
        let data = leaf.decoded()?;
        std::fs::write(dest, &data)
            .with_context(|| format!("writing part to {}", dest.display()))?;
        Ok(data.len() as u64)
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let result = match &mut self.backend {
            Backend::Tls(s) => s.logout(),
            Backend::Plain(s) => s.logout(),
        };
        if let Err(e) = result {
            // The connection was already gone (server closed it, network
            // drop): there is nothing left to log out, and any real command
            // failure has already been reported. Only server-side
            // rejections (BAD/NO) are worth a warning.
            if matches!(e, imap::Error::ConnectionLost) {
                return;
            }
            eprintln!("warning: IMAP logout failed: {}", e);
        }
    }
}

impl Drop for RealClient {
    fn drop(&mut self) {
        self.close();
    }
}

fn login<S>(client: imap::Client<S>, config: &Config) -> Result<Session<S>>
where
    S: std::io::Read + std::io::Write,
{
    let mut client = client;
    client.read_greeting().context("could not read server greeting")?;
    client
        .login(&config.username, &config.password)
        .map_err(|(e, _)| e)
        .with_context(|| format!("login as '{}' failed (check credentials / server)", config.username))
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

fn bytes_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
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
        .map(bytes_to_string)
        .filter(|m: &String| !m.is_empty())
        .unwrap_or_default();
    let host: String = addr
        .host
        .map(bytes_to_string)
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
    use super::decode_rfc2047;

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
}
