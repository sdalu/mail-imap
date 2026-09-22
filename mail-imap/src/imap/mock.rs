use crate::config::Config;
use crate::imap::{
    sort_results, thread_component, FolderInfo, ImapBackend, Mailbox, PartInfo, SearchResult,
    SortCriteria, ThreadRefs,
};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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
            subscribed: BTreeSet::new(),
        })
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
    fn list_folders(&mut self) -> Result<Vec<FolderInfo>> {
        Ok(self
            .folders
            .iter()
            .map(|name| FolderInfo {
                name: name.clone(),
                delimiter: Some("/".to_string()),
                no_inferiors: name == "INBOX",
                attrs: Vec::new(),
            })
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

    fn get_email(&mut self, _folder: &str, uid: u32) -> Result<String> {
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

    fn folder_uids(&mut self, _folder: &str) -> Result<Vec<u32>> {
        Ok(self.messages.iter().map(|(u, _, _, _)| *u).collect())
    }

    fn thread_uids(&mut self, folder: &str, uid: u32) -> Result<Vec<u32>> {
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

    fn list_parts(&mut self, _folder: &str, uid: u32) -> Result<Vec<PartInfo>> {
        if !self.messages.iter().any(|(u, _, _, _)| *u == uid) {
            bail!("no email with UID {} (mock)", uid);
        }
        Ok(self.parts(uid))
    }

    fn save_part(
        &mut self,
        _folder: &str,
        uid: u32,
        part: u32,
        dest: &Path,
    ) -> Result<u64> {
        let parts = self.list_parts(_folder, uid)?;
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
        std::fs::write(dest, &data)?;
        Ok(data.len() as u64)
    }

    fn store_flags(
        &mut self,
        _folder: &str,
        uids: &[u32],
        add: &[String],
        remove: &[String],
    ) -> Result<()> {
        for uid in uids {
            if !self.messages.iter().any(|(u, _, _, _)| u == uid) {
                bail!("no email with UID {} (mock)", uid);
            }
        }
        for uid in uids {
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

    fn message_flags(&mut self, _folder: &str, uid: u32) -> Result<Vec<String>> {
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
