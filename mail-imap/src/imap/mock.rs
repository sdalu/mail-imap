use crate::config::Config;
use crate::imap::{
    Permanent,
    sort_results, thread_component, FolderInfo, ImapBackend, Mailbox, PartInfo, RawMessage,
    SearchResult, SortCriteria, ThreadRefs,
};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, FixedOffset};
use std::collections::{BTreeMap, BTreeSet};

/// In-memory mock backend. This is the original mockup, kept for offline
/// testing so the tool can be exercised without a reachable IMAP server.
pub struct MockClient {
    /// Folders that exist in the mock mailbox.
    folders: Vec<String>,
    /// uid -> (subject, from, body)
    messages: Vec<(u32, String, String, String)>,
    /// uid -> (Message-IDs, referenced Message-IDs) used by `thread`.
    thread_ids: Vec<(u32, Vec<String>, Vec<String>)>,
    /// uid -> flags/keywords, maintained by `store_flags` (flag/tag).
    message_flags: BTreeMap<u32, BTreeSet<String>>,
    /// Mailboxes `set_subscribed` has been told about.
    subscribed: BTreeSet<String>,
    /// (folder, uid) pairs `move_messages` has filed elsewhere; they
    /// stop being listed by `folder_uids` for that folder.
    moved: BTreeSet<(String, u32)>,
}

impl MockClient {
    pub fn connect(_config: &Config) -> Result<Self> {
        Ok(MockClient {
            folders: vec![
                "INBOX".to_string(),
                "Sent Items".to_string(),
                "Drafts".to_string(),
                "Trash".to_string(),
                "Spam".to_string(),
            ],
            messages: vec![
                (1, "Welcome aboard".into(), "alice@example.com".into(), "Hello, this is message 1.".into()),
                (2, "Meeting notes".into(), "bob@example.com".into(), "Please review the notes.".into()),
                (3, "Quarterly report".into(), "carol@example.com".into(), "Attached: Q3 numbers.".into()),
                (4, "Lunch?".into(), "dave@example.com".into(), "Pizza at noon?".into()),
                (5, "Your invoice".into(), "billing@example.com".into(), "Invoice #42 is due.".into()),
            ],
            // 4 replies to 2; 5 replies to 3, referencing 1 as well, so
            // {1, 3, 5} form one thread and {2, 4} another; 1..3 stand
            // alone apart from those links.
            thread_ids: vec![
                (1, vec!["<m1@mail>".into()], vec![]),
                (2, vec!["<m2@mail>".into()], vec![]),
                (3, vec!["<m3@mail>".into()], vec!["<m1@mail>".into()]),
                (4, vec!["<m4@mail>".into()], vec!["<m2@mail>".into()]),
                (5, vec!["<m5@mail>".into()], vec!["<m3@mail>".into()]),
            ],
            message_flags: BTreeMap::new(),
            // Seeded rather than empty, because an account is
            // subscribed to its own folders and an empty set makes
            // `folder list --subscribed` structurally incapable of
            // showing anything here -- a demo of a feature that can
            // only ever print "none". Spam is left out: a real account
            // commonly is not subscribed to it, and a set that differs
            // from the folder list is the only one that proves the
            // command is filtering rather than listing.
            subscribed: ["INBOX", "Sent Items", "Drafts", "Trash"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            moved: BTreeSet::new(),
        })
    }

    /// Does this mailbox exist?
    ///
    /// A real backend selects the folder before it does anything with
    /// it, so naming one that is not there is an error. The mock used
    /// to ignore the folder argument entirely and answer anyway, which
    /// made `-f Nonexistent` succeed here and fail on the wire.
    fn require_folder(&self, folder: &str) -> Result<()> {
        if !self.folders.iter().any(|f| f == folder) {
            bail!("no mailbox '{}' (mock)", folder);
        }
        Ok(())
    }

    /// Fixed part metadata: message 3 carries a spreadsheet,
    /// message 5 a PDF. Every message also has its plain-text body as
    /// part 1.
    fn parts(&self, uid: u32) -> Vec<PartInfo> {
        let mut out = Vec::new();
        if let Some((_, _, _, body)) = self.messages.iter().find(|(u, _, _, _)| *u == uid) {
            out.push(PartInfo {
                part: 1,
                content_type: "text/plain".to_string(),
                filename: None,
                size: body.len() as u64,
            });
        }
        match uid {
            3 => out.push(PartInfo {
                part: 2,
                content_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                    .to_string(),
                filename: Some("q3-numbers.xlsx".to_string()),
                size: 20480,
            }),
            5 => out.push(PartInfo {
                part: 2,
                content_type: "application/pdf".to_string(),
                filename: Some("invoice-42.pdf".to_string()),
                size: 51200,
            }),
            _ => {}
        }
        out
    }

    fn search_in_folder(
        &mut self,
        folder: &str,
        query: &str,
        cap: usize,
        sort: Option<&SortCriteria>,
    ) -> Result<Vec<SearchResult>> {
        self.require_folder(folder)?;
        let raw = query.trim().to_lowercase();
        // The mock carries no flags and has no notion of mailbox content,
        // so the standard "match everything" keys ALL and UNSEEN are
        // treated as an empty (match-all) filter.
        let q = if matches!(raw.as_str(), "all" | "unseen") {
            String::new()
        } else {
            raw
        };
        let mut results: Vec<SearchResult> = self
            .messages
            .iter()
            .filter(|(_, subject, from, _)| {
                q.is_empty()
                    || subject.to_lowercase().contains(&q)
                    || from.to_lowercase().contains(&q)
            })
            .map(|(uid, subject, from, _)| SearchResult {
                uid: *uid,
                folder: folder.to_string(),
                subject: subject.clone(),
                from: from.clone(),
                date: Some("2026-09-20 12:00:00 +0000".to_string()),
                size: Some(120),
                flags: self
                    .message_flags
                    .get(uid)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default(),
                // Matches `parts()`: uids 3 and 5 carry an extra part.
                parts: self.parts(*uid).len() as u32,
            })
            .collect();
        if let Some(spec) = sort {
            sort_results(&mut results, spec);
        }
        results.truncate(cap);
        Ok(results)
    }
}

impl ImapBackend for MockClient {
    /// The mock advertises nothing, which is the point: it stands in
    /// for the barest server there is, so every degradation ladder is
    /// exercised offline.
    fn capabilities(&mut self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn list_folders(&mut self) -> Result<Vec<FolderInfo>> {
        Ok(self
            .folders
            .iter()
            .map(|name| FolderInfo {
                name: name.clone(),
                delimiter: Some("/".to_string()),
                no_inferiors: name == "INBOX",
                // RFC 6154 special uses, as an account that has them
                // would report them.
                attrs: match name.as_str() {
                    "Sent Items" => vec!["\\Sent".to_string()],
                    "Drafts" => vec!["\\Drafts".to_string()],
                    "Trash" => vec!["\\Trash".to_string()],
                    "Spam" => vec!["\\Junk".to_string()],
                    _ => Vec::new(),
                },
            })
            .collect())
    }

    fn list_subscribed_folders(&mut self) -> Result<Vec<FolderInfo>> {
        Ok(self
            .list_folders()?
            .into_iter()
            .filter(|f| self.subscribed.contains(&f.name))
            .collect())
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
        self.require_folder(folder)?;
        let (_, subject, from, body) = self
            .messages
            .iter()
            .find(|(u, _, _, _)| *u == uid)
            .ok_or_else(|| anyhow::anyhow!("no email with UID {} (mock)", uid))?;
        Ok(format!(
            "Subject: {}\nFrom: {}\nDate: 2026-09-20 12:00:00 +0000\n\n{}\n",
            subject, from, body
        ))
    }

    fn mailbox_counts(&mut self, folder: Option<&str>) -> Result<Vec<Mailbox>> {
        let inbox_messages = self.messages.len() as u32;
        let counts: Vec<Mailbox> = self
            .folders
            .iter()
            .map(|name| {
                let is_inbox = name == "INBOX";
                Mailbox {
                    name: name.clone(),
                    messages: if is_inbox { inbox_messages } else { 0 },
                    unseen: if is_inbox { inbox_messages } else { 0 },
                    recent: 0,
                    uid_next: if is_inbox { inbox_messages + 1 } else { 1 },
                    uid_validity: 1,
                }
            })
            .collect();
        match folder {
            Some(f) => counts
                .into_iter()
                .find(|m| m.name == f)
                .ok_or_else(|| anyhow::anyhow!("no such folder '{}' (mock)", f))
                .map(|m| vec![m]),
            None => Ok(counts),
        }
    }

    fn folder_uids(&mut self, folder: &str) -> Result<Vec<u32>> {
        self.require_folder(folder)?;
        Ok(self
            .messages
            .iter()
            .map(|(u, _, _, _)| *u)
            .filter(|u| !self.moved.contains(&(folder.to_string(), *u)))
            .collect())
    }

    fn thread_uids(&mut self, folder: &str, uid: u32) -> Result<Vec<u32>> {
        self.require_folder(folder)?;
        if !self.messages.iter().any(|(u, _, _, _)| *u == uid) {
            bail!("no email with UID {} (mock)", uid);
        }
        let msgs: Vec<ThreadRefs> = self
            .thread_ids
            .iter()
            .map(|(u, ids, refs)| ThreadRefs {
                uid: *u,
                message_ids: ids.clone(),
                references: refs.clone(),
            })
            .collect();
        thread_component(uid, &msgs)
            .with_context(|| format!("threading UID {} in '{}'", uid, folder))
    }

    fn list_parts(&mut self, folder: &str, uid: u32) -> Result<Vec<PartInfo>> {
        self.require_folder(folder)?;
        if !self.messages.iter().any(|(u, _, _, _)| *u == uid) {
            bail!("no email with UID {} (mock)", uid);
        }
        Ok(self.parts(uid))
    }

    fn fetch_part(&mut self, folder: &str, uid: u32, part: u32) -> Result<Vec<u8>> {
        let parts = self.list_parts(folder, uid)?;
        let max_part = parts.iter().map(|p| p.part).max().unwrap_or(0);
        if part == 0 || part > max_part {
            bail!(
                "no part {} in message UID {} (mock has {} part(s))",
                part,
                uid,
                parts.len()
            );
        }
        // Produce bytes whose length matches the part size reported by
        // `list_parts`.
        let data: Vec<u8> = if part == 1 {
            self.messages
                .iter()
                .find(|(u, _, _, _)| *u == uid)
                .map(|(_, _, _, body)| body.as_bytes().to_vec())
                .unwrap_or_default()
        } else {
            let declared = parts
                .iter()
                .find(|p| p.part == part)
                .map(|p| p.size as usize)
                .unwrap_or(0);
            let mut data = format!("mock attachment data (uid {} part {})\n", uid, part)
                .into_bytes();
            data.resize(declared, 0);
            data
        };
        Ok(data)
    }

    fn store_flags(
        &mut self,
        folder: &str,
        uids: &[u32],
        add: &[String],
        remove: &[String],
    ) -> Result<()> {
        self.require_folder(folder)?;
        // RFC 3501 6.4.8: `UID STORE` ignores a UID that does not
        // exist, without an error -- measured against a real server,
        // which answers OK. Refusing here instead made the mock
        // stricter than the thing it stands in for, and that is not a
        // safe direction to be wrong in: it hid a real defect, where
        // `tag junk` on a missing UID reported success on the wire and
        // could not be reproduced offline.
        for uid in uids {
            if !self.messages.iter().any(|(u, _, _, _)| u == uid) {
                continue;
            }
            let set = self.message_flags.entry(*uid).or_default();
            for r in remove {
                set.remove(r);
            }
            for a in add {
                set.insert(a.clone());
            }
        }
        Ok(())
    }

    fn permanent_flags(&mut self, folder: &str) -> Result<Permanent> {
        self.require_folder(folder)?;
        // The mock keeps whatever it is told, and says so.
        Ok(Permanent {
            any_keyword: true,
            ..Permanent::default()
        })
    }

    fn move_messages(&mut self, folder: &str, uids: &[u32], to: &str) -> Result<()> {
        self.require_folder(folder)?;
        if !self.folders.iter().any(|f| f == to) {
            bail!("no mailbox '{}' to file into", to);
        }
        // As with `store_flags`: a UID that is not there is ignored,
        // which is what a real `UID MOVE` does (measured).
        for uid in uids {
            if !self.messages.iter().any(|(u, _, _, _)| u == uid) {
                continue;
            }
            self.moved.insert((folder.to_string(), *uid));
        }
        Ok(())
    }

    fn copy_messages(&mut self, folder: &str, _uids: &[u32], to: &str) -> Result<()> {
        self.require_folder(folder)?;
        if !self.folders.iter().any(|f| f == to) {
            bail!("no mailbox '{}' to file into", to);
        }
        // Nothing else to track: a message the mock knows about is
        // already visible from every folder it has not been `moved`
        // out of (see `folder_uids`), so filing a copy into `to`
        // changes nothing that read is not already showing. A UID that
        // is not there is ignored, as with `store_flags`/`move_messages`.
        Ok(())
    }

    fn expunge_messages(&mut self, folder: &str, uids: &[u32]) -> Result<Vec<u32>> {
        self.require_folder(folder)?;
        // No UIDPLUS gate here: `capabilities()` advertises nothing
        // (that is the point of it -- see its own doc comment), and
        // refusing for want of a capability the mock never claims would
        // make it refuse every expunge, which is *stricter* than the
        // real server it stands in for -- the direction CLAUDE.md
        // records as the dangerous one.
        let eligible: Vec<u32> = uids
            .iter()
            .copied()
            .filter(|uid| {
                self.message_flags
                    .get(uid)
                    .map(|flags| flags.iter().any(|f| f.eq_ignore_ascii_case("\\Deleted")))
                    .unwrap_or(false)
            })
            .collect();
        if eligible.is_empty() {
            bail!(
                "none of the given message(s) are marked \\Deleted (mock): 'expunge' only \
                 removes messages already marked for removal -- 'flag add <selection> \
                 deleted' is what marks them"
            );
        }
        self.messages.retain(|(u, _, _, _)| !eligible.contains(u));
        self.thread_ids.retain(|(u, _, _)| !eligible.contains(u));
        self.message_flags.retain(|u, _| !eligible.contains(u));
        Ok(eligible)
    }

    fn append_message(
        &mut self,
        folder: &str,
        content: &[u8],
        flags: &[String],
        // Not stored: the mock has nowhere to keep it and nothing reads
        // INTERNALDATE back from it. Taking the parameter (rather than
        // refusing it) is what matters -- a date a real server accepts
        // must not be refused here, which would make the mock stricter
        // than the thing it stands in for.
        _internal_date: Option<DateTime<FixedOffset>>,
    ) -> Result<Option<u32>> {
        if !self.folders.iter().any(|f| f == folder) {
            // Mirrors the real backend's NO [TRYCREATE] translation --
            // 'append' does not create the mailbox on either backend.
            bail!(
                "no mailbox '{}' to append into: 'append' does not create it -- 'folder \
                 create {}' first (mock)",
                folder,
                folder
            );
        }
        // A demo aid, not a MIME parser: pull Subject/From out of the
        // header block by hand and keep everything after the blank
        // line as the body, which is all `search`/`get_email` need.
        let text = String::from_utf8_lossy(content);
        let mut subject = "(no subject)".to_string();
        let mut from = "(unknown)".to_string();
        let mut body = String::new();
        let mut in_body = false;
        for line in text.split("\r\n") {
            if in_body {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(line);
                continue;
            }
            if line.is_empty() {
                in_body = true;
                continue;
            }
            if let Some(v) = line.strip_prefix("Subject:") {
                subject = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("From:") {
                from = v.trim().to_string();
            }
        }
        let uid = self.messages.iter().map(|(u, _, _, _)| *u).max().unwrap_or(0) + 1;
        self.messages.push((uid, subject, from, body));
        if !flags.is_empty() {
            self.message_flags
                .insert(uid, flags.iter().cloned().collect());
        }
        // No UIDPLUS gate: `capabilities()` advertises nothing here
        // (see its own doc comment), but reporting a UID unconditionally
        // is the *permissive* direction -- CLAUDE.md's rule is that the
        // mock must never be stricter than a real server, not that it
        // must match every detail of one.
        Ok(Some(uid))
    }

    fn fetch_raw_message(&mut self, folder: &str, uid: u32) -> Result<RawMessage> {
        self.require_folder(folder)?;
        let (_, subject, from, body) = self
            .messages
            .iter()
            .find(|(u, _, _, _)| *u == uid)
            .ok_or_else(|| anyhow::anyhow!("no email with UID {} (mock)", uid))?
            .clone();
        // Built to agree with `parts()`, which is what `part list`
        // reports: a mock whose raw bytes described a different message
        // than its own part listing would make `part strip` behave one
        // way here and another on a server.
        let parts = self.parts(uid);
        let mut msg = format!(
            "From: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\n",
            from, subject
        );
        if parts.len() < 2 {
            msg.push_str("Content-Type: text/plain\r\n\r\n");
            msg.push_str(body.as_str());
            msg.push_str("\r\n");
        } else {
            const B: &str = "mock-boundary-4a1f";
            msg.push_str(&format!(
                "Content-Type: multipart/mixed; boundary={}\r\n\r\n--{}\r\n\
                 Content-Type: text/plain\r\n\r\n{}\r\n",
                B, B, body
            ));
            for p in parts.iter().skip(1) {
                // Filler sized to what `parts()` declares, base64'd, so
                // the decoded length a strip records matches the size
                // `part list` printed.
                let filler = vec![b'.'; p.size as usize];
                msg.push_str(&format!("--{}\r\nContent-Type: {}\r\n", B, p.content_type));
                if let Some(name) = &p.filename {
                    msg.push_str(&format!(
                        "Content-Disposition: attachment; filename=\"{}\"\r\n",
                        name
                    ));
                }
                msg.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
                for line in base64::encode(&filler).as_bytes().chunks(76) {
                    msg.push_str(std::str::from_utf8(line).unwrap());
                    msg.push_str("\r\n");
                }
            }
            msg.push_str(&format!("--{}--\r\n", B));
        }
        Ok(RawMessage {
            bytes: msg.into_bytes(),
            flags: self
                .message_flags
                .get(&uid)
                .map(|f| f.iter().cloned().collect())
                .unwrap_or_default(),
            internal_date: None,
        })
    }

    fn can_expunge_by_uid(&mut self) -> Result<bool> {
        // It advertises no extensions, but it can remove exactly the
        // messages it is handed, which is what the question asks.
        Ok(true)
    }

    fn create_folder(&mut self, name: &str, use_attr: Option<&str>) -> Result<()> {
        if self.folders.iter().any(|f| f == name) {
            bail!("mailbox '{}' already exists", name);
        }
        // The mock advertises no capabilities, so it stands in for the
        // servers that do not take a special-use attribute either.
        if let Some(attr) = use_attr {
            bail!(
                "the mock backend does not advertise CREATE-SPECIAL-USE, so it will \
                 not take USE ({}) for '{}'",
                attr,
                name
            );
        }
        self.folders.push(name.to_string());
        Ok(())
    }

    fn rename_folder(&mut self, from: &str, to: &str) -> Result<()> {
        if self.folders.iter().any(|f| f == to) {
            bail!("mailbox '{}' already exists", to);
        }
        match self.folders.iter_mut().find(|f| *f == from) {
            Some(slot) => {
                *slot = to.to_string();
                Ok(())
            }
            None => bail!("no mailbox '{}'", from),
        }
    }

    fn set_subscribed(&mut self, name: &str, subscribed: bool) -> Result<()> {
        if !self.folders.iter().any(|f| f == name) {
            bail!("no mailbox '{}'", name);
        }
        if subscribed {
            self.subscribed.insert(name.to_string());
        } else {
            self.subscribed.remove(name);
        }
        Ok(())
    }

    fn delete_folder(&mut self, name: &str, _force: bool) -> Result<()> {
        // Every check `force` governs has already run in `ImapClient`,
        // so this only has to find the mailbox and drop it. The
        // subscription, if any, is left as it is: RFC 3501's own `LSUB`
        // description says a server "will not unilaterally remove an
        // existing mailbox name from the subscription list even if a
        // mailbox by that name no longer exists", so `list_folders` /
        // `LIST` losing the name is all a real `DELETE` promises.
        match self.folders.iter().position(|f| f == name) {
            Some(pos) => {
                self.folders.remove(pos);
                Ok(())
            }
            None => bail!("no mailbox '{}'", name),
        }
    }

    fn message_flags(&mut self, folder: &str, uid: u32) -> Result<Vec<String>> {
        self.require_folder(folder)?;
        if !self.messages.iter().any(|(u, _, _, _)| *u == uid) {
            bail!("no email with UID {} (mock)", uid);
        }
        Ok(self
            .message_flags
            .get(&uid)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default())
    }

    fn close(&mut self) {}
}
