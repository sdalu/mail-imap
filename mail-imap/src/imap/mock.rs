use crate::config::Config;
use crate::imap::{normalize_flags, FolderInfo, ImapBackend, SearchResult};
use anyhow::{bail, Result};

/// In-memory mock backend. This is the original mockup, kept for offline
/// testing so the tool can be exercised without a reachable IMAP server.
pub struct MockClient {
    /// Folders that exist in the mock mailbox.
    folders: Vec<String>,
    /// uid -> (subject, from, body)
    messages: Vec<(u32, String, String, String)>,
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
        })
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

    fn search_emails(&mut self, _folder: &str, query: &str) -> Result<Vec<SearchResult>> {
        let q = query.to_lowercase();
        let results: Vec<SearchResult> = self
            .messages
            .iter()
            .filter(|(_, subject, from, _)| {
                q.is_empty()
                    || subject.to_lowercase().contains(&q)
                    || from.to_lowercase().contains(&q)
            })
            .map(|(uid, subject, from, _)| SearchResult {
                uid: *uid,
                subject: subject.clone(),
                from: from.clone(),
                date: Some("2026-09-20 12:00:00 +0000".to_string()),
                size: Some(120),
                flags: Vec::new(),
            })
            .collect();
        Ok(results)
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

    fn move_email(&mut self, _folder: &str, uid: u32, target: &str) -> Result<()> {
        if !self.messages.iter().any(|(u, _, _, _)| *u == uid) {
            anyhow::bail!("no email with UID {} (mock)", uid);
        }
        if !self.folders.iter().any(|f| f == target) {
            self.folders.push(target.to_string());
        }
        Ok(())
    }

    fn set_tags(&mut self, _folder: &str, uid: u32, add: &[String], remove: &[String]) -> Result<()> {
        if add.is_empty() && remove.is_empty() {
            bail!("no tags given");
        }
        if !self.messages.iter().any(|(u, _, _, _)| *u == uid) {
            bail!("no email with UID {} (mock)", uid);
        }
        Ok(())
    }

    fn set_flags(&mut self, _folder: &str, uid: u32, add: &[String], remove: &[String]) -> Result<()> {
        let add = normalize_flags(add)?;
        let remove = normalize_flags(remove)?;
        if add.is_empty() && remove.is_empty() {
            bail!("no flags given");
        }
        if !self.messages.iter().any(|(u, _, _, _)| *u == uid) {
            bail!("no email with UID {} (mock)", uid);
        }
        Ok(())
    }

    fn close(&mut self) {}
}
