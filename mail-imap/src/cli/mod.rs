pub mod keywords;
pub mod modutf7;
pub mod select;
pub mod tbkey;

use crate::config::Config;
use crate::imap::{FolderInfo, ImapBackend, ImapClient, Mailbox, PartInfo, SearchResult};
use anyhow::{bail, Result};
use serde::Serialize;
use select::Selection;
use unicode_normalization::UnicodeNormalization;
use std::path::PathBuf;

/// The folders a command works on, as given on the command line:
/// literal names, IMAP `LIST` patterns, or nothing at all (in which
/// case the `folder` of the config is used).
#[derive(Debug, Clone, Default)]
pub struct FolderSpec {
    patterns: Vec<String>,
}

impl FolderSpec {
    pub fn new(patterns: Vec<String>) -> Self {
        FolderSpec { patterns }
    }

    /// Whether the user named any folder (`-f`, `-A`).
    pub fn is_given(&self) -> bool {
        !self.patterns.is_empty()
    }
}

/// The mailbox names and hierarchy delimiters known to the server,
/// fetched only when a pattern actually has to be expanded.
fn known_folders(client: &mut ImapClient) -> Result<Vec<(String, Option<String>)>> {
    Ok(client
        .list_folders()?
        .into_iter()
        .map(|f| (f.name, f.delimiter))
        .collect())
}

/// Resolve a [`FolderSpec`] to the folders to work on, expanding
/// patterns against the mailbox list.
pub fn folders(client: &mut ImapClient, spec: &FolderSpec, config: &Config) -> Result<Vec<String>> {
    let patterns = if spec.is_given() {
        spec.patterns.clone()
    } else {
        vec![config.folder.clone()]
    };
    let known = if patterns.iter().any(|p| select::is_pattern(p)) {
        known_folders(client)?
    } else {
        Vec::new()
    };
    select::expand_folders(&patterns, &known)
}

/// The one folder an unqualified UID selection refers to. Several
/// folders are an error rather than a guess: the selection says which
/// one it means (`Archive:5`), or `-f` names a single folder.
pub fn default_folder(
    client: &mut ImapClient,
    spec: &FolderSpec,
    config: &Config,
) -> Result<String> {
    let folders = folders(client, spec, config)?;
    match folders.len() {
        1 => Ok(folders.into_iter().next().unwrap()),
        _ => bail!(
            "{} folders are selected ({}), so a bare UID is ambiguous: qualify it \
             with a folder (e.g. {}::12345) or name a single one with -f",
            folders.len(),
            folders.join(", "),
            folders.first().map(String::as_str).unwrap_or("INBOX")
        ),
    }
}

/// Messages of one folder, in the order the selections named them.
#[derive(Debug, Clone)]
pub struct Group {
    pub folder: String,
    pub uids: Vec<u32>,
}

/// Resolve parsed selections into per-folder UID groups. Folders keep
/// the order they were first named in; a folder's UID list is fetched
/// once, and only when a selection of that folder holds a range or `*`.
pub fn resolve_groups(
    client: &mut ImapClient,
    selections: &[Selection],
    default: &str,
) -> Result<Vec<Group>> {
    let mut grouped: Vec<(String, Vec<&Selection>)> = Vec::new();
    for selection in selections {
        let folder = selection.folder.clone().unwrap_or_else(|| default.to_string());
        match grouped.iter_mut().find(|(f, _)| *f == folder) {
            Some((_, sels)) => sels.push(selection),
            None => grouped.push((folder, vec![selection])),
        }
    }

    let mut out = Vec::new();
    for (folder, selections) in grouped {
        let available = if selections.iter().any(|s| s.needs_uid_list()) {
            Some(client.folder_uids(&folder)?)
        } else {
            None
        };
        let mut uids: Vec<u32> = Vec::new();
        for selection in &selections {
            for uid in selection.resolve(available.as_deref())? {
                if !uids.contains(&uid) {
                    uids.push(uid);
                }
            }
        }
        if uids.is_empty() {
            bail!(
                "selection '{}' matched no message in '{}'",
                selections
                    .iter()
                    .map(|s| s.source.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                folder
            );
        }
        out.push(Group { folder, uids });
    }
    Ok(out)
}

/// Every UID of every group, as `(folder, uid)` pairs in order.
fn flatten(groups: &[Group]) -> Vec<(&str, u32)> {
    groups
        .iter()
        .flat_map(|g| g.uids.iter().map(move |u| (g.folder.as_str(), *u)))
        .collect()
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
    /// Present when exactly one folder was searched; `folders` carries
    /// the list when several were.
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
struct KnownKeyword<'a> {
    keyword: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    means: Option<&'a str>,
}

#[derive(Serialize)]
struct KnownOutput<'a> {
    count: usize,
    /// Keywords in the IANA IMAP/JMAP registry.
    registered: &'a [KnownKeyword<'a>],
    /// Keywords no registry defines, that clients write anyway.
    well_known: &'a [KnownKeyword<'a>],
}

#[derive(Serialize)]
struct PartsSaveOutput<'a> {
    folder: &'a str,
    uid: u32,
    part: u32,
    file: &'a str,
    size: u64,
}

/// Annotate the keywords a convention explains. A `$label1` says
/// nothing by itself — it is unreadable unless you happen to run the
/// client that wrote it. JSON keeps the raw names; a person gets the
/// gloss.
fn gloss(flags: &[String]) -> Vec<String> {
    flags
        .iter()
        .map(|f| {
            if let Some(means) = keywords::meaning(f) {
                return format!("{} ({})", f, means);
            }
            if let Some(text) = modutf7::decoded_display(f) {
                return format!("{} (\"{}\", modified UTF-7)", f, text);
            }
            if let Some(text) = tbkey::decode(f) {
                return format!("{} (\"{}\", Thunderbird tag key)", f, text);
            }
            f.clone()
        })
        .collect()
}

/// Print the keywords the tool knows about. Needs no server: both
/// tables live in the binary (`src/cli/keywords.rs`).
pub fn tags_known(json: bool) -> Result<()> {
    let registered: Vec<KnownKeyword> = keywords::registered()
        .iter()
        .map(|k| KnownKeyword {
            keyword: k,
            means: None,
        })
        .collect();
    let well_known: Vec<KnownKeyword> = keywords::well_known()
        .iter()
        .map(|(k, m)| KnownKeyword {
            keyword: k,
            means: Some(m),
        })
        .collect();
    if json {
        return emit_json(&KnownOutput {
            count: registered.len() + well_known.len(),
            registered: &registered,
            well_known: &well_known,
        });
    }
    println!("IANA-registered keywords ({}):", registered.len());
    for k in &registered {
        println!("  {}", k.keyword);
    }
    println!();
    println!(
        "Well known, but registered nowhere ({}):",
        well_known.len()
    );
    for k in &well_known {
        println!("  {:<10} {}", k.keyword, k.means.unwrap_or(""));
    }
    println!();
    println!("Any other atom is a valid keyword too; these are the ones with an agreed meaning.");
    Ok(())
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

pub fn search_emails(
    config: &Config,
    query: &str,
    spec: &FolderSpec,
    json: bool,
    debug: bool,
) -> Result<()> {
    let sort = config.sort.as_deref().map(crate::imap::parse_sort).transpose()?;
    if debug {
        if let Some(spec) = &config.sort {
            eprintln!("Sorting results by '{}'", spec);
        }
    }
    let mut client = ImapClient::connect(config, debug)?;
    let folders = folders(&mut client, spec, config)?;
    if debug {
        eprintln!("Searching folders {:?} with query '{}'", folders, query);
    }
    let results = client.search_folders(&folders, query, config.max, sort.as_ref())?;
    if debug {
        eprintln!("Found {} result(s)", results.len());
    }

    if json {
        emit_json(&SearchOutput {
            folder: folders.first().filter(|_| folders.len() == 1).map(|s| s.as_str()),
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

/// Connect, resolve the selections, and hand back the per-folder
/// groups. The default folder is only resolved when some selection
/// needs it, so `read Archive:5 Sent:9` works with several `-f`
/// folders in force.
fn selection_groups(
    client: &mut ImapClient,
    spec: &FolderSpec,
    config: &Config,
    selections: &[Selection],
    recency: Option<select::UidItem>,
) -> Result<Vec<Group>> {
    // `--last N` / `--first N` name a count rather than UIDs, so they
    // are not ambiguous across folders the way a bare UID is: they mean
    // N per selected folder.
    if let Some(item) = recency {
        if !selections.is_empty() {
            bail!(
                "--last/--first name a count of messages, so they cannot be combined \
                 with an explicit message selection"
            );
        }
        let per_folder: Vec<Selection> = folders(client, spec, config)?
            .into_iter()
            .map(|folder| Selection {
                folder: Some(folder),
                items: vec![item],
                source: match item {
                    select::UidItem::Last(n) => format!("--last {}", n),
                    select::UidItem::First(n) => format!("--first {}", n),
                    _ => "count".to_string(),
                },
            })
            .collect();
        return resolve_groups(client, &per_folder, "");
    }
    let default = if selections.iter().any(|s| s.folder.is_none()) {
        default_folder(client, spec, config)?
    } else {
        String::new()
    };
    resolve_groups(client, selections, &default)
}

pub fn read_emails(
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    recency: Option<select::UidItem>,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections, recency)?;
    let messages = flatten(&groups);
    if debug {
        eprintln!("Reading {} email(s): {:?}", messages.len(), messages);
    }

    for (i, (folder, uid)) in messages.iter().enumerate() {
        let content = client.get_email(folder, *uid)?;
        if json {
            emit_json(&ReadOutput {
                folder,
                uid: *uid,
                content: &content,
            })?;
        } else {
            if messages.len() > 1 && i > 0 {
                println!();
            }
            if messages.len() > 1 {
                println!("--- {}::{} ---", folder, uid);
            }
            println!("{}", content);
        }
    }
    Ok(())
}

pub fn mailbox_counts(config: &Config, spec: &FolderSpec, json: bool, debug: bool) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    // `count` is the one command whose default is every mailbox rather
    // than the config folder.
    let selected = if spec.is_given() {
        Some(folders(&mut client, spec, config)?)
    } else {
        None
    };
    if debug {
        eprintln!("Getting mailbox counts (folders: {:?})", selected);
    }
    let counts = match &selected {
        None => client.mailbox_counts(None)?,
        Some(folders) => {
            let mut out = Vec::new();
            for folder in folders {
                out.extend(client.mailbox_counts(Some(folder))?);
            }
            out
        }
    };

    if json {
        emit_json(&CountOutput {
            all: selected.is_none(),
            counts: &counts,
        })?;
        return Ok(());
    }

    match &selected {
        Some(folders) => println!("Status for {}:", folders.join(", ")),
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

pub fn folder_uids(config: &Config, spec: &FolderSpec, json: bool, debug: bool) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let folders = folders(&mut client, spec, config)?;
    if debug {
        eprintln!("Listing UIDs for {:?}", folders);
    }

    for folder in &folders {
        let uids = client.folder_uids(folder)?;
        if json {
            emit_json(&UidsOutput {
                folder,
                count: uids.len(),
                uids: &uids,
            })?;
            continue;
        }
        if uids.is_empty() {
            println!("No messages in '{}'", folder);
            continue;
        }
        let list = uids
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");
        println!("UIDs in '{}' ({}): {}", folder, uids.len(), list);
    }
    Ok(())
}

pub fn thread_uids(
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    recency: Option<select::UidItem>,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections, recency)?;

    for (folder, uid) in flatten(&groups) {
        if debug {
            eprintln!("Reconstructing thread of UID {} in '{}'", uid, folder);
        }
        let uids = client.thread_uids(folder, uid)?;
        if json {
            emit_json(&ThreadOutput {
                folder,
                uid,
                count: uids.len(),
                uids: &uids,
            })?;
            continue;
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
    }
    Ok(())
}

pub fn unread(config: &Config, spec: &FolderSpec, json: bool, debug: bool) -> Result<()> {
    search_emails(config, "UNSEEN", spec, json, debug)
}

/// Are these two keywords the same name?
///
/// The comparison is on the **decoded** text, folding ASCII case only.
/// Keying on "contains `&`" instead would refuse to fold `R&-D` against
/// `r&-d`, which are two spellings of one ASCII word; keying on the raw
/// bytes with a blind fold would merge `r&AOk-gie` ("régie") with
/// `r&aok-gie` ("r檉gie"), which are two different words. Folding only
/// ASCII is deliberate: IMAP requires no Unicode case folding, and
/// `é`/`É` are left as the distinct keywords a server would see.
fn same_keyword(a: &str, b: &str) -> bool {
    match (modutf7::decode(a), modutf7::decode(b)) {
        (Ok(x), Ok(y)) => x.eq_ignore_ascii_case(&y),
        _ => a == b,
    }
}

/// Characters that are neither control nor ASCII space, yet leave no
/// mark: a keyword carrying one is a lookalike of the keyword without
/// it, and nothing in a listing would show the difference.
fn is_invisible(c: char) -> bool {
    c.is_whitespace()
        || matches!(c,
            '\u{00ad}'                  // soft hyphen
            | '\u{200b}'..='\u{200f}'   // zero-width and bidi marks
            | '\u{202a}'..='\u{202e}'   // bidi embedding
            | '\u{2060}'..='\u{2064}'   // word joiner, invisible operators
            | '\u{feff}')               // byte-order mark
}

/// Validate flag/tag names. The two commands own one kind each and do
/// not overlap: `flag` takes only the IMAP-defined system flags
/// (`\Seen`, `\Answered`, `\Flagged`, `\Deleted`, `\Draft` —
/// case-insensitive, normalized here; `\Recent` is server-managed and
/// rejected), `tag` only user-defined keywords (letters, digits and the
/// atom punctuation `$ ! # & ' + - / = ? ^ _ \` { | } ~ .`).
/// `system` selects which. Deduplicates, preserving order.
///
/// The split is what keeps a mutation honest: `flag add 5 -- '\Deleted'
/// Trash` used to store a keyword `Trash` on message 5, and now says so.
pub fn parse_flag_names(names: &[String], system: bool, wire_form: bool) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim();
        if name.is_empty() {
            bail!("empty flag name");
        }
        if let Some(rest) = name.strip_prefix('\\') {
            if !system {
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
                    "unknown system flag '{}' (valid: \\Seen, \\Answered, \\Flagged, \\Deleted, \\Draft)",
                    name
                ),
            };
            if !out.iter().any(|f| f.eq_ignore_ascii_case(&flag)) {
                out.push(flag);
            }
            continue;
        }
        if system {
            bail!(
                "'{}' is not an IMAP-defined flag: 'flag' takes \\Seen, \\Answered, \\Flagged, \\Deleted \
                 and \\Draft; user-defined keywords are the 'tag' command's",
                name
            );
        }
        if crate::cli::select::parse_selection(name)
            .map(|s| s.folder.is_none())
            .unwrap_or(false)
        {
            bail!(
                "'{}' reads as a message selection, not a flag name: UIDs go before \
                 the '--' separator, tag names after it",
                name
            );
        }
        if name.starts_with('-') {
            bail!(
                "invalid flag/keyword '{}' (names must not start with '-'; global options like -j go before the UID list, not after '--')",
                name
            );
        }
        // A keyword is an IMAP atom (RFC 3501 §9): everything except
        // the atom-specials. Note that ',' IS allowed — modified UTF-7
        // writes it inside base64, so refusing it would refuse every
        // encoded keyword.
        if name.contains(['(', ')', '{', '%', '*', '"', '\\', ']'])
            || name.chars().any(|c| c.is_control() || is_invisible(c))
        {
            bail!(
                "invalid keyword '{}': an IMAP atom excludes ( ) {{ % * \" \\ ] , spaces \
                 and control characters, and this tool also refuses invisible ones \
                 (NBSP, BOM, zero-width) because they make lookalike keywords \
                 (a comma is fine: modified UTF-7 uses it)",
                name
            );
        }
        if let Some(flag) = keywords::jmap_spelling_of(name) {
            bail!(
                "'{}' is JMAP's spelling of the IMAP system flag {}; in IMAP that is \
                 a flag, not a keyword (use 'flag' with {})",
                name,
                flag,
                flag
            );
        }
        // Atoms are ASCII, so a name is encoded to modified UTF-7 on the
        // way out. The old rule — pass a name through when it happens to
        // decode — was undecidable from the name alone: 'pen&ink-notes'
        // decodes (to "pen詹notes") and 'fish&chips-2024' does not, and
        // no user can tell which without doing base64 by hand. So the
        // default is literal, and `--wire` is how a key copied out of a
        // listing goes back verbatim.
        let wire = if wire_form {
            // --wire says "this is already an atom", and an atom is
            // ASCII. Sending the raw bytes would be a malformed command.
            if !name.is_ascii() {
                bail!(
                    "'{}' is not ASCII, so it cannot be a wire keyword: drop --wire and \
                     let it be encoded (it would be sent as '{}')",
                    name,
                    modutf7::encode(name)
                );
            }
            name.to_string()
        } else {
            // Compose first: "régie" typed on a Mac may arrive as e +
            // U+0301, which would encode to a different atom that looks
            // identical in every listing, and which a later `tag remove`
            // spelled the other way would silently miss.
            let composed: String = name.nfc().collect();
            let name: &str = &composed;
            let encoded = modutf7::encode(name);
            if encoded != name && modutf7::is_canonical(name) {
                eprintln!(
                    "Note: '{}' is also a valid modified UTF-7 key ({:?}); it was taken \
                     literally and sent as '{}'. Pass --wire to send it verbatim.",
                    name,
                    modutf7::decode(name).unwrap_or_default(),
                    encoded
                );
            }
            encoded
        };
        // A registered keyword goes out in its registered spelling, the
        // same normalization the system flags get.
        let wire = keywords::canonical(&wire)
            .map(str::to_string)
            .unwrap_or(wire);
        if !out.iter().any(|s| same_keyword(s, &wire)) {
            out.push(wire);
        }
    }
    if out.is_empty() {
        bail!("no flag names given");
    }
    Ok(out)
}

/// Shared implementation of `flag add|remove` and `tag add|remove`.
/// `system` distinguishes flag (true) from tag (false);
/// `add` enables the flags, `remove` disables them.
#[allow(clippy::too_many_arguments)]
pub fn change_flags(
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    recency: Option<select::UidItem>,
    names: &[String],
    system: bool,
    wire_form: bool,
    add: bool,
    json: bool,
    debug: bool,
) -> Result<()> {
    let flags = parse_flag_names(names, system, wire_form)?;
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections, recency)?;
    let (added, removed): (&[String], &[String]) =
        if add { (&flags, &[]) } else { (&[], &flags) };

    for group in &groups {
        // RFC 3501: `UID STORE` ignores a UID that does not exist,
        // without an error. Reporting "Added \Deleted on 1 message(s)"
        // for a typo'd UID would be a lie the server never told, so the
        // only mutating path checks the UIDs first.
        let existing = client.folder_uids(&group.folder)?;
        let missing: Vec<String> = group
            .uids
            .iter()
            .filter(|u| !existing.contains(u))
            .map(|u| u.to_string())
            .collect();
        if !missing.is_empty() {
            bail!(
                "no message with UID {} in '{}' (UID STORE would ignore it silently, \
                 so nothing was changed)",
                missing.join(", "),
                group.folder
            );
        }
        if debug {
            eprintln!(
                "{} {:?} on UIDs {:?} in '{}'",
                if add { "adding" } else { "removing" },
                flags,
                group.uids,
                group.folder
            );
        }
        client.store_flags(&group.folder, &group.uids, added, removed)?;

        if json {
            emit_json(&FlagChangeOutput {
                folder: &group.folder,
                count: group.uids.len(),
                uids: &group.uids,
                added,
                removed,
            })?;
            continue;
        }
        let verb = if add { "Added" } else { "Removed" };
        println!(
            "{} {} on {} message(s) in '{}': UIDs {}",
            verb,
            flags.join(", "),
            group.uids.len(),
            group.folder,
            group
                .uids
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

pub fn flag_list(
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    recency: Option<select::UidItem>,
    tags_only: bool,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections, recency)?;

    for (folder, uid) in flatten(&groups) {
        if debug {
            eprintln!(
                "Listing {} of {}::{}",
                if tags_only { "tags" } else { "flags" },
                folder,
                uid
            );
        }
        let mut flags = client.message_flags(folder, uid)?;
        if tags_only {
            flags.retain(|f| !f.starts_with('\\'));
        }
        if json {
            emit_json(&FlagListOutput {
                folder,
                uid,
                count: flags.len(),
                flags: &flags,
            })?;
            continue;
        }
        if flags.is_empty() {
            println!(
                "{}::{}: no {}",
                folder,
                uid,
                if tags_only { "tags" } else { "flags" }
            );
            continue;
        }
        let shown = gloss(&flags);
        println!(
            "{}::{}: {} {}: {}",
            folder,
            uid,
            flags.len(),
            if tags_only { "tag(s)" } else { "flag(s)" },
            shown.join(", ")
        );
    }
    Ok(())
}

pub fn parts_list(
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    recency: Option<select::UidItem>,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections, recency)?;

    for (folder, uid) in flatten(&groups) {
        if debug {
            eprintln!("Listing MIME parts of {}::{}", folder, uid);
        }
        let parts = client.list_parts(folder, uid)?;
        if json {
            emit_json(&PartsListOutput {
                folder,
                uid,
                count: parts.len(),
                parts: &parts,
            })?;
            continue;
        }
        if parts.is_empty() {
            println!("{}::{}: no parts", folder, uid);
            continue;
        }
        println!("{}::{}: {} part(s):", folder, uid, parts.len());
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
    spec: &FolderSpec,
    selection: &Selection,
    part: u32,
    out: Option<PathBuf>,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, std::slice::from_ref(selection), None)?;
    let messages = flatten(&groups);
    if messages.len() != 1 {
        bail!(
            "'part save' writes one part of one message, but selection '{}' names {} \
             (use 'part list' to see them, then save one at a time)",
            selection.source,
            messages.len()
        );
    }
    let (folder, uid) = messages[0];
    let folder = folder.to_string();
    if debug {
        eprintln!("Saving part {} of UID {} to '{}'", part, uid, out.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "default filename".to_string()));
    }
    let parts = client.list_parts(&folder, uid)?;
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

    let size = client.save_part(&folder, uid, part, &dest)?;
    if json {
        emit_json(&PartsSaveOutput {
            folder: &folder,
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

    fn mock_config() -> Config {
        Config {
            mock: true,
            ..Config::default()
        }
    }

    fn mock_client() -> ImapClient {
        ImapClient::connect(&mock_config(), false).expect("connect")
    }

    fn sels(tokens: &[&str]) -> Vec<Selection> {
        let owned: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
        select::parse_selections(&owned).expect("parse")
    }

    #[test]
    fn folders_default_to_the_config_folder() {
        let mut client = mock_client();
        let spec = FolderSpec::default();
        assert!(!spec.is_given());
        assert_eq!(
            folders(&mut client, &spec, &mock_config()).unwrap(),
            vec!["INBOX"]
        );
    }

    #[test]
    fn folder_patterns_expand_against_the_mailbox_list() {
        let mut client = mock_client();
        let spec = FolderSpec::new(vec!["S*".to_string()]);
        assert_eq!(
            folders(&mut client, &spec, &mock_config()).unwrap(),
            vec!["Sent Items", "Spam"]
        );
        let all = FolderSpec::new(vec!["*".to_string()]);
        assert_eq!(folders(&mut client, &all, &mock_config()).unwrap().len(), 5);
    }

    #[test]
    fn a_pattern_matching_no_mailbox_is_an_error() {
        let mut client = mock_client();
        let spec = FolderSpec::new(vec!["Nope/*".to_string()]);
        assert!(folders(&mut client, &spec, &mock_config()).is_err());
    }

    #[test]
    fn a_bare_uid_is_refused_when_several_folders_are_selected() {
        let mut client = mock_client();
        let spec = FolderSpec::new(vec!["INBOX".to_string(), "Trash".to_string()]);
        let err = default_folder(&mut client, &spec, &mock_config())
            .expect_err("two folders must be ambiguous");
        assert!(err.to_string().contains("ambiguous"), "{}", err);
        // Qualified selections need no default folder, so they still work.
        let groups =
            selection_groups(&mut client, &spec, &mock_config(), &sels(&["Trash::2"]), None).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].folder, "Trash");
    }

    #[test]
    fn selections_group_by_folder_in_first_named_order() {
        let mut client = mock_client();
        let spec = FolderSpec::default();
        let groups = selection_groups(&mut client, &spec, &mock_config(), &sels(&["Archive::2", "5", "Archive::3,2", "1"]), None)
        .unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].folder, "Archive");
        assert_eq!(groups[0].uids, vec![2, 3]); // deduplicated
        assert_eq!(groups[1].folder, "INBOX");
        assert_eq!(groups[1].uids, vec![5, 1]); // input order kept
        assert_eq!(
            flatten(&groups),
            vec![("Archive", 2), ("Archive", 3), ("INBOX", 5), ("INBOX", 1)]
        );
    }

    #[test]
    fn ranges_resolve_against_the_folders_uid_list() {
        let mut client = mock_client();
        let spec = FolderSpec::default();
        // The mock holds UIDs 1..=5.
        let groups =
            selection_groups(&mut client, &spec, &mock_config(), &sels(&["2-4"]), None).unwrap();
        assert_eq!(groups[0].uids, vec![2, 3, 4]);
        let groups = selection_groups(&mut client, &spec, &mock_config(), &sels(&["*"]), None).unwrap();
        assert_eq!(groups[0].uids, vec![1, 2, 3, 4, 5]);
        let groups =
            selection_groups(&mut client, &spec, &mock_config(), &sels(&["4-*"]), None).unwrap();
        assert_eq!(groups[0].uids, vec![4, 5]);
    }

    #[test]
    fn a_recency_count_applies_to_every_selected_folder() {
        let mut client = mock_client();
        let spec = FolderSpec::new(vec!["INBOX".to_string(), "Trash".to_string()]);
        let groups = selection_groups(
            &mut client,
            &spec,
            &mock_config(),
            &[],
            Some(select::UidItem::Last(2)),
        )
        .unwrap();
        // Two folders, the two newest of each — and no ambiguity error,
        // because a count names no UID.
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].folder, "INBOX");
        assert_eq!(groups[0].uids, vec![4, 5]);
        assert_eq!(groups[1].folder, "Trash");
        assert_eq!(groups[1].uids, vec![4, 5]);
    }

    #[test]
    fn a_recency_count_refuses_to_share_with_an_explicit_selection() {
        let mut client = mock_client();
        let err = selection_groups(
            &mut client,
            &FolderSpec::default(),
            &mock_config(),
            &sels(&["3"]),
            Some(select::UidItem::Last(2)),
        )
        .expect_err("a count and a selection are exclusive");
        assert!(err.to_string().contains("cannot be combined"), "{}", err);
    }

    #[test]
    fn a_range_past_the_end_of_the_mailbox_matches_nothing() {
        let mut client = mock_client();
        let spec = FolderSpec::default();
        let err = selection_groups(&mut client, &spec, &mock_config(), &sels(&["99-*"]), None)
            .expect_err("must not fall back to the last message");
        assert!(err.to_string().contains("matched no message"), "{}", err);
    }

    #[test]
    fn part_save_refuses_a_selection_naming_several_messages() {
        let config = mock_config();
        let err = parts_save(
            &config,
            &FolderSpec::default(),
            &select::parse_selection("1,2").unwrap(),
            1,
            None,
            false,
            false,
        )
        .expect_err("part save takes exactly one message");
        assert!(err.to_string().contains("one part of one message"), "{}", err);
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
            &["\\seen".into(), "\\FLAGGED".into(), "\\seen".into()],
            true,
            false,
        )
        .unwrap();
        assert_eq!(v, vec!["\\Seen", "\\Flagged"]);
    }

    #[test]
    fn a_convention_keyword_is_glossed_for_a_reader() {
        let flags = vec![
            "\\Seen".to_string(),
            "$label1".to_string(),
            "invoice".to_string(),
            "r&AOk-gie".to_string(),
        ];
        let shown = gloss(&flags);
        assert_eq!(shown[0], "\\Seen", "a system flag needs no gloss");
        assert!(shown[1].starts_with("$label1 (Thunderbird tag 1"), "{}", shown[1]);
        assert_eq!(shown[2], "invoice", "an unknown keyword is left alone");
        assert_eq!(
            shown[3], "r&AOk-gie (\"régie\", modified UTF-7)",
            "an encoded keyword is unreadable until it is decoded"
        );
    }

    #[test]
    fn a_thunderbird_key_is_read_back() {
        let flags = vec!["r=c3=a9gie".to_string(), "my=20tag".to_string()];
        let shown = gloss(&flags);
        assert_eq!(shown[0], "r=c3=a9gie (\"régie\", Thunderbird tag key)");
        assert_eq!(shown[1], "my=20tag (\"my tag\", Thunderbird tag key)");
    }

    #[test]
    fn non_ascii_keywords_go_out_as_modified_utf7() {
        // An IMAP atom is ASCII, so this is correctness, not a nicety:
        // sending "régie" raw would be a malformed command.
        assert_eq!(
            parse_flag_names(&["régie".into()], false, false).unwrap(),
            vec!["r&AOk-gie"]
        );
        // Every '&' is escaped, including one that would have decoded:
        // whether 'pen&ink-notes' is a wire key or two words with an
        // ampersand cannot be told from the name, so the default is
        // literal and --wire is how a wire key is sent.
        assert_eq!(
            parse_flag_names(&["R&D".into()], false, false).unwrap(),
            vec!["R&-D"]
        );
        assert_eq!(
            parse_flag_names(&["pen&ink-notes".into()], false, false).unwrap(),
            vec!["pen&-ink-notes"]
        );
        assert_eq!(
            parse_flag_names(&["r&AOk-gie".into()], false, false).unwrap(),
            vec!["r&-AOk-gie"],
            "literal by default"
        );
        assert_eq!(
            parse_flag_names(&["r&AOk-gie".into()], false, true).unwrap(),
            vec!["r&AOk-gie"],
            "--wire sends the atom as given"
        );
    }

    #[test]
    fn the_fold_is_on_the_decoded_text_and_ascii_only() {
        // 'r&AOk-gie' is "régie" and 'r&aok-gie' is "r檉gie": base64 is
        // case-sensitive, so as wire keys these are two keywords.
        assert_eq!(
            parse_flag_names(&["r&AOk-gie".into(), "r&aok-gie".into()], false, true).unwrap(),
            vec!["r&AOk-gie", "r&aok-gie"]
        );
        assert!(!same_keyword("r&AOk-gie", "r&aok-gie"));
        // ... but an ampersand elsewhere in a name must not switch the
        // fold off: these are two spellings of one ASCII word.
        assert!(same_keyword("R&-D", "r&-d"));
        assert!(same_keyword("Invoice", "invoice"));
        assert_eq!(
            parse_flag_names(&["R&D".into(), "r&d".into()], false, false).unwrap(),
            vec!["R&-D"]
        );
        assert_eq!(
            parse_flag_names(&["Régie".into(), "régie".into()], false, false).unwrap(),
            vec!["R&AOk-gie"],
            "the accent is the same letter; only its case differs"
        );
        // Non-ASCII case is left alone: IMAP asks for no Unicode fold.
        assert!(!same_keyword("r&AOk-gie", "r&AMk-gie"));
    }

    #[test]
    fn a_comma_is_a_legal_atom_character() {
        // Refusing it would refuse every modified UTF-7 keyword whose
        // base64 happens to contain one.
        assert_eq!(
            parse_flag_names(&["&U,BTFw-".into()], false, true).unwrap(),
            vec!["&U,BTFw-"]
        );
        for bad in ["a(b", "a)b", "a{b", "a b", "a%b", "a*b", "a\"b", "a]b"] {
            assert!(
                parse_flag_names(&[bad.to_string()], false, false).is_err(),
                "should reject {:?}",
                bad
            );
        }
    }

    #[test]
    fn decomposed_names_are_composed_before_encoding() {
        // "régie" typed on a Mac can arrive as e + U+0301. Encoded as
        // it stands it becomes a different atom that looks identical in
        // every listing, and a later `tag remove` spelled the other way
        // would silently miss it.
        let nfd = "re\u{301}gie".to_string();
        let nfc = "régie".to_string();
        assert_ne!(nfd, nfc, "the two inputs really are different strings");
        assert_eq!(
            parse_flag_names(std::slice::from_ref(&nfd), false, false).unwrap(),
            vec!["r&AOk-gie"]
        );
        assert_eq!(
            parse_flag_names(&[nfd, nfc], false, false).unwrap(),
            vec!["r&AOk-gie"],
            "and the two spellings are one keyword"
        );
    }

    #[test]
    fn wire_mode_refuses_what_cannot_be_an_atom() {
        // --wire says "this is already an atom", and an atom is ASCII —
        // which also makes composition moot there: no decomposed name
        // can reach wire mode in the first place.
        let err = parse_flag_names(&["régie".into()], false, true)
            .expect_err("an atom is ASCII");
        assert!(err.to_string().contains("r&AOk-gie"), "{}", err);
        let nfd = "re\u{301}gie".to_string();
        assert!(parse_flag_names(std::slice::from_ref(&nfd), false, true).is_err());
        // An ASCII name still goes through untouched.
        assert_eq!(
            parse_flag_names(&["r&AOk-gie".into()], false, true).unwrap(),
            vec!["r&AOk-gie"]
        );
    }

    #[test]
    fn invisible_characters_are_refused() {
        // They pass the atom check (neither control nor ASCII space) and
        // would make a keyword no listing can tell from its twin.
        for bad in ["\u{feff}invoice", "my\u{a0}tag", "in\u{200b}voice", "tag\u{ad}"] {
            assert!(
                parse_flag_names(&[bad.to_string()], false, false).is_err(),
                "should refuse {:?}",
                bad
            );
        }
        assert!(is_invisible('\u{a0}') && is_invisible('\u{feff}') && is_invisible(' '));
        assert!(!is_invisible('a') && !is_invisible('é') && !is_invisible('$'));
    }

    #[test]
    fn keywords_deduplicate_case_insensitively() {
        // Two spellings of one keyword are never what the user meant:
        // on a case-folding server it is redundant, and on a
        // case-keeping one it silently creates two keywords.
        assert_eq!(
            parse_flag_names(&["Invoice".into(), "invoice".into()], false, false).unwrap(),
            vec!["Invoice"]
        );
        assert_eq!(
            parse_flag_names(&["\\seen".into(), "\\SEEN".into()], true, false).unwrap(),
            vec!["\\Seen"]
        );
    }

    #[test]
    fn registered_keywords_go_out_in_their_registered_spelling() {
        assert_eq!(
            parse_flag_names(&["$important".into(), "$MDNSENT".into()], false, false).unwrap(),
            vec!["$Important", "$MDNSent"]
        );
        // An unregistered keyword is passed through exactly as typed.
        assert_eq!(
            parse_flag_names(&["MyTag".into()], false, false).unwrap(),
            vec!["MyTag"]
        );
    }

    #[test]
    fn jmap_keyword_spellings_of_system_flags_are_refused() {
        // The registry is shared with JMAP, which writes four IMAP
        // system flags as '$'-keywords. Storing '$seen' as a tag would
        // set something no IMAP client reads.
        for (jmap, imap) in [("$seen", "\\Seen"), ("$draft", "\\Draft")] {
            let err = parse_flag_names(&[jmap.into()], false, false).expect_err(jmap);
            assert!(err.to_string().contains(imap), "{}: {}", jmap, err);
        }
    }

    #[test]
    fn flag_and_tag_own_one_kind_each() {
        // 'flag' is the IMAP-defined set, 'tag' the user-defined one;
        // neither accepts the other's names. This is also what stops
        // `flag add 5 -- '\Deleted' Trash` storing a keyword "Trash".
        assert!(parse_flag_names(&["junk".into()], true, false).is_err());
        assert!(parse_flag_names(&["Trash".into()], true, false).is_err());
        assert!(parse_flag_names(&["\\Seen".into()], false, false).is_err());
        assert_eq!(
            parse_flag_names(&["\\Deleted".into()], true, false).unwrap(),
            vec!["\\Deleted"]
        );
        assert_eq!(
            parse_flag_names(&["invoice".into()], false, false).unwrap(),
            vec!["invoice"]
        );
    }

    #[test]
    fn parse_flag_names_rejects_recent_and_unknown_system_flags() {
        assert!(parse_flag_names(&["\\Recent".into()], true, false).is_err());
        assert!(parse_flag_names(&["\\Bogus".into()], true, false).is_err());
        assert!(parse_flag_names(&["\\".into()], true, false).is_err());
    }

    #[test]
    fn parse_flag_names_tag_mode_rejects_system_flags() {
        assert!(parse_flag_names(&["\\Seen".into()], false, false).is_err());
        let v = parse_flag_names(&["$Important".into(), "my-tag".into()], false, false).unwrap();
        assert_eq!(v, vec!["$Important", "my-tag"]);
    }

    #[test]
    fn parse_flag_names_rejects_special_characters() {
        for bad in ["a b", "a,b", "a*b", "a%b", "a(b", "a}b", "\"x\"", "a\nb", "-mytag", "-j"] {
            assert!(
                parse_flag_names(&[bad.to_string()], true, false).is_err(),
                "should reject {:?}",
                bad
            );
        }
        assert!(parse_flag_names(&[], true, false).is_err());
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
