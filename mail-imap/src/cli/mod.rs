use crate::config::Config;
use crate::imap::{FolderInfo, ImapBackend, ImapClient, Mailbox, PartInfo, SearchResult};
use anyhow::{bail, Result};
use serde::Serialize;
use std::path::PathBuf;

/// UID selection parser.
///
/// Supports a single UID (`5`) or a comma-separated list (`1,4,7`).
/// Ranges (`4-7`) are **not** supported. Preserves input order and
/// deduplicates.
pub fn parse_uids(spec: &str) -> Result<Vec<u32>> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("empty UID spec");
    }
    let mut out: Vec<u32> = Vec::new();
    for token in spec.split(',') {
        let token = token.trim();
        if token.is_empty() {
            bail!("empty element in UID spec '{}'", spec);
        }
        if token.contains('-') {
            bail!(
                "UID ranges are not supported in spec '{}' (use separate arguments or a comma-separated list, e.g. '1 2 3' or '1,2,3')",
                spec
            );
        }
        let uid: u32 = token
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid UID '{}' in spec '{}'", token, spec))?;
        if !out.contains(&uid) {
            out.push(uid);
        }
    }
    if out.is_empty() {
        bail!("no UIDs in spec '{}'", spec);
    }
    Ok(out)
}

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
    /// Present when a single folder was searched (as before); when several
    /// folders were searched, `folders` is used instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    folders: Option<&'a [String]>,
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
struct CountOutput<'a> {
    /// `true` when counts are shown for all selectable mailboxes.
    all: bool,
    counts: &'a [Mailbox],
}

#[derive(Serialize)]
struct UidsOutput<'a> {
    folder: &'a str,
    count: usize,
    uids: &'a [u32],
}

#[derive(Serialize)]
struct ThreadOutput<'a> {
    folder: &'a str,
    /// The message the thread was requested for.
    uid: u32,
    count: usize,
    /// All UIDs of the thread (including `uid`), ascending.
    uids: &'a [u32],
}

#[derive(Serialize)]
struct PartsListOutput<'a> {
    folder: &'a str,
    uid: u32,
    count: usize,
    parts: &'a [PartInfo],
}

#[derive(Serialize)]
struct FlagChangeOutput<'a> {
    folder: &'a str,
    count: usize,
    uids: &'a [u32],
    added: &'a [String],
    removed: &'a [String],
}

#[derive(Serialize)]
struct FlagListOutput<'a> {
    folder: &'a str,
    uid: u32,
    count: usize,
    flags: &'a [String],
}

#[derive(Serialize)]
struct PartsSaveOutput<'a> {
    folder: &'a str,
    uid: u32,
    part: u32,
    file: &'a str,
    size: u64,
}

pub fn list_folders(config: &Config, json: bool, debug: bool) -> Result<()> {
    if debug {
        eprintln!("Connecting to {}:{} as {}", config.server, config.port, config.username);
    }
    let mut client = ImapClient::connect(config, debug)?;
    if debug {
        eprintln!("Listing folders...");
    }
    let folders = client.list_folders()?;
    if debug {
        eprintln!("Found {} folder(s)", folders.len());
    }

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

pub fn search_emails(config: &Config, query: &str, folders: Vec<String>, json: bool, debug: bool) -> Result<()> {
    if debug {
        eprintln!("Searching folders {:?} with query '{}'", folders, query);
    }
    let sort = config.sort.as_deref().map(crate::imap::parse_sort).transpose()?;
    if debug {
        if let Some(spec) = &config.sort {
            eprintln!("Sorting results by '{}'", spec);
        }
    }
    let mut client = ImapClient::connect(config, debug)?;
    let results = client.search_folders(&folders, query, config.max, sort.as_ref())?;
    if debug {
        eprintln!("Found {} result(s)", results.len());
    }

    if json {
        emit_json(&SearchOutput {
            folder: folders.get(0).filter(|_| folders.len() == 1).map(|s| s.as_str()),
            folders: if folders.len() == 1 { None } else { Some(&folders) },
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

    if folders.len() == 1 {
        println!(
            "Found {} email(s) in '{}' matching: {}",
            results.len(),
            folders[0],
            query
        );
        for r in &results {
            print_search_result("  ", r);
        }
    } else {
        println!(
            "Found {} email(s) in {} folder(s) matching: {}",
            results.len(),
            folders.len(),
            query
        );
        for folder in &folders {
            let hits: Vec<&SearchResult> = results
                .iter()
                .filter(|r| &r.folder == folder)
                .collect();
            if hits.is_empty() {
                continue;
            }
            println!("  {} ({}):", folder, hits.len());
            for r in hits {
                print_search_result("    ", r);
            }
        }
    }
    Ok(())
}

fn print_search_result(indent: &str, r: &SearchResult) {
    let date = r.date.as_deref().unwrap_or("unknown date");
    let size = r
        .size
        .map(|s| format!("  [{} bytes]", s))
        .unwrap_or_default();
    let parts = if r.parts > 0 {
        format!("  [{} part(s)]", r.parts)
    } else {
        String::new()
    };
    let flags = if r.flags.is_empty() {
        String::new()
    } else {
        format!("  [{}]", r.flags.join(" "))
    };
    println!(
        "{}UID {} | {} | {} | {}{}{}{}",
        indent, r.uid, date, r.subject, r.from, size, parts, flags
    );
}

pub fn read_emails(config: &Config, spec: &str, json: bool, debug: bool) -> Result<()> {
    let uids = parse_uids(spec)?;
    if debug {
        eprintln!("Reading {} email(s) from '{}': {:?}", uids.len(), config.folder, uids);
    }
    let mut client = ImapClient::connect(config, debug)?;

    for (i, uid) in uids.iter().enumerate() {
        let content = client.get_email(&config.folder, *uid)?;
        if json {
            emit_json(&ReadOutput {
                folder: &config.folder,
                uid: *uid,
                content: &content,
            })?;
        } else {
            if uids.len() > 1 && i > 0 {
                println!();
            }
            if uids.len() > 1 {
                println!("--- UID {} ---", uid);
            }
            println!("{}", content);
        }
    }
    Ok(())
}

pub fn mailbox_counts(config: &Config, folder: Option<&str>, json: bool, debug: bool) -> Result<()> {
    if debug {
        eprintln!("Getting mailbox counts (folder: {:?})", folder);
    }
    let mut client = ImapClient::connect(config, debug)?;
    let counts = client.mailbox_counts(folder)?;

    if json {
        emit_json(&CountOutput {
            all: folder.is_none(),
            counts: &counts,
        })?;
        return Ok(());
    }

    match folder {
        Some(f) => println!("Status for '{}':", f),
        None => println!("Mailbox counts:"),
    }
    for m in &counts {
        println!(
            "  {}: messages={} unseen={} recent={} uidnext={} uidvalidity={}",
            m.name, m.messages, m.unseen, m.recent, m.uid_next, m.uid_validity
        );
    }
    Ok(())
}

pub fn folder_uids(config: &Config, json: bool, debug: bool) -> Result<()> {
    if debug {
        eprintln!("Listing UIDs for '{}'", config.folder);
    }
    let mut client = ImapClient::connect(config, debug)?;
    let uids = client.folder_uids(&config.folder)?;

    if json {
        emit_json(&UidsOutput {
            folder: &config.folder,
            count: uids.len(),
            uids: &uids,
        })?;
        return Ok(());
    }

    if uids.is_empty() {
        println!("No messages in '{}'", config.folder);
        return Ok(());
    }
    let list = uids
        .iter()
        .map(|u| u.to_string())
        .collect::<Vec<_>>()
        .join(",");
    println!("UIDs in '{}' ({}): {}", config.folder, uids.len(), list);
    Ok(())
}

pub fn thread_uids(config: &Config, uid: u32, json: bool, debug: bool) -> Result<()> {
    let folder = config.folder.clone();
    if debug {
        eprintln!("Reconstructing thread of UID {} in '{}'", uid, folder);
    }
    let mut client = ImapClient::connect(config, debug)?;
    let uids = client.thread_uids(&folder, uid)?;
    if debug {
        eprintln!("Thread has {} message(s)", uids.len());
    }

    if json {
        emit_json(&ThreadOutput {
            folder: &folder,
            uid,
            count: uids.len(),
            uids: &uids,
        })?;
        return Ok(());
    }
    println!(
        "Thread of UID {} in '{}' ({} message(s)):",
        uid,
        folder,
        uids.len()
    );
    println!(
        "  {}",
        uids.iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    Ok(())
}

pub fn unread(config: &Config, folders: Vec<String>, json: bool, debug: bool) -> Result<()> {
    search_emails(config, "UNSEEN", folders, json, debug)
}

/// Validate flag/tag names. Each name is a system flag (`\Seen`,
/// `\Answered`, `\Flagged`, `\Deleted`, `\Draft` — case-insensitive,
/// normalized here; `\Recent` is server-managed and rejected) or a custom
/// keyword (letters, digits and the atom punctuation `$ ! # & ' + - / = ?
/// ^ _ \` { | } ~ .`). `allow_system` is false for `tag`, which only
/// accepts keywords. Deduplicates, preserving order.
pub fn parse_flag_names(names: &[String], allow_system: bool) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim();
        if name.is_empty() {
            bail!("empty flag name");
        }
        if let Some(rest) = name.strip_prefix('\\') {
            if !allow_system {
                bail!(
                    "'{}' is a system flag; tags must be plain keywords (use the 'flag' command for system flags)",
                    name
                );
            }
            let normalized = match rest.to_ascii_lowercase().as_str() {
                "seen" => Some("\\Seen"),
                "answered" => Some("\\Answered"),
                "flagged" => Some("\\Flagged"),
                "deleted" => Some("\\Deleted"),
                "draft" => Some("\\Draft"),
                "recent" => bail!("\\Recent is managed by the server and cannot be set"),
                _ => None,
            };
            let flag = match normalized {
                Some(f) => f.to_string(),
                None => bail!(
                    "unknown system flag '{}' (valid: \\Seen, \\Answered, \\Flagged, \\Deleted, \\Draft; any other token is a custom keyword)",
                    name
                ),
            };
            if !out.contains(&flag) {
                out.push(flag);
            }
            continue;
        }
        if name.starts_with('-') {
            bail!(
                "invalid flag/keyword '{}' (names must not start with '-'; global options like -j go before the UID list, not after '--')",
                name
            );
        }
        if name.contains(['\\', '*', '%', '(', ')', '{', '}', '"', ' ', ','])
            || name.chars().any(|c| c.is_control())
        {
            bail!(
                "invalid flag/keyword '{}': no \\ * % ( ) {{ }} \" , or spaces are allowed in keywords",
                name
            );
        }
        if !out.iter().any(|s| s == name) {
            out.push(name.to_string());
        }
    }
    if out.is_empty() {
        bail!("no flag names given");
    }
    Ok(out)
}

/// Shared implementation of `flag add|remove` and `tag add|remove`.
/// `allow_system` distinguishes flag (true) from tag (false);
/// `add` enables the flags, `remove` disables them.
pub fn change_flags(
    config: &Config,
    spec: &str,
    names: &[String],
    allow_system: bool,
    add: bool,
    json: bool,
    debug: bool,
) -> Result<()> {
    let uids = parse_uids(spec)?;
    let flags = parse_flag_names(names, allow_system)?;
    if debug {
        eprintln!(
            "{} {:?} on UIDs {:?} in '{}'",
            if add { "adding" } else { "removing" },
            flags,
            uids,
            config.folder
        );
    }
    let mut client = ImapClient::connect(config, debug)?;
    let (added, removed): (&[String], &[String]) =
        if add { (&flags, &[]) } else { (&[], &flags) };
    client.store_flags(&config.folder, &uids, added, removed)?;

    if json {
        emit_json(&FlagChangeOutput {
            folder: &config.folder,
            count: uids.len(),
            uids: &uids,
            added,
            removed,
        })?;
        return Ok(());
    }
    let verb = if add { "Added" } else { "Removed" };
    println!(
        "{} {} on {} message(s) in '{}': UIDs {}",
        verb,
        flags.join(", "),
        uids.len(),
        config.folder,
        uids.iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

pub fn flag_list(config: &Config, spec: &str, tags_only: bool, json: bool, debug: bool) -> Result<()> {
    if debug {
        eprintln!(
            "Listing {} for UID {}...",
            if tags_only { "tags" } else { "flags" },
            spec
        );
    }
    let uids = parse_uids(spec)?;
    let mut client = ImapClient::connect(config, debug)?;

    for uid in &uids {
        let mut flags = client.message_flags(&config.folder, *uid)?;
        if tags_only {
            flags.retain(|f| !f.starts_with('\\'));
        }
        if json {
            emit_json(&FlagListOutput {
                folder: &config.folder,
                uid: *uid,
                count: flags.len(),
                flags: &flags,
            })?;
            continue;
        }
        if flags.is_empty() {
            println!(
                "UID {}: no {}",
                uid,
                if tags_only { "tags" } else { "flags" }
            );
            continue;
        }
        println!(
            "UID {}: {} {}: {}",
            uid,
            flags.len(),
            if tags_only { "tag(s)" } else { "flag(s)" },
            flags.join(", ")
        );
    }
    Ok(())
}

pub fn parts_list(config: &Config, spec: &str, json: bool, debug: bool) -> Result<()> {
    if debug {
        eprintln!("Listing MIME parts for UID {}...", spec);
    }
    let uids = parse_uids(spec)?;
    let mut client = ImapClient::connect(config, debug)?;

    for uid in &uids {
        let parts = client.list_parts(&config.folder, *uid)?;
        if json {
            emit_json(&PartsListOutput {
                folder: &config.folder,
                uid: *uid,
                count: parts.len(),
                parts: &parts,
            })?;
            continue;
        }
        if parts.is_empty() {
            println!("UID {}: no parts", uid);
            continue;
        }
        println!("UID {}: {} part(s):", uid, parts.len());
        for a in &parts {
            let name = match &a.filename {
                Some(f) => format!(", filename={}", f),
                None => String::new(),
            };
            println!("  [{}] {}{} ({} bytes)", a.part, a.content_type, name, a.size);
        }
    }
    Ok(())
}

pub fn parts_save(
    config: &Config,
    uid: u32,
    part: u32,
    out: Option<PathBuf>,
    json: bool,
    debug: bool,
) -> Result<()> {
    if debug {
        eprintln!("Saving part {} of UID {} to '{}'", part, uid, out.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "default filename".to_string()));
    }
    let mut client = ImapClient::connect(config, debug)?;
    let parts = client.list_parts(&config.folder, uid)?;
    let info = parts
        .iter()
        .find(|a| a.part == part)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no part {} in message UID {} (message has {} part(s))",
                part,
                uid,
                parts.len()
            )
        })?;
    let dest = out.unwrap_or_else(|| {
        PathBuf::from(
            info.filename
                .as_deref()
                .unwrap_or(&format!("uid{}_part{}", uid, part)),
        )
    });

    let size = client.save_part(&config.folder, uid, part, &dest)?;
    if json {
        emit_json(&PartsSaveOutput {
            folder: &config.folder,
            uid,
            part,
            file: dest.to_str().unwrap_or_default(),
            size,
        })?;
    } else {
        println!(
            "Saved part {} of UID {} to '{}' ({} bytes)",
            part,
            uid,
            dest.display(),
            size
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uids_single() {
        assert_eq!(parse_uids("5").unwrap(), vec![5]);
        assert_eq!(parse_uids(" 5 ").unwrap(), vec![5]);
    }

    #[test]
    fn parse_uids_comma_list() {
        assert_eq!(
            parse_uids("1,4,7").unwrap(),
            vec![1, 4, 7]
        );
        assert_eq!(
            parse_uids("1, 4 ,7").unwrap(),
            vec![1, 4, 7]
        );
    }

    #[test]
    fn parse_uids_deduplicates() {
        assert_eq!(parse_uids("3,1,3,2").unwrap(), vec![3, 1, 2]);
    }

    #[test]
    fn parse_uids_rejects_ranges() {
        assert!(parse_uids("4-7").is_err());
        assert!(parse_uids("1,4-7,8").is_err());
    }

    #[test]
    fn parse_uids_rejects_garbage() {
        assert!(parse_uids("").is_err());
        assert!(parse_uids(",").is_err());
        assert!(parse_uids("1,,3").is_err());
        assert!(parse_uids("abc").is_err());
        assert!(parse_uids("-1").is_err());
        assert!(parse_uids("1.5").is_err());
    }

    #[test]
    fn search_output_json_shape_single_folder() {
        let results = vec![SearchResult {
            uid: 7,
            folder: "INBOX".into(),
            subject: "Hi".into(),
            from: "a@example.com".into(),
            date: Some("2026-09-20 12:00:00 +0000".into()),
            size: Some(120),
            flags: vec!["\\Seen".into()],
            parts: 2,
        }];
        let out = SearchOutput {
            folder: Some("INBOX"),
            folders: None,
            query: "hi",
            count: 1,
            results: &results,
        };
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string(&out).unwrap())
            .expect("parse");
        assert_eq!(value["folder"], "INBOX");
        assert!(value.get("folders").is_none());
        assert_eq!(value["query"], "hi");
        assert_eq!(value["count"], 1);
        assert_eq!(value["results"][0]["uid"], 7);
        assert_eq!(value["results"][0]["folder"], "INBOX");
        assert_eq!(value["results"][0]["subject"], "Hi");
        assert_eq!(value["results"][0]["flags"][0], "\\Seen");
        assert_eq!(value["results"][0]["parts"], 2);
    }

    #[test]
    fn search_output_json_shape_multi_folder() {
        let results = vec![
            SearchResult {
                uid: 1,
                folder: "INBOX".into(),
                subject: "A".into(),
                from: "a@example.com".into(),
                date: None,
                size: None,
                flags: Vec::new(),
                parts: 1,
            },
            SearchResult {
                uid: 2,
                folder: "Archive".into(),
                subject: "B".into(),
                from: "b@example.com".into(),
                date: None,
                size: None,
                flags: Vec::new(),
                parts: 1,
            },
        ];
        let folders = vec!["INBOX".to_string(), "Archive".to_string()];
        let out = SearchOutput {
            folder: None,
            folders: Some(&folders),
            query: "ALL",
            count: 2,
            results: &results,
        };
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string(&out).unwrap())
            .expect("parse");
        assert!(value.get("folder").is_none());
        assert_eq!(value["folders"][0], "INBOX");
        assert_eq!(value["folders"][1], "Archive");
        assert_eq!(value["results"][1]["folder"], "Archive");
    }

    #[test]
    fn count_output_json_shape() {
        let counts = vec![Mailbox {
            name: "INBOX".into(),
            messages: 5,
            unseen: 2,
            recent: 0,
            uid_next: 6,
            uid_validity: 1,
        }];
        let out = CountOutput {
            all: true,
            counts: &counts,
        };
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string(&out).unwrap())
            .expect("parse");
        assert_eq!(value["all"], true);
        assert_eq!(value["counts"][0]["name"], "INBOX");
        assert_eq!(value["counts"][0]["unseen"], 2);
    }

    #[test]
    fn thread_output_json_shape() {
        let uids = vec![3, 9, 14];
        let out = ThreadOutput {
            folder: "INBOX",
            uid: 9,
            count: uids.len(),
            uids: &uids,
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&out).unwrap()).expect("parse");
        assert_eq!(value["folder"], "INBOX");
        assert_eq!(value["uid"], 9);
        assert_eq!(value["count"], 3);
        assert_eq!(value["uids"][0], 3);
        assert_eq!(value["uids"][2], 14);
    }

    #[test]
    fn parts_output_json_shape() {
        let parts = vec![PartInfo {
            part: 2,
            content_type: "application/pdf".into(),
            filename: Some("invoice-42.pdf".into()),
            size: 51200,
        }];
        let out = PartsListOutput {
            folder: "INBOX",
            uid: 5,
            count: 1,
            parts: &parts,
        };
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string(&out).unwrap())
            .expect("parse");
        assert_eq!(value["uid"], 5);
        assert_eq!(value["parts"][0]["part"], 2);
        assert_eq!(value["parts"][0]["filename"], "invoice-42.pdf");
    }

    #[test]
    fn parse_flag_names_normalizes_and_dedups() {
        let v = parse_flag_names(
            &["\\seen".into(), "\\FLAGGED".into(), "junk".into(), "\\seen".into()],
            true,
        )
        .unwrap();
        assert_eq!(v, vec!["\\Seen", "\\Flagged", "junk"]);
    }

    #[test]
    fn parse_flag_names_rejects_recent_and_unknown_system_flags() {
        assert!(parse_flag_names(&["\\Recent".into()], true).is_err());
        assert!(parse_flag_names(&["\\Bogus".into()], true).is_err());
        assert!(parse_flag_names(&["\\".into()], true).is_err());
    }

    #[test]
    fn parse_flag_names_tag_mode_rejects_system_flags() {
        assert!(parse_flag_names(&["\\Seen".into()], false).is_err());
        let v = parse_flag_names(&["$Important".into(), "my-tag".into()], false).unwrap();
        assert_eq!(v, vec!["$Important", "my-tag"]);
    }

    #[test]
    fn parse_flag_names_rejects_special_characters() {
        for bad in ["a b", "a,b", "a*b", "a%b", "a(b", "a}b", "\"x\"", "a\nb", "-mytag", "-j"] {
            assert!(
                parse_flag_names(&[bad.to_string()], true).is_err(),
                "should reject {:?}",
                bad
            );
        }
        assert!(parse_flag_names(&[], true).is_err());
    }

    #[test]
    fn flag_change_output_json_shape() {
        let uids = vec![2, 5];
        let added = vec!["\\Flagged".to_string(), "invoice".to_string()];
        let none: Vec<String> = Vec::new();
        let out = FlagChangeOutput {
            folder: "INBOX",
            count: 2,
            uids: &uids,
            added: &added,
            removed: &none,
        };
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string(&out).unwrap())
            .expect("parse");
        assert_eq!(value["folder"], "INBOX");
        assert_eq!(value["count"], 2);
        assert_eq!(value["uids"][1], 5);
        assert_eq!(value["added"][0], "\\Flagged");
        assert_eq!(value["added"][1], "invoice");
        assert_eq!(value["removed"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn flag_list_output_json_shape() {
        let flags = vec!["\\Seen".to_string(), "invoice".to_string()];
        let out = FlagListOutput {
            folder: "INBOX",
            uid: 5,
            count: 2,
            flags: &flags,
        };
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string(&out).unwrap())
            .expect("parse");
        assert_eq!(value["folder"], "INBOX");
        assert_eq!(value["uid"], 5);
        assert_eq!(value["count"], 2);
        assert_eq!(value["flags"][0], "\\Seen");
        assert_eq!(value["flags"][1], "invoice");
    }
}
