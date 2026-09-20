use crate::config::Config;
use crate::imap::{ImapBackend, ImapClient};
use anyhow::{bail, Result};

pub fn list_folders(config: &Config) -> Result<()> {
    let mut client = ImapClient::connect(config)?;
    let folders = client.list_folders()?;

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

pub fn search_emails(config: &Config, query: &str) -> Result<()> {
    let mut client = ImapClient::connect(config)?;
    let results = client.search_emails(&config.folder, query)?;

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

pub fn read_email(config: &Config, id: u32) -> Result<()> {
    let mut client = ImapClient::connect(config)?;
    let content = client.get_email(&config.folder, id)?;
    println!("{}", content);
    Ok(())
}

pub fn move_email(config: &Config, id: u32, folder: &str) -> Result<()> {
    let mut client = ImapClient::connect(config)?;
    client.move_email(&config.folder, id, folder)?;
    println!(
        "Email (UID {}) moved from '{}' to '{}'",
        id, config.folder, folder
    );
    Ok(())
}

pub fn tag_email(config: &Config, id: u32, tags: &[String]) -> Result<()> {
    if tags.is_empty() {
        bail!("no tags given");
    }
    let mut client = ImapClient::connect(config)?;
    client.tag_email(&config.folder, id, tags)?;
    println!("Email (UID {}) tagged with: {}", id, tags.join(", "));
    Ok(())
}
