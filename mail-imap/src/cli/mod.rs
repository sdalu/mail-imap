use crate::config::Config;
use crate::imap::{FolderInfo, ImapBackend, ImapClient, SearchResult};
use anyhow::{bail, Result};
use serde::Serialize;

/// Print `value` as compact single-line JSON to stdout.
fn emit_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

#[derive(Serialize)]
struct FoldersOutput<'a> {
    count: usize,
    folders: &'a [FolderInfo],
}

#[derive(Serialize)]
struct SearchOutput<'a> {
    folder: &'a str,
    query: &'a str,
    count: usize,
    results: &'a [SearchResult],
}

#[derive(Serialize)]
struct ReadOutput<'a> {
    folder: &'a str,
    uid: u32,
    content: &'a str,
}

#[derive(Serialize)]
struct MoveOutput<'a> {
    folder: &'a str,
    uid: u32,
    to: &'a str,
}

#[derive(Serialize)]
struct SetOutput<'a> {
    folder: &'a str,
    uid: u32,
    added: &'a [String],
    removed: &'a [String],
}

pub fn list_folders(config: &Config, json: bool) -> Result<()> {
    let mut client = ImapClient::connect(config)?;
    let folders = client.list_folders()?;

    if json {
        emit_json(&FoldersOutput {
            count: folders.len(),
            folders: &folders,
        })?;
        return Ok(());
    }

    if folders.is_empty() {
        println!("No folders found.");
        return Ok(());
    }

    println!("Folders ({}):", folders.len());
    for f in folders {
        let mut meta = Vec::new();
        if let Some(d) = &f.delimiter {
            if !d.is_empty() {
                meta.push(format!("delim='{}'", d));
            }
        }
        if f.no_inferiors {
            meta.push("\\Noinferiors".to_string());
        }
        meta.extend(f.attrs.iter().cloned());
        let extra = meta.join(" ");
        if extra.is_empty() {
            println!("  - {}", f.name);
        } else {
            println!("  - {} ({})", f.name, extra);
        }
    }
    Ok(())
}

pub fn search_emails(config: &Config, query: &str, json: bool) -> Result<()> {
    let mut client = ImapClient::connect(config)?;
    let results = client.search_emails(&config.folder, query)?;

    if json {
        emit_json(&SearchOutput {
            folder: &config.folder,
            query,
            count: results.len(),
            results: &results,
        })?;
        return Ok(());
    }

    if results.is_empty() {
        println!("No emails matched query: {}", query);
        return Ok(());
    }

    println!(
        "Found {} email(s) in '{}' matching: {}",
        results.len(),
        config.folder,
        query
    );
    for r in results {
        let date = r.date.as_deref().unwrap_or("unknown date");
        let size = r
            .size
            .map(|s| format!("  [{} bytes]", s))
            .unwrap_or_default();
        let flags = if r.flags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", r.flags.join(" "))
        };
        println!(
            "  UID {} | {} | {} | {}{}{}",
            r.uid, date, r.subject, r.from, size, flags
        );
    }
    Ok(())
}

pub fn read_email(config: &Config, id: u32, json: bool) -> Result<()> {
    let mut client = ImapClient::connect(config)?;
    let content = client.get_email(&config.folder, id)?;
    if json {
        emit_json(&ReadOutput {
            folder: &config.folder,
            uid: id,
            content: &content,
        })?;
    } else {
        println!("{}", content);
    }
    Ok(())
}

pub fn move_email(config: &Config, id: u32, folder: &str, json: bool) -> Result<()> {
    let mut client = ImapClient::connect(config)?;
    client.move_email(&config.folder, id, folder)?;
    if json {
        emit_json(&MoveOutput {
            folder: &config.folder,
            uid: id,
            to: folder,
        })?;
    } else {
        println!(
            "Email (UID {}) moved from '{}' to '{}'",
            id, config.folder, folder
        );
    }
    Ok(())
}

pub fn set_tags(config: &Config, id: u32, add: &[String], remove: &[String], json: bool) -> Result<()> {
    if add.is_empty() && remove.is_empty() {
        bail!("no tags given");
    }
    let mut client = ImapClient::connect(config)?;
    client.set_tags(&config.folder, id, add, remove)?;
    if json {
        emit_json(&SetOutput {
            folder: &config.folder,
            uid: id,
            added: add,
            removed: remove,
        })?;
    } else {
        let mut done = Vec::new();
        if !add.is_empty() {
            done.push(format!("added tags: {}", add.join(", ")));
        }
        if !remove.is_empty() {
            done.push(format!("removed tags: {}", remove.join(", ")));
        }
        println!("Email (UID {}): {}", id, done.join("; "));
    }
    Ok(())
}

pub fn set_flags(config: &Config, id: u32, add: &[String], remove: &[String], json: bool) -> Result<()> {
    if add.is_empty() && remove.is_empty() {
        bail!("no flags given");
    }
    let mut client = ImapClient::connect(config)?;
    client.set_flags(&config.folder, id, add, remove)?;
    if json {
        emit_json(&SetOutput {
            folder: &config.folder,
            uid: id,
            added: add,
            removed: remove,
        })?;
    } else {
        let mut done = Vec::new();
        if !add.is_empty() {
            done.push(format!("added {}", add.join(", ")));
        }
        if !remove.is_empty() {
            done.push(format!("removed {}", remove.join(", ")));
        }
        println!("Email (UID {}): {}", id, done.join("; "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_output_json_shape() {
        let results = vec![SearchResult {
            uid: 7,
            subject: "Hi".into(),
            from: "a@example.com".into(),
            date: Some("2026-09-20 12:00:00 +0000".into()),
            size: Some(120),
            flags: vec!["\\Seen".into()],
        }];
        let out = SearchOutput {
            folder: "INBOX",
            query: "hi",
            count: 1,
            results: &results,
        };
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string(&out).unwrap())
            .expect("parse");
        assert_eq!(value["folder"], "INBOX");
        assert_eq!(value["query"], "hi");
        assert_eq!(value["count"], 1);
        assert_eq!(value["results"][0]["uid"], 7);
        assert_eq!(value["results"][0]["subject"], "Hi");
        assert_eq!(value["results"][0]["flags"][0], "\\Seen");
    }

    #[test]
    fn set_output_json_shape() {
        let add = vec!["important".to_string()];
        let remove: Vec<String> = Vec::new();
        let out = SetOutput {
            folder: "INBOX",
            uid: 3,
            added: &add,
            removed: &remove,
        };
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string(&out).unwrap())
            .expect("parse");
        assert_eq!(value["uid"], 3);
        assert_eq!(value["added"][0], "important");
        assert!(value["removed"].is_array());
        assert!(value["removed"].as_array().unwrap().is_empty());
    }
}
