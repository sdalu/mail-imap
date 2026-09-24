pub mod keywords;
pub mod modutf7;
pub mod select;
pub mod tbkey;

use crate::config::Config;
use crate::imap::{FolderInfo, ImapBackend, ImapClient, Mailbox, PartInfo, SearchResult};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, FixedOffset};
use serde::Serialize;
use select::Selection;
use unicode_normalization::UnicodeNormalization;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The folders a command works on, as given on the command line:
/// literal names, IMAP `LIST` patterns, or nothing at all (in which
/// case the `folder` of the config is used).
/// `PartialEq` so a caller can check what was asked for against a spec
/// built the public way (`FolderSpec::new`), rather than this having to
/// expose the vector it keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
        // Same reason as `Selection::resolve`'s own set: input order is
        // the contract, but asking "seen already" must not be a scan of
        // everything collected so far.
        let mut uids: Vec<u32> = Vec::new();
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for selection in &selections {
            for uid in selection.resolve(available.as_deref())? {
                if seen.insert(uid) {
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
fn emit_json(out: &mut dyn Write, value: &impl Serialize) -> Result<()> {
    writeln!(out, "{}", serde_json::to_string(value)?)?;
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
    /// Which leaf `content`'s body came from: `"text"`, `"html"`,
    /// `"raw"` or `"none"` -- see `mime::render_message`.
    source: &'a str,
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
    /// What the junk family says, across all five spellings, when it
    /// says anything: "junk", "not-junk" or "contradictory".
    #[serde(skip_serializing_if = "Option::is_none")]
    junk: Option<&'a str>,
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
struct MoveOutput<'a> {
    folder: &'a str,
    to: &'a str,
    count: usize,
    uids: &'a [u32],
}

#[derive(Serialize)]
struct CopyOutput<'a> {
    folder: &'a str,
    to: &'a str,
    count: usize,
    uids: &'a [u32],
}

#[derive(Serialize)]
struct ExpungeOutput<'a> {
    folder: &'a str,
    /// How many of the selected UIDs were actually eligible (already
    /// marked `\Deleted`) and so removed -- not how many were named.
    count: usize,
    uids: &'a [u32],
}

#[derive(Serialize)]
struct AppendOutput<'a> {
    folder: &'a str,
    bytes: usize,
    flags: &'a [String],
    /// The UID the server assigned, when it advertises `UIDPLUS` and
    /// reported one; `null` otherwise -- the caller then has no handle
    /// on the message it just created.
    uid: Option<u32>,
}

#[derive(Serialize)]
struct FolderChangeOutput<'a> {
    action: &'a str,
    folder: &'a str,
    /// The new name, for `rename`.
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<&'a str>,
    /// The RFC 6154 attribute declared at creation, for `create`.
    #[serde(rename = "use", skip_serializing_if = "Option::is_none")]
    use_attr: Option<&'a str>,
}

#[derive(Serialize)]
struct PartsSaveOutput<'a> {
    folder: &'a str,
    uid: u32,
    part: u32,
    file: &'a str,
    size: u64,
}

/// Render flag names for a reader.
///
/// `wire` prints the atoms exactly as the server sent them, which is
/// what `--wire` on `add`/`remove` takes back. Otherwise each name is
/// shown in the form you would type it without `--wire`: a system flag
/// as the bare word `flag add` wants, a modified UTF-7 keyword as the
/// text it encodes — both of which re-encode to the same atom. What
/// cannot round-trip is glossed instead of rewritten: a convention
/// keyword gets its meaning, and a Thunderbird key its text, because
/// typing either back would produce a different atom.
///
/// JSON always carries the wire form; a machine wants the atom.
fn render_names(flags: &[String], wire: bool) -> Vec<String> {
    if wire {
        return flags.to_vec();
    }
    flags
        .iter()
        .map(|f| {
            if let Some(bare) = bare_system_flag(f) {
                return bare.to_string();
            }
            if let Some(means) = keywords::listing_meaning(f) {
                return format!("{} ({})", f, means);
            }
            if let Some(text) = modutf7::decoded_display(f) {
                return text;
            }
            if let Some(text) = tbkey::decode(f) {
                return format!("{} (\"{}\", Thunderbird tag key)", f, text);
            }
            f.clone()
        })
        .collect()
}

/// The bare word `flag` takes for a system flag the server sent.
fn bare_system_flag(name: &str) -> Option<String> {
    let rest = name.strip_prefix('\\')?;
    let lower = rest.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "seen" | "answered" | "flagged" | "deleted" | "draft" | "recent"
    )
    .then_some(lower)
}

/// Print the keywords the tool knows about. Needs no server: both
/// tables live in the binary (`src/cli/keywords.rs`).
pub fn tags_known(out: &mut dyn Write, json: bool) -> Result<()> {
    let registered: Vec<KnownKeyword> = keywords::registered()
        .iter()
        .map(|(k, m)| KnownKeyword {
            keyword: k,
            means: Some(m),
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
        return emit_json(out, &KnownOutput {
            count: registered.len() + well_known.len(),
            registered: &registered,
            well_known: &well_known,
        });
    }
    writeln!(out, "IANA-registered keywords ({}):", registered.len())?;
    for k in &registered {
        writeln!(out, "  {:<17} {}", k.keyword, k.means.unwrap_or(""))?;
    }
    writeln!(out)?;
    writeln!(out, "Well known, but registered nowhere ({}):",
        well_known.len()
    )?;
    for k in &well_known {
        writeln!(out, "  {:<17} {}", k.keyword, k.means.unwrap_or(""))?;
    }
    writeln!(out)?;
    writeln!(out, "Any other atom is a valid keyword too; these are the ones with an agreed meaning.")?;
    Ok(())
}

/// List mailboxes.
///
/// `long` adds the hierarchy delimiter and the LIST attributes to each
/// line. They are off by default because the common use of this
/// command is to find out what a folder is *called* so it can be typed
/// back into `-f`, and a name buried in parentheses is harder to read
/// off and harder to copy. JSON ignores `long` and always carries
/// every field: it is read by a caller, which cannot ask again.
pub fn list_folders(out: &mut dyn Write,
    config: &Config,
    json: bool,
    debug: bool,
    long: bool,
    subscribed: bool,
) -> Result<()> {
    if debug {
        eprintln!("Connecting to {}:{} as {}", config.server, config.port, config.username);
    }
    let mut client = ImapClient::connect(config, debug)?;
    if debug {
        eprintln!("Listing {}folders...", if subscribed { "subscribed " } else { "" });
    }
    let folders = if subscribed {
        client.list_subscribed_folders()?
    } else {
        client.list_folders()?
    };
    if debug {
        eprintln!("Found {} folder(s)", folders.len());
    }

    if json {
        emit_json(out, &FoldersOutput {
            count: folders.len(),
            folders: &folders,
        })?;
        return Ok(());
    }

    if folders.is_empty() {
        writeln!(out, "{}", if subscribed { "No subscribed folders found." } else { "No folders found." })?;
        return Ok(());
    }

    writeln!(out, "{} ({}):", if subscribed { "Subscribed folders" } else { "Folders" }, folders.len())?;
    for f in folders {
        writeln!(out, "{}", folder_line(&f, long))?;
    }
    Ok(())
}

/// One line of the `folder` listing.
///
/// Without `long` this is the name and nothing else, so it can be read
/// off and typed straight back into `-f`. With it, the hierarchy
/// delimiter and the LIST attributes follow in parentheses. A mailbox
/// the server described with neither still prints as a bare name: an
/// empty `()` would say something was withheld.
fn folder_line(f: &FolderInfo, long: bool) -> String {
    if !long {
        return format!("  - {}", f.name);
    }
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
    if meta.is_empty() {
        format!("  - {}", f.name)
    } else {
        format!("  - {} ({})", f.name, meta.join(" "))
    }
}

// ---------------------------------------------------------------- info

/// What `info` reports about the tool and the account, in the order a
/// caller needs it: what this build is, what it is allowed to change,
/// how to spell a folder path, and which wire path each operation will
/// take on this server.
#[derive(Serialize)]
struct InfoOutput<'a> {
    tool: ToolInfo<'a>,
    config: ConfigInfo<'a>,
    access: AccessInfo<'a>,
    folders: FoldersInfo<'a>,
    defaults: DefaultsInfo<'a>,
    server: ServerInfo,
}

#[derive(Serialize)]
struct ToolInfo<'a> {
    name: &'a str,
    version: &'a str,
    /// `real` or `mock`.
    backend: &'a str,
}

#[derive(Serialize)]
struct ConfigInfo<'a> {
    /// The file the settings came from, or `null` when none was read
    /// (`--mock` without a config).
    path: Option<&'a str>,
    /// The profile the settings came from, or `null` when the file
    /// names none.
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<&'a str>,
    server: &'a str,
    port: u16,
    /// `implicit`, `starttls` or `none`.
    tls: &'a str,
    insecure: bool,
    /// How this run authenticates: `login` or `xoauth2`. Under
    /// `xoauth2` the secret is an OAuth 2 access token.
    auth: &'a str,
    username: &'a str,
}

#[derive(Serialize)]
struct AccessInfo<'a> {
    /// What this run may do: the config's level, narrowed by
    /// `--access-level`.
    effective: &'a str,
    /// The ceiling the config sets.
    configured: &'a str,
    may: AccessMay,
}

#[derive(Serialize)]
struct AccessMay {
    store_flags: bool,
    move_messages: bool,
    /// Same rung as `move_messages`: the message keeps existing, one
    /// more copy of it appears.
    copy_messages: bool,
    change_folders: bool,
    delete_folders: bool,
    strip_part: bool,
    set_deleted: bool,
    /// `UID EXPUNGE` a message already marked `\Deleted`.
    expunge: bool,
    /// `APPEND` a message into a mailbox.
    append: bool,
}

#[derive(Serialize)]
struct FoldersInfo<'a> {
    /// The delimiter to build a path with, or `null` where there is
    /// none to be had.
    delimiter: Option<String>,
    /// `config`, `server` or `none`.
    delimiter_source: &'a str,
    /// What the mailbox list itself reported, kept even when a config
    /// entry overrides it: the two disagreeing is worth seeing.
    server_delimiter: Option<String>,
    /// Every distinct delimiter the mailbox list reported: more than
    /// one means more than one namespace.
    delimiters_seen: Vec<String>,
    /// The folder a command uses when `-f` names none.
    default: &'a str,
    default_exists: bool,
    count: usize,
    /// RFC 6154 special use -> the mailbox that carries it.
    special_use: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct DefaultsInfo<'a> {
    max: usize,
    sort: Option<&'a str>,
}

#[derive(Serialize)]
struct ServerInfo {
    capabilities: Vec<String>,
    /// The path `move` will take: `UID MOVE`, `UID COPY + UID EXPUNGE`,
    /// or `refused`.
    filing: &'static str,
    /// The path `expunge` will take: `UID EXPUNGE`, or `refused`
    /// where the server has no UIDPLUS and a plain `EXPUNGE` would
    /// take the whole mailbox's `\Deleted` set rather than the named
    /// messages.
    expunging: &'static str,
    /// `server` or `client`, for `-S` and for `thread`.
    sorting: &'static str,
    threading: &'static str,
    /// Whether `folder create --use` can be honoured.
    create_special_use: bool,
}

/// `info`: everything a caller would otherwise have to guess — the
/// access level in force, the hierarchy delimiter, and which wire path
/// each operation takes on this server.
pub fn info(
    out: &mut dyn Write,
    config: &Config,
    config_path: Option<&str>,
    profile: Option<&str>,
    configured: crate::config::AccessLevel,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let report = build_info(&mut client, config, config_path, profile, configured)?;
    if json {
        return emit_json(out, &report);
    }
    print_info(out, &report)?;
    Ok(())
}

/// The payload, built from an open connection — separate from `info`
/// so the suite can read it rather than the printed page.
fn build_info<'a>(
    client: &mut ImapClient,
    config: &'a Config,
    config_path: Option<&'a str>,
    profile: Option<&'a str>,
    configured: crate::config::AccessLevel,
) -> Result<InfoOutput<'a>> {
    let caps = client.capabilities()?;
    let has = |c: &str| caps.iter().any(|x| x.eq_ignore_ascii_case(c));
    let folders = client.list_folders()?;

    // The delimiter is per mailbox on the wire and per namespace in
    // practice, so report every one seen and name the one INBOX uses
    // as the one to build a path with. A config entry overrides it:
    // the operator saying so beats a server that says NIL.
    let mut delimiters_seen: Vec<String> = Vec::new();
    for f in &folders {
        if let Some(d) = &f.delimiter {
            if !d.is_empty() && !delimiters_seen.contains(d) {
                delimiters_seen.push(d.clone());
            }
        }
    }
    let from_server = folders
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case("INBOX"))
        .and_then(|f| f.delimiter.clone())
        .filter(|d| !d.is_empty())
        .or_else(|| delimiters_seen.first().cloned());
    let (delimiter, delimiter_source) = match (&config.delimiter, &from_server) {
        (Some(d), _) => (Some(d.clone()), "config"),
        (None, Some(d)) => (Some(d.clone()), "server"),
        (None, None) => (None, "none"),
    };

    let mut special_use: BTreeMap<String, String> = BTreeMap::new();
    for f in &folders {
        for attr in &f.attrs {
            if special_use_name(attr).is_some() {
                special_use.insert(attr.clone(), f.name.clone());
            }
        }
    }

    let level = config.access;
    Ok(InfoOutput {
        tool: ToolInfo {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
            backend: if client.is_mock() { "mock" } else { "real" },
        },
        config: ConfigInfo {
            path: config_path,
            profile,
            server: &config.server,
            port: config.port,
            tls: if config.ssl {
                "implicit"
            } else if config.starttls {
                "starttls"
            } else {
                "none"
            },
            insecure: config.insecure,
            auth: config.auth.as_str(),
            username: &config.username,
        },
        access: AccessInfo {
            effective: level.as_str(),
            configured: configured.as_str(),
            may: AccessMay {
                store_flags: level.may_store_flags(),
                move_messages: level.may_move(),
                copy_messages: level.may_move(),
                change_folders: level.may_change_folders(),
                delete_folders: level.may_delete_folder(),
                strip_part: level.may_strip_part(),
                set_deleted: level.may_set("\\Deleted"),
                expunge: level.may_expunge(),
                append: level.may_append(),
            },
        },
        folders: FoldersInfo {
            delimiter,
            delimiter_source,
            server_delimiter: from_server,
            delimiters_seen,
            default: &config.folder,
            default_exists: folders
                .iter()
                .any(|f| same_folder(&f.name, &config.folder)),
            count: folders.len(),
            special_use,
        },
        defaults: DefaultsInfo {
            max: config.max,
            sort: config.sort.as_deref(),
        },
        server: ServerInfo {
            filing: if has("MOVE") {
                "UID MOVE"
            } else if has("UIDPLUS") {
                "UID COPY + UID EXPUNGE"
            } else {
                "refused"
            },
            expunging: if has("UIDPLUS") {
                "UID EXPUNGE"
            } else {
                "refused"
            },
            sorting: if has("SORT") { "server" } else { "client" },
            threading: if has("THREAD=REFERENCES") {
                "server"
            } else {
                "client"
            },
            create_special_use: has("CREATE-SPECIAL-USE"),
            capabilities: caps,
        },
    })
}

/// Is this the same mailbox name? INBOX is the one name IMAP defines
/// as case-insensitive.
fn same_folder(a: &str, b: &str) -> bool {
    a == b || (a.eq_ignore_ascii_case("INBOX") && b.eq_ignore_ascii_case("INBOX"))
}

/// Whether an attribute is one of the seven RFC 6154 special uses.
fn special_use_name(attr: &str) -> Option<&'static str> {
    special_use(attr.trim_start_matches('\\'))
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn print_info(out: &mut dyn Write, i: &InfoOutput) -> Result<()> {
    writeln!(out, "{} {} ({} backend)", i.tool.name, i.tool.version, i.tool.backend)?;
    writeln!(out, "  config      {}",
        i.config.path.unwrap_or("(none read: built-in defaults)")
    )?;
    // Only when there is one: a line saying "profile (none)" on every
    // single-account config is noise on the common case.
    if let Some(p) = i.config.profile {
        writeln!(out, "  profile     {}", p)?;
    }
    writeln!(out, "  account     {}@{}:{} ({}{})",
        i.config.username,
        i.config.server,
        i.config.port,
        match i.config.tls {
            "implicit" => "implicit TLS",
            "starttls" => "STARTTLS",
            _ => "no TLS",
        },
        if i.config.insecure {
            ", certificate checks off"
        } else {
            ""
        }
    )?;
    writeln!(out, "  auth        {}", i.config.auth)?;

    writeln!(out)?;
    writeln!(out, "Access level: {}", i.access.effective)?;
    if i.access.effective != i.access.configured {
        writeln!(out, "  (narrowed for this run; the config allows {})", i.access.configured)?;
    }
    // Width, not hand-counted spaces: the labels change, the column
    // should not have to be re-counted when they do.
    const MAY: usize = 35;
    writeln!(out, "  {:<MAY$} {}", "set and clear flags and tags", yes_no(i.access.may.store_flags))?;
    writeln!(out, "  {:<MAY$} {}", "move mail to another folder", yes_no(i.access.may.move_messages))?;
    writeln!(out, "  {:<MAY$} {}", "copy mail into another folder", yes_no(i.access.may.copy_messages))?;
    writeln!(out, "  {:<MAY$} {}", "create / rename / subscribe", yes_no(i.access.may.change_folders))?;
    writeln!(out, "  {:<MAY$} {}", "delete a folder", yes_no(i.access.may.delete_folders))?;
    writeln!(out, "  {:<MAY$} {}", "strip a part from a message", yes_no(i.access.may.strip_part))?;
    writeln!(out, "  {:<MAY$} {}", "set \\Deleted", yes_no(i.access.may.set_deleted))?;
    writeln!(out, "  {:<MAY$} {}", "expunge a \\Deleted message", yes_no(i.access.may.expunge))?;
    writeln!(out, "  {:<MAY$} {}", "append a message into a mailbox", yes_no(i.access.may.append))?;

    writeln!(out)?;
    writeln!(out, "Folders")?;
    match &i.folders.delimiter {
        Some(d) => writeln!(out, "  delimiter   '{}' (from the {}){}{}",
            d,
            i.folders.delimiter_source,
            match &i.folders.server_delimiter {
                Some(s) if s != d => format!("; the server reports '{}'", s),
                None if i.folders.delimiter_source == "config" =>
                    "; the server reports none".to_string(),
                _ => String::new(),
            },
            if i.folders.delimiters_seen.len() > 1 {
                format!(
                    " -- the list also reports {}, so this account has more than one namespace",
                    i.folders
                        .delimiters_seen
                        .iter()
                        .map(|d| format!("'{}'", d))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                String::new()
            }
        )?,
        None => writeln!(out, "  delimiter   none reported: this account has no hierarchy")?,
    }
    writeln!(out, "  default     {}{}",
        i.folders.default,
        if i.folders.default_exists {
            ""
        } else {
            "  -- NOT in the mailbox list"
        }
    )?;
    writeln!(out, "  mailboxes   {}", i.folders.count)?;
    for (attr, name) in &i.folders.special_use {
        writeln!(out, "  {:<11} {}", attr, name)?;
    }

    writeln!(out)?;
    writeln!(out, "Search defaults")?;
    writeln!(out, "  max         {}",
        if i.defaults.max == 0 {
            "unlimited".to_string()
        } else {
            i.defaults.max.to_string()
        }
    )?;
    writeln!(out, "  sort        {}",
        i.defaults.sort.unwrap_or("(none: most recent first)")
    )?;

    writeln!(out)?;
    writeln!(out, "This server")?;
    writeln!(out, "  {:<19} {}", "moving mail", i.server.filing)?;
    writeln!(out, "  {:<19} {}", "expunging", i.server.expunging)?;
    writeln!(out, "  {:<19} {}-side", "sorting (-S)", i.server.sorting)?;
    writeln!(out, "  {:<19} {}-side", "threading", i.server.threading)?;
    writeln!(out, "  {:<19} {}",
        "folder create --use",
        if i.server.create_special_use {
            "available"
        } else {
            "refused (no CREATE-SPECIAL-USE)"
        }
    )?;
    writeln!(out, "  {:<19} {}",
        "advertises",
        if i.server.capabilities.is_empty() {
            "(nothing)".to_string()
        } else {
            i.server.capabilities.join(" ")
        }
    )?;
    Ok(())
}

/// `folder create|rename|subscribe|unsubscribe|delete`. Every one of
/// these is gated in `ImapClient` -- `restructure` for the first four,
/// `full` for `delete` -- the handler only reports what happened.
pub fn folder_create(out: &mut dyn Write,
    config: &Config,
    name: &str,
    use_attr: Option<&str>,
    wire_form: bool,
    json: bool,
    debug: bool,
) -> Result<()> {
    // Normalized before the connection is opened: a name the tool will
    // refuse is worth refusing without a login first.
    let use_attr = parse_use_attr(use_attr, wire_form)?;
    let use_attr = use_attr.as_deref();
    let mut client = ImapClient::connect(config, debug)?;
    client.create_folder(name, use_attr)?;
    if json {
        return emit_json(out, &FolderChangeOutput {
            action: "create",
            folder: name,
            to: None,
            use_attr,
        });
    }
    match use_attr {
        Some(attr) => writeln!(out, "Created '{}' with special use {}", name, attr)?,
        None => writeln!(out, "Created '{}'", name)?,
    }
    Ok(())
}

pub fn folder_rename(out: &mut dyn Write, config: &Config, from: &str, to: &str, json: bool, debug: bool) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    client.rename_folder(from, to)?;
    if json {
        return emit_json(out, &FolderChangeOutput {
            action: "rename",
            folder: from,
            to: Some(to),
            use_attr: None,
        });
    }
    writeln!(out, "Renamed '{}' to '{}'", from, to)?;
    Ok(())
}

pub fn folder_subscribe(out: &mut dyn Write,
    config: &Config,
    name: &str,
    subscribed: bool,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    client.set_subscribed(name, subscribed)?;
    if json {
        return emit_json(out, &FolderChangeOutput {
            action: if subscribed { "subscribe" } else { "unsubscribe" },
            folder: name,
            to: None,
            use_attr: None,
        });
    }
    writeln!(out, "{} '{}'",
        if subscribed { "Subscribed to" } else { "Unsubscribed from" },
        name
    )?;
    Ok(())
}

pub fn folder_delete(out: &mut dyn Write, config: &Config, name: &str, force: bool, json: bool, debug: bool) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    client.delete_folder(name, force)?;
    if json {
        return emit_json(out, &FolderChangeOutput {
            action: "delete",
            folder: name,
            to: None,
            use_attr: None,
        });
    }
    writeln!(out, "Deleted '{}'", name)?;
    Ok(())
}

pub fn search_emails(out: &mut dyn Write,
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
        emit_json(out, &SearchOutput {
            folder: folders.first().filter(|_| folders.len() == 1).map(|s| s.as_str()),
            folders: if folders.len() == 1 { None } else { Some(&folders) },
            query,
            count: results.len(),
            results: &results,
        })?;
        return Ok(());
    }

    if results.is_empty() {
        writeln!(out, "No emails matched query: {}", query)?;
        return Ok(());
    }

    // One date column, two dates on the message: it follows the sort,
    // so the column a reader checks the order against is the one the
    // order was made from.
    let show_sent = sort.as_ref().is_some_and(|s| s.leads_with_sent_date());

    if folders.len() == 1 {
        writeln!(out, "Found {} email(s) in '{}' matching: {}",
            results.len(),
            folders[0],
            query
        )?;
        for r in &results {
            print_search_result(out, "  ", r, show_sent)?;
        }
    } else {
        writeln!(out, "Found {} email(s) in {} folder(s) matching: {}",
            results.len(),
            folders.len(),
            query
        )?;
        for folder in &folders {
            let hits: Vec<&SearchResult> = results
                .iter()
                .filter(|r| &r.folder == folder)
                .collect();
            if hits.is_empty() {
                continue;
            }
            writeln!(out, "  {} ({}):", folder, hits.len())?;
            for r in hits {
                print_search_result(out, "    ", r, show_sent)?;
            }
        }
    }
    Ok(())
}

/// The date cell of a result line: one of the message's two dates,
/// always saying which.
///
/// `search` has room for one date and the message has two, so the
/// label is not decoration — without it the column is ambiguous, and
/// with the wrong one chosen it is worse: a list ordered by the sent
/// date but printed with arrival dates is correctly ordered and looks
/// scrambled, which is how this started.
fn date_cell(r: &SearchResult, sent: bool) -> String {
    if !sent {
        return format!("arrived {}", r.date.as_deref().unwrap_or("unknown"));
    }
    match r.sent.as_deref() {
        None => "sent unknown".to_string(),
        // Normalised to the arrival date's format, so the two are
        // comparable and the column lines up. A header that will not
        // parse is printed as it came: it is what the message says,
        // and it is also the explanation for why such a message sorted
        // to the front.
        Some(raw) => match crate::imap::parse_sent_date(raw) {
            Some(d) => format!("sent {}", d.format("%Y-%m-%d %H:%M:%S %z")),
            None => format!("sent {}", raw),
        },
    }
}

fn print_search_result(out: &mut dyn Write, indent: &str, r: &SearchResult, sent: bool) -> Result<()> {
    let date = date_cell(r, sent);
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
    writeln!(out, "{}UID {} | {} | {} | {}{}{}{}",
        indent, r.uid, date, r.subject, r.from, size, parts, flags
    )?;
    Ok(())
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
) -> Result<Vec<Group>> {
    // An unqualified selection that is only counts (`last:20`) names no
    // UID, so it is not ambiguous across folders the way a bare UID is:
    // it means N *per folder*, and applies to each folder -f/-A chose.
    let mut expanded: Vec<Selection> = Vec::new();
    let mut needs_default = false;
    for selection in selections {
        if selection.folder.is_none() && selection.is_count_only() {
            for folder in folders(client, spec, config)? {
                expanded.push(Selection {
                    folder: Some(folder),
                    ..selection.clone()
                });
            }
        } else {
            needs_default |= selection.folder.is_none();
            expanded.push(selection.clone());
        }
    }
    let default = if needs_default {
        default_folder(client, spec, config)?
    } else {
        String::new()
    };
    resolve_groups(client, &expanded, &default)
}

/// `move <SELECTION...> <FOLDER>`: file messages into the mailbox
/// named last. Gated in `ImapClient` on `organize`; the target folder
/// has to exist already, since making one is `restructure`'s business.
pub fn move_messages(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    to: &str,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections)?;

    // Same guard `flag`/`tag` have had since the `tag junk` defect:
    // UID MOVE ignores a UID that is not there and answers OK, so
    // without this the tool reports having filed a message that never
    // existed.
    check_uids_exist(&mut client, &groups, "UID MOVE")?;

    for group in &groups {
        client.move_messages(&group.folder, &group.uids, to)?;
        if json {
            emit_json(out, &MoveOutput {
                folder: &group.folder,
                to,
                count: group.uids.len(),
                uids: &group.uids,
            })?;
            continue;
        }
        writeln!(out, "Filed {} message(s) from '{}' into '{}': UIDs {}",
            group.uids.len(),
            group.folder,
            to,
            group
                .uids
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    Ok(())
}

/// `copy <SELECTION...> <FOLDER>`: file a copy of the selected messages
/// into the mailbox named last, leaving the originals where they are.
/// Gated in `ImapClient` on `organize`, the same as `move`: the
/// message keeps existing either way, one more copy of it appears. The
/// target folder has to exist already, for the same reason `move`'s
/// does.
pub fn copy_messages(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    to: &str,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections)?;

    check_uids_exist(&mut client, &groups, "UID COPY")?;

    for group in &groups {
        client.copy_messages(&group.folder, &group.uids, to)?;
        if json {
            emit_json(out, &CopyOutput {
                folder: &group.folder,
                to,
                count: group.uids.len(),
                uids: &group.uids,
            })?;
            continue;
        }
        writeln!(out, "Copied {} message(s) from '{}' into '{}': UIDs {}",
            group.uids.len(),
            group.folder,
            to,
            group
                .uids
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    Ok(())
}

/// `expunge <SELECTION...>`: permanently remove the selected messages
/// that already carry `\Deleted` (`UID EXPUNGE`). Gated in
/// `ImapClient` on `full`. This never sets `\Deleted` itself -- of the
/// selected UIDs, only the ones already marked are removed, and how
/// many is what gets reported; `flag add <selection> deleted` is what
/// marks a message for removal in the first place.
///
/// There is deliberately no form of this command that takes no
/// selection and expunges everything marked `\Deleted` in a mailbox --
/// that is the unbounded action `move_messages` already refuses to
/// take on a server without UIDPLUS. `search DELETED` gives the UIDs;
/// this takes them.
pub fn expunge_messages(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections)?;

    // Checked before the \Deleted test below, so that a UID which is
    // simply absent says so, rather than being told it is not marked
    // \Deleted and to go and mark it.
    check_uids_exist(&mut client, &groups, "UID EXPUNGE")?;

    for group in &groups {
        let removed = client.expunge_messages(&group.folder, &group.uids)?;
        if json {
            emit_json(out, &ExpungeOutput {
                folder: &group.folder,
                count: removed.len(),
                uids: &removed,
            })?;
            continue;
        }
        writeln!(out, "Removed {} of {} selected message(s) in '{}' (only the ones marked \\Deleted): \
             UIDs {}",
            removed.len(),
            group.uids.len(),
            group.folder,
            removed
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    Ok(())
}

/// `append <FOLDER> <FILE>`: put the message in `file` into `folder`.
/// `file` of `-` reads it from stdin. `flag_names` are parsed by
/// `parse_flag_names` -- the same spellings `flag add` takes -- and set
/// on the message as it is created; empty sets none. `date`, when
/// given, is an ISO 8601 timestamp (`2026-09-22T18:40:11+02:00`) that
/// sets INTERNALDATE; an unparseable one is refused here, naming the
/// expected shape, rather than left for the server to reject less
/// legibly. Without it, INTERNALDATE is left to the server -- RFC 3501
/// has that default to now -- and it is never taken from the message's
/// own `Date:` header: that is the sender's clock, not when this
/// mailbox received it, and conflating the two would silently misdate
/// every import. Gated in `ImapClient` on `full`, and CRLF-normalized
/// and RFC 5322 shape-checked there too, so both happen regardless of
/// backend.
///
/// `folder` must already exist: `append` does not create it, and a
/// server's `NO [TRYCREATE]` comes back naming `folder create` instead
/// of the server's own wording.
// Eight, because the writer makes it eight: the seven this command
// already needed to do its job, plus somewhere to print. Grouping them
// into a struct would move the arguments rather than remove them, and
// this is the signature `main.rs` calls once.
#[allow(clippy::too_many_arguments)]
pub fn append_message(out: &mut dyn Write,
    config: &Config,
    folder: &str,
    file: &str,
    flag_names: &[String],
    date: Option<&str>,
    json: bool,
    debug: bool,
) -> Result<()> {
    // `--flag` is optional, and no flags at creation is an ordinary,
    // valid append -- `parse_flag_names` returns an empty `Vec` for an
    // empty list rather than refusing it, so this needs no special case.
    let flags = parse_flag_names(flag_names, true, false)?;
    let internal_date: Option<DateTime<FixedOffset>> = match date {
        None => None,
        Some(d) => Some(DateTime::parse_from_rfc3339(d).map_err(|e| {
            anyhow::anyhow!(
                "--date '{}' is not ISO 8601 ({:#}): write it like \
                 2026-09-22T18:40:11+02:00",
                d,
                e
            )
        })?),
    };
    let content: Vec<u8> = if file == "-" {
        use std::io::Read;
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("reading the message from stdin")?;
        buf
    } else {
        std::fs::read(file).with_context(|| format!("reading '{}'", file))?
    };

    let mut client = ImapClient::connect(config, debug)?;
    let uid = client.append_message(folder, &content, &flags, internal_date)?;
    if json {
        return emit_json(out, &AppendOutput {
            folder,
            bytes: content.len(),
            flags: &flags,
            uid,
        });
    }
    match uid {
        Some(uid) => writeln!(out, "Appended {} byte(s) to '{}': UID {}", content.len(), folder, uid)?,
        None => writeln!(out, "Appended {} byte(s) to '{}'; the server did not report a UID (no UIDPLUS)",
            content.len(),
            folder
        )?,
    }
    Ok(())
}

pub fn read_emails(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    raw: bool,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections)?;
    let messages = flatten(&groups);
    if debug {
        eprintln!("Reading {} email(s): {:?}", messages.len(), messages);
    }

    for (i, (folder, uid)) in messages.iter().enumerate() {
        let rendered = client.read_message(folder, *uid, raw)?;
        let content = rendered.to_text();
        if json {
            emit_json(out, &ReadOutput {
                folder,
                uid: *uid,
                content: &content,
                source: rendered.source,
            })?;
        } else {
            if messages.len() > 1 && i > 0 {
                writeln!(out)?;
            }
            if messages.len() > 1 {
                writeln!(out, "--- {}::{} ---", folder, uid)?;
            }
            writeln!(out, "{}", content)?;
        }
    }
    Ok(())
}

pub fn mailbox_counts(out: &mut dyn Write, config: &Config, spec: &FolderSpec, json: bool, debug: bool) -> Result<()> {
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
        emit_json(out, &CountOutput {
            all: selected.is_none(),
            counts: &counts,
        })?;
        return Ok(());
    }

    match &selected {
        Some(folders) => writeln!(out, "Status for {}:", folders.join(", "))?,
        None => writeln!(out, "Mailbox counts:")?,
    }
    for m in &counts {
        writeln!(out, "  {}: messages={} unseen={} recent={} uidnext={} uidvalidity={}",
            m.name, m.messages, m.unseen, m.recent, m.uid_next, m.uid_validity
        )?;
    }
    Ok(())
}

pub fn folder_uids(out: &mut dyn Write, config: &Config, spec: &FolderSpec, json: bool, debug: bool) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let folders = folders(&mut client, spec, config)?;
    if debug {
        eprintln!("Listing UIDs for {:?}", folders);
    }

    for folder in &folders {
        let uids = client.folder_uids(folder)?;
        if json {
            emit_json(out, &UidsOutput {
                folder,
                count: uids.len(),
                uids: &uids,
            })?;
            continue;
        }
        if uids.is_empty() {
            writeln!(out, "No messages in '{}'", folder)?;
            continue;
        }
        let list = uids
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");
        writeln!(out, "UIDs in '{}' ({}): {}", folder, uids.len(), list)?;
    }
    Ok(())
}

pub fn thread_uids(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections)?;

    for (folder, uid) in flatten(&groups) {
        if debug {
            eprintln!("Reconstructing thread of UID {} in '{}'", uid, folder);
        }
        let uids = client.thread_uids(folder, uid)?;
        if json {
            emit_json(out, &ThreadOutput {
                folder,
                uid,
                count: uids.len(),
                uids: &uids,
            })?;
            continue;
        }
        writeln!(out, "Thread of UID {} in '{}' ({} message(s)):",
            uid,
            folder,
            uids.len()
        )?;
        writeln!(out, "  {}",
            uids.iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        )?;
    }
    Ok(())
}

pub fn unread(
    out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    json: bool,
    debug: bool,
) -> Result<()> {
    search_emails(out, config, "UNSEEN", spec, json, debug)
}

/// The IMAP-defined flag a bare word names, if it names one. The five
/// `flag` accepts; `\Recent` is not among them, being the server's.
fn system_flag(bare: &str) -> Option<&'static str> {
    match bare.to_ascii_lowercase().as_str() {
        "seen" => Some("\\Seen"),
        "answered" => Some("\\Answered"),
        "flagged" => Some("\\Flagged"),
        "deleted" => Some("\\Deleted"),
        "draft" => Some("\\Draft"),
        _ => None,
    }
}

/// The RFC 6154 special-use attribute a bare word names.
///
/// The set is closed — a server takes these seven and no others — so
/// `--use archive` is unambiguous without a sigil, and a bare word
/// needs no shell quoting where `'\Archive'` does.
fn special_use(bare: &str) -> Option<&'static str> {
    match bare.to_ascii_lowercase().as_str() {
        "all" => Some("\\All"),
        "archive" => Some("\\Archive"),
        "drafts" => Some("\\Drafts"),
        "flagged" => Some("\\Flagged"),
        "junk" => Some("\\Junk"),
        "sent" => Some("\\Sent"),
        "trash" => Some("\\Trash"),
        _ => None,
    }
}

/// Validate `folder create --use` and return the atom to send.
///
/// The rule `flag` and `tag` follow, applied to the one other place a
/// backslashed atom reaches the command line: without `--wire` a bare
/// word is taken and the wire form refused, with `--wire` the atom a
/// `folder` listing prints is taken and a bare word refused. Settling
/// on a closed set here is also what keeps an arbitrary string out of
/// the `CREATE ... (USE (...))` command line.
pub fn parse_use_attr(name: Option<&str>, wire_form: bool) -> Result<Option<String>> {
    let Some(name) = name else {
        if wire_form {
            bail!("--wire says how to read --use, and no --use was given");
        }
        return Ok(None);
    };
    let name = name.trim();
    if name.is_empty() {
        bail!("empty --use attribute");
    }
    let bare = match name.strip_prefix('\\') {
        Some(rest) if wire_form => rest,
        Some(_) => bail!(
            "'{}' is the wire form: write it as {} here, or pass --wire",
            name,
            name.trim_start_matches('\\').to_ascii_lowercase()
        ),
        None if wire_form => bail!(
            "--wire takes the form a listing prints: write '\\{}{}' or drop --wire",
            name.chars().next().unwrap_or('x').to_ascii_uppercase(),
            name.get(1..).unwrap_or("").to_ascii_lowercase()
        ),
        None => name,
    };
    match special_use(bare) {
        Some(attr) => Ok(Some(attr.to_string())),
        None => bail!(
            "'{}' is not an RFC 6154 special use: --use takes all, archive, drafts, \
             flagged, junk, sent and trash",
            name
        ),
    }
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
/// The split is what keeps a mutation honest: without it,
/// `flag add 5 -- '\Deleted' Trash` stores a keyword `Trash` on message
/// 5 and leaves the Trash folder alone.
///
/// An empty `names` is not refused here: it is not a fact about
/// parsing names, it is a precondition of the commands that need at
/// least one (`flag add`/`tag add`, refused earlier and better by
/// `split_args` in `main.rs`) and not of the one that does not
/// (`append`'s optional `--flag`, for which an empty list is an
/// ordinary append that sets no flags). An empty input returns an
/// empty `Vec`.
pub fn parse_flag_names(names: &[String], system: bool, wire_form: bool) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim();
        if name.is_empty() {
            bail!("empty flag name");
        }
        if system {
            // `flag` owns exactly five names, so they need no sigil to
            // be unambiguous — and a bare word needs no shell quoting,
            // which '\Seen' does. --wire takes the form a listing
            // prints, for a name copied straight back out of one.
            let bare = match name.strip_prefix('\\') {
                Some(rest) if wire_form => rest,
                Some(_) => bail!(
                    "'{}' is the wire form: write it as {} here, or pass --wire",
                    name,
                    name.trim_start_matches('\\').to_ascii_lowercase()
                ),
                None if wire_form => bail!(
                    "--wire takes the form a listing prints: write '\\{}{}' or drop --wire",
                    name.chars().next().unwrap_or('x').to_ascii_uppercase(),
                    name.get(1..).unwrap_or("").to_ascii_lowercase()
                ),
                None => name,
            };
            let flag = match system_flag(bare) {
                Some(f) => f,
                None if bare.eq_ignore_ascii_case("recent") => {
                    bail!("\\Recent is managed by the server and cannot be set")
                }
                None => bail!(
                    "'{}' is not an IMAP-defined flag: 'flag' takes seen, answered, \
                     flagged, deleted and draft; user-defined keywords are the 'tag' \
                     command's",
                    name
                ),
            };
            if !out.iter().any(|f| f.eq_ignore_ascii_case(flag)) {
                out.push(flag.to_string());
            }
            continue;
        }
        if let Some(rest) = name.strip_prefix('\\') {
            bail!(
                "'{}' is a system flag; tags are user-defined keywords (use \
                 'flag add ... {}')",
                name,
                rest.to_ascii_lowercase()
            );
        }
        // A keyword spelled like a system flag with the backslash
        // dropped is almost always that flag, meant for `flag`. Stored
        // as a keyword it is inert, reads like the flag in a listing,
        // and slips past the access level that governs the real one.
        if let Some(flag) = system_flag(name).or_else(|| {
            name.eq_ignore_ascii_case("recent").then_some("\\Recent")
        }) {
            bail!(
                "'{}' is how {} is written without its backslash; as a keyword it \
                 would mean nothing to any client. Use 'flag add ... {}'",
                name,
                flag,
                name.to_ascii_lowercase()
            );
        }
        if crate::cli::select::parse_selection(name)
            .map(|s| s.folder.is_none())
            .unwrap_or(false)
        {
            bail!(
                "'{}' reads as a message selection, not a tag name",
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
                "invalid keyword '{}': an IMAP atom excludes ( ) {{ % * \" \\ ] spaces \
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
        // way out. Taking a name for a wire key whenever it happens to
        // decode is undecidable from the name alone: 'pen&ink-notes'
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
            // Guards a note on stderr and nothing else -- `encoded` is
            // what comes back either way -- so both halves of this
            // condition survive mutation: no test captures stderr.
            // That is the structural gap TODO.md records for the whole
            // command layer, not a hole in this line.
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
        if keywords::JUNK
            .iter()
            .chain(keywords::NOT_JUNK)
            .any(|k| k.eq_ignore_ascii_case(name))
        {
            eprintln!(
                "Note: '{}' is one of five spellings of junk / not junk. Setting it \
                 alone leaves the others as they were, which is how a message ends up \
                 both; 'tag junk' and 'tag notjunk' set what this mailbox keeps and \
                 clear the opposite.",
                name
            );
        }
        // A registered keyword goes out in its registered spelling, the
        // same normalization the system flags get.
        let wire = keywords::canonical(&wire)
            .map(str::to_string)
            .unwrap_or(wire);
        if !out.iter().any(|s| same_keyword(s, &wire)) {
            out.push(wire);
        }
    }
    Ok(out)
}

/// Refuse the command if any group names a UID the folder does not have.
///
/// RFC 3501 §6.4.8: `UID STORE` ignores a UID that does not exist,
/// without an error. Reporting "Added \Deleted on 1 message(s)" for a
/// typo'd UID would be a lie the server never told.
///
/// This runs over EVERY group before the first `store_flags`, not per
/// group inside the mutation loop. Inside it, a bad UID in the second
/// folder aborts with "nothing was changed" after the first folder was
/// already changed -- the one thing the message promises did not happen.
fn check_uids_exist(client: &mut ImapClient, groups: &[Group], wire: &str) -> Result<()> {
    for group in groups {
        let existing = client.folder_uids(&group.folder)?;
        let missing: Vec<String> = group
            .uids
            .iter()
            .filter(|u| !existing.contains(u))
            .map(|u| u.to_string())
            .collect();
        if !missing.is_empty() {
            bail!(
                "no message with UID {} in '{}' ({} would ignore it silently, \
                 so nothing was changed)",
                missing.join(", "),
                group.folder,
                wire
            );
        }
    }
    Ok(())
}

/// Shared implementation of `flag add|remove` and `tag add|remove`.
/// `system` distinguishes flag (true) from tag (false);
/// `add` enables the flags, `remove` disables them.
#[allow(clippy::too_many_arguments)]
pub fn change_flags(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    names: &[String],
    system: bool,
    wire_form: bool,
    add: bool,
    json: bool,
    debug: bool,
) -> Result<()> {
    let flags = parse_flag_names(names, system, wire_form)?;
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections)?;
    let (added, removed): (&[String], &[String]) =
        if add { (&flags, &[]) } else { (&[], &flags) };

    // Everything that can refuse the command is answered here, for every
    // group, before any group is changed.
    for group in &groups {
        // RFC 3501 §7.1: a flag outside PERMANENTFLAGS is either
        // ignored or kept for this session only. Storing one and
        // reporting success would be a lie found out at the next
        // refresh -- which is exactly how a Thunderbird `Junk` vanishes
        // on a server that keeps only `$Junk`.
        let permanent = client.permanent_flags(&group.folder)?;
        if let Some(lost) = added.iter().find(|f| !permanent.keeps(f)) {
            bail!(
                "'{}' is not in the PERMANENTFLAGS of '{}', so the server would keep it \
                 for this session at best. It keeps: {}",
                lost,
                group.folder,
                if permanent.flags.is_empty() {
                    "nothing it named".to_string()
                } else {
                    permanent.flags.join(", ")
                }
            );
        }
    }
    check_uids_exist(&mut client, &groups, "UID STORE")?;

    for group in &groups {
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
            emit_json(out, &FlagChangeOutput {
                folder: &group.folder,
                count: group.uids.len(),
                uids: &group.uids,
                added,
                removed,
            })?;
            continue;
        }
        let verb = if add { "Added" } else { "Removed" };
        writeln!(out, "{} {} on {} message(s) in '{}': UIDs {}",
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
        )?;
    }
    Ok(())
}

/// `tag junk` / `tag notjunk`: say one thing about a message in every
/// spelling the mailbox will keep, and take back every spelling of the
/// opposite.
///
/// Five names carry two meanings (`keywords::JUNK`, `NOT_JUNK`), and
/// no standard says which wins when both are present. Clients create
/// that contradiction by setting their own spelling and leaving the
/// others alone; this does the opposite — every opposite spelling is
/// cleared, unconditionally, because clearing what is not there costs
/// nothing and leaving it there costs correctness. What is *written*
/// is filtered by `PERMANENTFLAGS`: all the spellings on a server that
/// takes new keywords, only the listed ones on a server that does not.
pub fn set_junk(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    junk: bool,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections)?;
    let (want, other) = if junk {
        (keywords::JUNK, keywords::NOT_JUNK)
    } else {
        (keywords::NOT_JUNK, keywords::JUNK)
    };

    // What each group will write, decided before anything is written: a
    // group that cannot take the keyword must refuse the whole command,
    // not just its own turn in the loop.
    let mut writes: Vec<Vec<String>> = Vec::new();
    for group in &groups {
        let permanent = client.permanent_flags(&group.folder)?;
        let add: Vec<String> = permanent
            .keepable(want)
            .iter()
            .map(|s| s.to_string())
            .collect();
        if add.is_empty() {
            bail!(
                "'{}' keeps none of {} and will not take new keywords, so there is no \
                 way to mark a message {}",
                group.folder,
                want.join(", "),
                if junk { "junk" } else { "not junk" }
            );
        }
        writes.push(add);
    }
    // `tag junk` mutates exactly as `tag add` does, so it owes the same
    // check: without it a typo'd UID is reported as a successful
    // marking, because UID STORE ignores it in silence.
    check_uids_exist(&mut client, &groups, "UID STORE")?;

    for (group, add) in groups.iter().zip(&writes) {
        let remove: Vec<String> = other.iter().map(|s| s.to_string()).collect();
        if debug {
            eprintln!(
                "junk: setting {:?}, clearing {:?} in '{}'",
                add, remove, group.folder
            );
        }
        client.store_flags(&group.folder, &group.uids, add, &remove)?;
        if json {
            emit_json(out, &FlagChangeOutput {
                folder: &group.folder,
                count: group.uids.len(),
                uids: &group.uids,
                added: add,
                removed: &remove,
            })?;
            continue;
        }
        writeln!(out, "Marked {} message(s) in '{}' {}: set {}, cleared {}",
            group.uids.len(),
            group.folder,
            if junk { "junk" } else { "not junk" },
            add.join(", "),
            remove.join(", ")
        )?;
    }
    Ok(())
}

// Eight, because the writer makes it eight: the seven this command
// already needed to do its job, plus somewhere to print. Grouping them
// into a struct would move the arguments rather than remove them, and
// this is the signature `main.rs` calls once.
#[allow(clippy::too_many_arguments)]
pub fn flag_list(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    tags_only: bool,
    wire: bool,
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections)?;

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
        let junk = match keywords::junk_state(&flags) {
            keywords::JunkState::Junk => Some("junk"),
            keywords::JunkState::NotJunk => Some("not-junk"),
            keywords::JunkState::Contradictory => Some("contradictory"),
            keywords::JunkState::Unsaid => None,
        };
        if json {
            emit_json(out, &FlagListOutput {
                folder,
                uid,
                count: flags.len(),
                flags: &flags,
                junk,
            })?;
            continue;
        }
        if junk == Some("contradictory") {
            writeln!(out, "{}::{}: both a junk and a not-junk keyword are set; no rule says \
                 which wins. 'tag junk' or 'tag notjunk' settles it",
                folder, uid
            )?;
        }
        if flags.is_empty() {
            writeln!(out, "{}::{}: no {}",
                folder,
                uid,
                if tags_only { "tags" } else { "flags" }
            )?;
            continue;
        }
        let shown = render_names(&flags, wire);
        writeln!(out, "{}::{}: {} {}: {}",
            folder,
            uid,
            flags.len(),
            if tags_only { "tag(s)" } else { "flag(s)" },
            shown.join(", ")
        )?;
    }
    Ok(())
}

pub fn parts_list(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selections: &[Selection],
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, selections)?;

    for (folder, uid) in flatten(&groups) {
        if debug {
            eprintln!("Listing MIME parts of {}::{}", folder, uid);
        }
        let parts = client.list_parts(folder, uid)?;
        if json {
            emit_json(out, &PartsListOutput {
                folder,
                uid,
                count: parts.len(),
                parts: &parts,
            })?;
            continue;
        }
        if parts.is_empty() {
            writeln!(out, "{}::{}: no parts", folder, uid)?;
            continue;
        }
        writeln!(out, "{}::{}: {} part(s):", folder, uid, parts.len())?;
        for a in &parts {
            let name = match &a.filename {
                Some(f) => format!(", filename={}", f),
                None => String::new(),
            };
            writeln!(out, "  [{}] {}{} ({} bytes)", a.part, a.content_type, name, a.size)?;
        }
    }
    Ok(())
}

/// The file to write a part to when `-o` names none.
///
/// The name comes out of the message -- `Content-Disposition: filename=`
/// or the `name=` parameter -- so it is remote input, chosen by whoever
/// sent the mail. Left as it arrived it is a path, and
/// `filename="../../../.ssh/authorized_keys"` writes there.
///
/// A rejected name falls back to the `uid<N>_part<M>` form rather than
/// being trimmed to its last component: trimming would still let the
/// sender pick the name of a file in the working directory, which is
/// the other half of what they should not get to choose. `-o` is
/// unfiltered, because there the caller named the path.
pub(crate) fn safe_part_filename(name: Option<&str>, uid: u32, part: u32) -> PathBuf {
    sanitized_part_name(name, &PathBuf::from(format!("uid{}_part{}", uid, part)))
}

/// The file to write a part to under `--all`, when the caller did not
/// pick a name for it. Same sanitization as `safe_part_filename`, but a
/// fallback that does not need a `uid` -- `--all` already names exactly
/// one message -- and does not guess at an extension from the content
/// type: a wrong one is worse than `.bin`.
fn safe_part_filename_all(name: Option<&str>, part: u32) -> PathBuf {
    sanitized_part_name(name, &PathBuf::from(format!("part-{}.bin", part)))
}

/// Shared by `safe_part_filename` and `safe_part_filename_all`: accept
/// the MIME-declared name only if it is a single bare path component,
/// else fall back to `fallback` and say why.
fn sanitized_part_name(name: Option<&str>, fallback: &Path) -> PathBuf {
    let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
        return fallback.to_path_buf();
    };
    let bare = !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != ".."
        && !Path::new(name).is_absolute()
        && Path::new(name).components().count() == 1;
    if !bare {
        eprintln!(
            "Note: the message calls this part {:?}, which is a path and not a file \
             name; saving as '{}' instead. Use -o to choose the destination.",
            name,
            fallback.display()
        );
        return fallback.to_path_buf();
    }
    PathBuf::from(name)
}

/// One line per stripped part, plus where the message went.
#[derive(Serialize)]
struct StripOutput<'a> {
    folder: &'a str,
    old_uid: u32,
    new_uid: Option<u32>,
    bytes_before: u64,
    bytes_after: u64,
    stripped: &'a [crate::imap::StrippedPart],
}

pub fn parts_strip(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selection: &Selection,
    parts: &[u32],
    json: bool,
    debug: bool,
) -> Result<()> {
    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, std::slice::from_ref(selection))?;
    let messages = flatten(&groups);
    if messages.len() != 1 {
        bail!(
            "'part strip' rewrites one message, but selection '{}' names {} \
             (use 'part list' to see the parts of one, then strip that one)",
            selection.source,
            messages.len()
        );
    }
    let (folder, uid) = messages[0];
    let folder = folder.to_string();
    let outcome = client.strip_part(&folder, uid, parts)?;

    if json {
        emit_json(out, &StripOutput {
            folder: &outcome.folder,
            old_uid: outcome.old_uid,
            new_uid: outcome.new_uid,
            bytes_before: outcome.bytes_before,
            bytes_after: outcome.bytes_after,
            stripped: &outcome.stripped,
        })?;
    } else {
        for p in &outcome.stripped {
            writeln!(out, "Stripped part {} ({}{}), {} bytes",
                p.part,
                p.content_type,
                match &p.filename {
                    Some(f) => format!(", {}", f),
                    None => String::new(),
                },
                p.size
            )?;
            writeln!(out, "  sha256 {}", p.sha256)?;
        }
        match outcome.new_uid {
            Some(n) => writeln!(out, "UID {} became UID {} in '{}' ({} -> {} bytes)",
                outcome.old_uid, n, outcome.folder, outcome.bytes_before, outcome.bytes_after
            )?,
            // Without UIDPLUS there is no APPENDUID, and this command
            // refuses without UIDPLUS -- so this branch is a server
            // that has the capability and did not report the UID.
            None => writeln!(out, "UID {} was rewritten in '{}' ({} -> {} bytes); the server reported no \
                 new UID",
                outcome.old_uid, outcome.folder, outcome.bytes_before, outcome.bytes_after
            )?,
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn parts_save(out: &mut dyn Write,
    config: &Config,
    spec: &FolderSpec,
    selection: &Selection,
    part: Option<u32>,
    all: bool,
    dest: Option<PathBuf>,
    json: bool,
    debug: bool,
) -> Result<()> {
    let to_stdout = dest.as_deref() == Some(Path::new("-"));
    if all && to_stdout {
        bail!(
            "'part save --all' can't write to stdout ('-o -'): several binaries \
             concatenated on one stream is not a thing anyone can use. Give a \
             directory with -o, or drop --all and save one part at a time"
        );
    }
    if to_stdout && json {
        bail!(
            "'part save -o -' can't be combined with -j/--json: the JSON object and \
             the part body would be on the same stdout stream"
        );
    }

    let mut client = ImapClient::connect(config, debug)?;
    let groups = selection_groups(&mut client, spec, config, std::slice::from_ref(selection))?;
    let messages = flatten(&groups);
    if messages.len() != 1 {
        bail!(
            "'part save' writes {} of one message, but selection '{}' names {} \
             (use 'part list' to see them, then save one at a time)",
            if all { "every part" } else { "one part" },
            selection.source,
            messages.len()
        );
    }
    let (folder, uid) = messages[0];
    let folder = folder.to_string();

    if all {
        return parts_save_all(out, &mut client, &folder, uid, dest, json, debug);
    }

    let part = part.expect("clap requires PART unless --all");
    if debug {
        eprintln!("Saving part {} of UID {} to '{}'", part, uid, dest.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "default filename".to_string()));
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

    if to_stdout {
        let size = stream_part_to_stdout(out, &mut client, &folder, uid, part)?;
        if debug {
            eprintln!(
                "Wrote {} bytes of part {} of UID {} to stdout",
                size, part, uid
            );
        }
        return Ok(());
    }

    let dest = dest.unwrap_or_else(|| safe_part_filename(info.filename.as_deref(), uid, part));

    let size = client.save_part(&folder, uid, part, &dest)?;
    if json {
        emit_json(out, &PartsSaveOutput {
            folder: &folder,
            uid,
            part,
            file: dest.to_str().unwrap_or_default(),
            size,
        })?;
    } else {
        writeln!(out, "Saved part {} of UID {} to '{}' ({} bytes)",
            part,
            uid,
            dest.display(),
            size
        )?;
    }
    Ok(())
}

/// `part save SELECTION --all`: every leaf part of the one selected
/// message, into `dir` (or the current directory). Refuses to
/// overwrite a file already there -- with attachment names coming from
/// the message rather than the caller, that is the one mistake this
/// command could make that a "no" can't undo.
fn parts_save_all(out: &mut dyn Write,
    client: &mut ImapClient,
    folder: &str,
    uid: u32,
    dest: Option<PathBuf>,
    json: bool,
    debug: bool,
) -> Result<()> {
    let dir = dest.unwrap_or_else(|| PathBuf::from("."));
    if !dir.is_dir() {
        bail!(
            "'part save --all -o {}' needs a directory that already exists; it will \
             not create one",
            dir.display()
        );
    }
    if debug {
        eprintln!("Saving all parts of UID {} to '{}'", uid, dir.display());
    }
    let parts = client.list_parts(folder, uid)?;
    for (saved, p) in parts.iter().enumerate() {
        let name = safe_part_filename_all(p.filename.as_deref(), p.part);
        let dest = dir.join(&name);
        if dest.exists() {
            bail!(
                "'{}' already exists; refusing to overwrite it ({} part(s) already \
                 saved before this one)",
                dest.display(),
                saved
            );
        }
        let size = client.save_part(folder, uid, p.part, &dest)?;
        if json {
            emit_json(out, &PartsSaveOutput {
                folder,
                uid,
                part: p.part,
                file: dest.to_str().unwrap_or_default(),
                size,
            })?;
        } else {
            writeln!(out, "Saved part {} of UID {} to '{}' ({} bytes)",
                p.part,
                uid,
                dest.display(),
                size
            )?;
        }
    }
    Ok(())
}

/// Write one part's decoded bytes to stdout, raw and undecorated, for
/// piping. `ImapClient::save_part` only knows how to write to a path,
/// so this goes through a private temp file rather than the caller's
/// own stdout descriptor, and always cleans it up.
fn stream_part_to_stdout(
    out: &mut dyn Write,
    client: &mut ImapClient,
    folder: &str,
    uid: u32,
    part: u32,
) -> Result<u64> {

    // Straight from the server to stdout. The earlier route through a
    // temporary file was not merely a detour: `std::env::temp_dir()` is
    // world-readable and the name was built from the pid, the UID and
    // the part number, so the attachment was briefly readable by every
    // local user at a path any of them could predict -- and could be
    // redirected by a symlink planted there first.
    let data = client.fetch_part(folder, uid, part)?;
    // Through the writer like everything else, rather than reaching for
    // `std::io::stdout()` again: it was the one path that named stdout
    // for itself, which left `-o -` the one path a test could not read
    // back. The bytes are the same bytes.
    out.write_all(&data)?;
    out.flush()?;
    Ok(data.len() as u64)
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
    fn a_part_filename_from_the_message_cannot_name_a_path() {
        // Every one of these arrives from a `Content-Disposition` the
        // sender wrote, and must not become a destination path.
        for hostile in [
            "../../../tmp/escape",
            "/etc/passwd",
            "..",
            ".",
            "sub/dir.pdf",
            "..\\..\\win.ini",
            "",
            "   ",
        ] {
            assert_eq!(
                safe_part_filename(Some(hostile), 7, 2),
                PathBuf::from("uid7_part2"),
                "{:?} must fall back, not be used",
                hostile
            );
        }
        assert_eq!(safe_part_filename(Some("q3.xlsx"), 7, 2), PathBuf::from("q3.xlsx"));
        assert_eq!(
            safe_part_filename(Some("  invoice 42.pdf  "), 7, 2),
            PathBuf::from("invoice 42.pdf")
        );
        assert_eq!(safe_part_filename(None, 7, 2), PathBuf::from("uid7_part2"));
    }

    #[test]
    fn every_group_is_checked_before_any_group_is_changed() {
        // The check that used to sit inside the mutation loop: a bad
        // UID in the second folder aborted with "nothing was changed"
        // after the first folder had already been changed.
        let mut client = mock_client();
        let groups = vec![
            Group { folder: "INBOX".to_string(), uids: vec![1] },
            Group { folder: "Trash".to_string(), uids: vec![99999] },
        ];
        let err = check_uids_exist(&mut client, &groups, "UID STORE")
            .expect_err("a UID that is not there must stop the command");
        assert!(err.to_string().contains("99999"), "{}", err);
        assert!(err.to_string().contains("Trash"), "{}", err);
        let fine = vec![Group { folder: "INBOX".to_string(), uids: vec![1, 2] }];
        assert!(check_uids_exist(&mut client, &fine, "UID STORE").is_ok());
    }

    #[test]
    fn the_keyword_error_does_not_call_a_comma_illegal() {
        // A comma IS legal in an atom -- modified UTF-7 writes one
        // inside base64 -- and the message used to list it as excluded
        // in the same breath as saying it was fine.
        assert_eq!(
            parse_flag_names(&["a,b".into()], false, false).unwrap(),
            vec!["a,b".to_string()]
        );
        let err = parse_flag_names(&["a(b".into()], false, false)
            .expect_err("'(' is an atom-special");
        let text = err.to_string();
        let excluded = text.split("spaces").next().unwrap_or("");
        assert!(!excluded.contains(','), "the list still excludes a comma: {}", text);
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
            selection_groups(&mut client, &spec, &mock_config(), &sels(&["Trash::2"])).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].folder, "Trash");
    }

    #[test]
    fn selections_group_by_folder_in_first_named_order() {
        let mut client = mock_client();
        let spec = FolderSpec::default();
        let groups = selection_groups(&mut client, &spec, &mock_config(), &sels(&["Archive::2", "5", "Archive::3,2", "1"]))
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
            selection_groups(&mut client, &spec, &mock_config(), &sels(&["2-4"])).unwrap();
        assert_eq!(groups[0].uids, vec![2, 3, 4]);
        let groups = selection_groups(&mut client, &spec, &mock_config(), &sels(&["*"])).unwrap();
        assert_eq!(groups[0].uids, vec![1, 2, 3, 4, 5]);
        let groups =
            selection_groups(&mut client, &spec, &mock_config(), &sels(&["4-"])).unwrap();
        assert_eq!(groups[0].uids, vec![4, 5]);
    }

    #[test]
    fn an_unqualified_count_applies_to_every_selected_folder() {
        let mut client = mock_client();
        let spec = FolderSpec::new(vec!["INBOX".to_string(), "Trash".to_string()]);
        let groups =
            selection_groups(&mut client, &spec, &mock_config(), &sels(&["last:2"])).unwrap();
        // Two folders, the two newest of each — and no ambiguity error,
        // because a count names no UID.
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].folder, "INBOX");
        assert_eq!(groups[0].uids, vec![4, 5]);
        assert_eq!(groups[1].folder, "Trash");
        assert_eq!(groups[1].uids, vec![4, 5]);
    }

    #[test]
    fn a_bare_uid_beside_a_count_is_still_ambiguous() {
        // The count is fine across folders; the UID is not, and the
        // selection as a whole is judged on what it names.
        let mut client = mock_client();
        let spec = FolderSpec::new(vec!["INBOX".to_string(), "Trash".to_string()]);
        assert!(
            selection_groups(&mut client, &spec, &mock_config(), &sels(&["last:2"])).is_ok()
        );
        let err = selection_groups(&mut client, &spec, &mock_config(), &sels(&["1,last:2"]))
            .expect_err("a UID with two folders selected is ambiguous");
        assert!(err.to_string().contains("ambiguous"), "{}", err);
    }

    #[test]
    fn a_qualified_count_names_its_own_folder() {
        let mut client = mock_client();
        let spec = FolderSpec::new(vec!["INBOX".to_string(), "Trash".to_string()]);
        let groups = selection_groups(
            &mut client,
            &spec,
            &mock_config(),
            &sels(&["Drafts::last:1", "Trash::first:2"]),
        )
        .unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].folder, "Drafts");
        assert_eq!(groups[0].uids, vec![5]);
        assert_eq!(groups[1].folder, "Trash");
        assert_eq!(groups[1].uids, vec![1, 2]);
    }

    #[test]
    fn a_range_past_the_end_of_the_mailbox_matches_nothing() {
        let mut client = mock_client();
        let spec = FolderSpec::default();
        let err = selection_groups(&mut client, &spec, &mock_config(), &sels(&["99-"]))
            .expect_err("must not fall back to the last message");
        assert!(err.to_string().contains("matched no message"), "{}", err);
    }

    /// Run a handler and give back what it printed.
    ///
    /// The point of threading a writer through every handler: until
    /// now a test could call one and see only its `Result`, so the
    /// whole body of `read_emails` could be replaced with `Ok(())`
    /// and nothing failed. 152 of `cli/mod.rs`'s 368 mutants survived
    /// for that reason.
    fn printed(run: impl FnOnce(&mut dyn Write) -> Result<()>) -> String {
        let mut buf: Vec<u8> = Vec::new();
        run(&mut buf).expect("the handler should succeed against the mock");
        String::from_utf8(buf).expect("output is UTF-8")
    }

    #[test]
    fn search_prints_a_header_and_one_line_a_message() {
        let text = printed(|out| {
            search_emails(out, &mock_config(), "ALL", &FolderSpec::default(), false, false)
        });
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("Found 5 email(s) in 'INBOX'"), "{}", lines[0]);
        assert_eq!(lines.len(), 6, "a header and one line per message: {:?}", lines);
        // The fields the README says a result line carries.
        let third = lines.iter().find(|l| l.contains("UID 3")).expect("UID 3");
        assert!(third.contains("Quarterly report"), "the subject: {}", third);
        assert!(third.contains("carol@example.com"), "the from: {}", third);
        assert!(third.contains("arrived 2026-09-20"), "the labelled date: {}", third);
        assert!(third.contains("[120 bytes]"), "the size: {}", third);
        assert!(third.contains("[2 part(s)]"), "the part count: {}", third);
    }

    #[test]
    fn the_sort_asked_for_decides_both_the_order_and_the_date_column() {
        let by_sent = printed(|out| {
            let config = Config { sort: Some("date".into()), ..mock_config() };
            search_emails(out, &config, "ALL", &FolderSpec::default(), false, false)
        });
        let uids: Vec<&str> = by_sent
            .lines()
            .skip(1)
            .map(|l| l.split(" | ").next().unwrap_or("").trim())
            .collect();
        assert_eq!(uids, ["UID 3", "UID 1", "UID 5", "UID 2", "UID 4"], "sent order");
        assert!(by_sent.contains("sent 2026-09-14"), "and the column follows: {}", by_sent);
        assert!(!by_sent.contains("arrived"), "not both at once: {}", by_sent);
    }

    #[test]
    fn json_output_is_one_object_on_one_line_and_nothing_else() {
        let text = printed(|out| {
            search_emails(out, &mock_config(), "ALL", &FolderSpec::default(), true, false)
        });
        assert_eq!(text.lines().count(), 1, "one object, one line: {}", text);
        let value: serde_json::Value = serde_json::from_str(text.trim()).expect("valid JSON");
        assert_eq!(value["count"], 5);
        assert_eq!(value["folder"], "INBOX");
        // Both dates travel in JSON whichever one the text shows.
        assert!(value["results"][0]["date"].is_string());
        assert!(value["results"][0]["sent"].is_string());
    }

    #[test]
    fn reading_a_message_prints_the_message_and_not_its_mime() {
        let text = printed(|out| {
            read_emails(
                out,
                &mock_config(),
                &FolderSpec::default(),
                &[select::parse_selection("1").unwrap()],
                false,
                false,
                false,
            )
        });
        assert!(text.contains("Subject: Welcome aboard"), "{}", text);
        assert!(text.contains("Hello, this is message 1."), "the body: {}", text);
        assert!(!text.contains("Content-Type:"), "the MIME is not the message: {}", text);
    }

    #[test]
    fn listing_folders_names_every_mailbox_and_long_adds_the_rest() {
        let text = printed(|out| list_folders(out, &mock_config(), false, false, false, false));
        assert!(text.starts_with("Folders (5):"), "{}", text);
        for name in ["INBOX", "Sent Items", "Drafts", "Trash", "Spam"] {
            assert!(text.contains(&format!("- {}", name)), "{} missing from {}", name, text);
        }
        let long = printed(|out| list_folders(out, &mock_config(), false, false, true, false));
        assert!(long.contains("Spam"), "{}", long);
        assert!(long.contains("Junk"), "--long carries the special use: {}", long);
    }

    #[test]
    fn reading_several_messages_banners_each_and_separates_them() {
        // One message prints bare; several get a `--- folder::uid ---`
        // banner and a blank line between them, and the guards that
        // decide this survived every mutation until a test read more
        // than one message.
        let one = printed(|out| {
            read_emails(
                out,
                &mock_config(),
                &FolderSpec::default(),
                &[select::parse_selection("1").unwrap()],
                false,
                false,
                false,
            )
        });
        assert!(!one.contains("---"), "a single message needs no banner: {}", one);

        let two = printed(|out| {
            read_emails(
                out,
                &mock_config(),
                &FolderSpec::default(),
                &[select::parse_selection("1,2").unwrap()],
                false,
                false,
                false,
            )
        });
        assert!(two.contains("--- INBOX::1 ---"), "{}", two);
        assert!(two.contains("--- INBOX::2 ---"), "{}", two);
        assert!(two.contains("Welcome aboard") && two.contains("Meeting notes"), "{}", two);
        // A blank line before the second banner, and not before the
        // first: the separator goes between, not in front.
        //
        // The exact run of newlines matters, and asserting less would
        // prove nothing: the rendered message already ends `\r\n`, and
        // `writeln!` adds one more, so `\n\n---` is there whether or
        // not the separator fired. Three is the count that only the
        // separator can produce -- checked against `od -c`, after a
        // mutant that removed the blank line sailed past the looser
        // assertion this line replaces.
        assert!(two.contains("\n\n\n--- INBOX::2 ---"), "separated: {:?}", two);
        assert!(!two.starts_with('\n'), "nothing before the first: {:?}", two);
    }

    #[test]
    fn searching_several_folders_groups_the_hits_under_each() {
        let many = printed(|out| {
            search_emails(
                out,
                &mock_config(),
                "ALL",
                &FolderSpec::new(vec!["*".to_string()]),
                false,
                false,
            )
        });
        assert!(many.starts_with("Found 25 email(s) in 5 folder(s)"), "{}", many);
        for folder in ["INBOX", "Sent Items", "Drafts", "Trash", "Spam"] {
            assert!(many.contains(&format!("{} (5):", folder)), "{} missing: {}", folder, many);
        }
        // One folder says so instead, and names it rather than counting.
        let one = printed(|out| {
            search_emails(out, &mock_config(), "ALL", &FolderSpec::default(), false, false)
        });
        assert!(one.starts_with("Found 5 email(s) in 'INBOX'"), "{}", one);
        assert!(!one.contains("folder(s)"), "not the plural form: {}", one);
    }

    #[test]
    fn a_result_line_mentions_parts_only_when_there_are_some() {
        let mut r = SearchResult {
            uid: 1,
            folder: "INBOX".into(),
            subject: "Hi".into(),
            from: "a@example.com".into(),
            date: Some("2026-09-20 12:00:00 +0000".into()),
            sent: None,
            size: None,
            flags: Vec::new(),
            parts: 0,
        };
        let line = printed(|out| print_search_result(out, "  ", &r, false));
        assert!(!line.contains("part(s)"), "nothing to say about no parts: {}", line);
        r.parts = 2;
        let line = printed(|out| print_search_result(out, "  ", &r, false));
        assert!(line.contains("[2 part(s)]"), "{}", line);
    }

    /// `part save -o -` puts the part's bytes on the stream and says
    /// nothing else on it.
    ///
    /// This path named `std::io::stdout()` for itself until now, which
    /// left it the one output a test could not read back -- and it is
    /// the path that must not print anything alongside the bytes, or
    /// whatever the caller redirects into is corrupt.
    #[test]
    fn saving_a_part_to_stdout_writes_the_bytes_and_nothing_else() {
        let mut buf: Vec<u8> = Vec::new();
        parts_save(
            &mut buf,
            &mock_config(),
            &FolderSpec::default(),
            &select::parse_selection("3").unwrap(),
            Some(2),
            false,
            Some(PathBuf::from("-")),
            false,
            false,
        )
        .expect("the mock has part 2 of UID 3");
        // The mock builds that part as filler sized to what `part list`
        // declares, so the length is the assertion that matters.
        assert_eq!(buf.len(), 20480, "the part's bytes, all of them");
        assert!(
            !buf.starts_with(b"Saved"),
            "no commentary on the stream that carries the file"
        );
    }

    #[test]
    fn part_save_refuses_a_selection_naming_several_messages() {
        let config = mock_config();
        let err = parts_save(
            &mut Vec::new(),
            &config,
            &FolderSpec::default(),
            &select::parse_selection("1,2").unwrap(),
            Some(1),
            false,
            None,
            false,
            false,
        )
        .expect_err("part save takes exactly one message");
        assert!(err.to_string().contains("one part of one message"), "{}", err);
    }

    #[test]
    fn the_date_column_says_which_date_it_is() {
        let r = SearchResult {
            uid: 1,
            folder: "INBOX".into(),
            subject: "Hi".into(),
            from: "a@example.com".into(),
            date: Some("2026-09-20 12:00:00 +0000".into()),
            sent: Some("Fri, 18 Sep 2026 17:30:00 +0200".into()),
            size: None,
            flags: Vec::new(),
            parts: 0,
        };
        assert_eq!(date_cell(&r, false), "arrived 2026-09-20 12:00:00 +0000");
        // Normalised to the arrival date's format so the column lines
        // up -- but keeping the message's own offset, which is what it
        // says about itself.
        assert_eq!(date_cell(&r, true), "sent 2026-09-18 17:30:00 +0200");
    }

    #[test]
    fn a_date_column_with_nothing_to_show_says_so_rather_than_borrowing() {
        // The arrival date is right there in both of these, and using
        // it would be the same conflation the sort keys exist to end.
        let mut r = SearchResult {
            uid: 1,
            folder: "INBOX".into(),
            subject: "Hi".into(),
            from: "a@example.com".into(),
            date: Some("2026-09-20 12:00:00 +0000".into()),
            sent: None,
            size: None,
            flags: Vec::new(),
            parts: 0,
        };
        assert_eq!(date_cell(&r, true), "sent unknown");
        // A header that will not parse is shown as it came: it is what
        // the message says, and it explains why such a message sorted
        // to the front of a date sort.
        r.sent = Some("whenever I got round to it".into());
        assert_eq!(date_cell(&r, true), "sent whenever I got round to it");
        // And the same rule on the other side.
        r.date = None;
        assert_eq!(date_cell(&r, false), "arrived unknown");
    }

    #[test]
    fn the_column_follows_the_primary_sort_key_only() {
        use crate::imap::parse_sort;
        assert!(parse_sort("date").unwrap().leads_with_sent_date());
        assert!(parse_sort("-date").unwrap().leads_with_sent_date());
        assert!(parse_sort("date,uid").unwrap().leads_with_sent_date());
        // An arrival list with ties broken by the sent date is still an
        // arrival list, and showing sent dates would explain its order
        // less well rather than better.
        assert!(!parse_sort("arrival,date").unwrap().leads_with_sent_date());
        assert!(!parse_sort("arrival").unwrap().leads_with_sent_date());
        assert!(!parse_sort("subject").unwrap().leads_with_sent_date());
    }

    /// The small helpers the command layer leans on, none of which
    /// had a test of its own.
    ///
    /// Each is one line and each decides something a user sees or a
    /// file gets called, which is exactly the shape that goes
    /// unasserted: a mutant replacing `yes_no`'s whole body with
    /// `"xyzzy"` -- so `info` reports `xyzzy` for every capability --
    /// survived the suite.
    #[test]
    fn the_one_line_helpers_answer_what_they_say_they_answer() {
        // INBOX is the one name IMAP defines as case-insensitive, and
        // `move`/`copy` use this to refuse filing a message into the
        // folder it is already in. All four of its mutants lived.
        assert!(same_folder("Archive", "Archive"));
        assert!(same_folder("INBOX", "inbox"), "INBOX folds case");
        assert!(!same_folder("Archive", "archive"), "and nothing else does");
        assert!(!same_folder("INBOX", "Archive"));

        assert_eq!(yes_no(true), "yes");
        assert_eq!(yes_no(false), "no");

        // RFC 6154, with or without the leading backslash the wire uses.
        assert_eq!(special_use_name("\\Sent"), Some("\\Sent"));
        assert_eq!(special_use_name("sent"), Some("\\Sent"));
        assert_eq!(special_use_name("\\Junk"), Some("\\Junk"));
        assert_eq!(special_use_name("\\Marked"), None, "not a special use");
        assert_eq!(special_use_name("nonsense"), None);

        // `--all` names the file when the part does not. The wrapper
        // was untested: replacing it with `PathBuf::default()` -- an
        // empty path -- survived, and every saved part would land on
        // the same unwritable name.
        assert_eq!(safe_part_filename_all(Some("report.pdf"), 2), PathBuf::from("report.pdf"));
        assert_eq!(safe_part_filename_all(None, 2), PathBuf::from("part-2.bin"));
        assert_eq!(safe_part_filename_all(Some("   "), 7), PathBuf::from("part-7.bin"));
        // And the sanitisation still applies: a name that is a path is
        // not a name.
        assert_eq!(safe_part_filename_all(Some("../etc/passwd"), 3), PathBuf::from("part-3.bin"));
        assert_eq!(safe_part_filename_all(Some("/etc/passwd"), 3), PathBuf::from("part-3.bin"));
    }

    /// Keywords that will not decode are compared as they stand.
    ///
    /// The same arm as `imap::same_keyword`'s, and the same gap: two
    /// undecodable keywords that are the same string are still the same
    /// keyword, and inverting the fallback would make `tag remove` miss
    /// exactly the malformed keywords it exists to handle.
    #[test]
    fn keyword_comparison_falls_back_to_the_text_when_decoding_fails() {
        assert!(same_keyword("&bogus", "&bogus"));
        assert!(!same_keyword("&bogus", "&other"));
        assert!(same_keyword("invoice", "INVOICE"), "ASCII case folds");
        assert!(!same_keyword("invoice", "receipt"));
    }

    /// `\Recent` is refused by name, and says why.
    ///
    /// `parse_flag_names` refuses it because the server manages it --
    /// a different refusal from "that is not an IMAP flag", and the
    /// difference is the whole message. Forcing the guard either way
    /// survived: with `false` the user is told `recent` is not a flag,
    /// which is wrong and sends them looking for a spelling.
    #[test]
    fn recent_is_refused_as_the_servers_own_not_as_a_typo() {
        let refuse = |name: &str| {
            parse_flag_names(&[name.to_string()], true, false)
                .expect_err(name)
                .to_string()
        };
        // Spelled any way, since flag names fold case.
        for spelling in ["recent", "Recent", "RECENT"] {
            let text = refuse(spelling);
            assert!(text.contains("managed by the server"), "{}: {}", spelling, text);
            assert!(
                !text.contains("is not an IMAP-defined flag"),
                "{} got the wrong refusal, which sends the reader hunting for a \
                 spelling that does not exist: {}",
                spelling,
                text
            );
        }
        // An actual typo gets the other message, the one that lists
        // what is available.
        assert!(refuse("seeen").contains("is not an IMAP-defined flag"));
        // And the five real ones still parse.
        let ok = parse_flag_names(
            &["seen", "answered", "flagged", "deleted", "draft"].map(String::from),
            true,
            false,
        )
        .expect("the five IMAP flags");
        assert_eq!(ok.len(), 5);
    }

    #[test]
    fn search_output_json_shape_single_folder() {
        let results = vec![SearchResult {
            uid: 7,
            folder: "INBOX".into(),
            subject: "Hi".into(),
            from: "a@example.com".into(),
            date: Some("2026-09-20 12:00:00 +0000".into()),
            sent: Some("Sun, 20 Sep 2026 11:00:00 +0000".into()),
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
        // Both dates, because they are two quantities and the text
        // output prints only the arrival one: `-j` is where a caller
        // that sorted by `date` can see what it sorted by.
        assert_eq!(value["results"][0]["date"], "2026-09-20 12:00:00 +0000");
        assert_eq!(value["results"][0]["sent"], "Sun, 20 Sep 2026 11:00:00 +0000");
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
                sent: None,
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
                sent: None,
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
    fn flag_names_are_bare_words_and_normalize() {
        let v = parse_flag_names(
            &["seen".into(), "FLAGGED".into(), "Seen".into()],
            true,
            false,
        )
        .unwrap();
        assert_eq!(v, vec!["\\Seen", "\\Flagged"], "case-free, deduplicated");
    }

    #[test]
    fn the_wire_form_of_a_flag_needs_wire() {
        // A bare word needs no shell quoting, which is the point; the
        // backslash form is what a listing prints, so --wire takes it.
        let err = parse_flag_names(&["\\Seen".into()], true, false)
            .expect_err("the wire form is --wire's");
        assert!(err.to_string().contains("write it as seen"), "{}", err);
        assert_eq!(
            parse_flag_names(&["\\Seen".into()], true, true).unwrap(),
            vec!["\\Seen"]
        );
        let err = parse_flag_names(&["seen".into()], true, true)
            .expect_err("--wire means the wire form");
        assert!(err.to_string().contains("\\Seen"), "{}", err);
    }

    #[test]
    fn info_reports_the_account_as_the_caller_would_have_to_guess_it() {
        let mut client = mock_client();
        let config = mock_config();
        let out = build_info(&mut client, &config, Some("/tmp/x.conf"), None, config.access)
            .expect("info");
        // The delimiter comes off the mailbox list, and the special
        // uses say which mailbox is the Trash on an account that does
        // not call it "Trash".
        assert_eq!(out.folders.delimiter.as_deref(), Some("/"));
        assert_eq!(out.folders.delimiter_source, "server");
        assert_eq!(
            out.folders.special_use.get("\\Trash").map(String::as_str),
            Some("Trash")
        );
        assert_eq!(
            out.folders.special_use.get("\\Junk").map(String::as_str),
            Some("Spam")
        );
        assert!(out.folders.default_exists, "INBOX is in the list");
        // The mock advertises nothing, so every degraded path is the
        // one reported -- and filing, which needs MOVE or UIDPLUS, is
        // reported as refused rather than as a fallback.
        assert_eq!(out.server.filing, "refused");
        assert_eq!(out.server.sorting, "client");
        assert_eq!(out.server.threading, "client");
        assert!(!out.server.create_special_use);
        // organize: flags and filing (move, copy) yes, the tree,
        // \Deleted, expunge and append no -- append is held to the same
        // rung as setting \Deleted itself.
        assert_eq!(out.access.effective, "organize");
        assert!(out.access.may.store_flags && out.access.may.move_messages);
        assert!(out.access.may.copy_messages);
        assert!(!out.access.may.change_folders && !out.access.may.set_deleted);
        assert!(!out.access.may.expunge);
        assert!(!out.access.may.append);
    }

    #[test]
    fn a_config_delimiter_overrides_the_server_and_says_so() {
        let mut client = mock_client();
        let config = Config {
            delimiter: Some(".".to_string()),
            ..mock_config()
        };
        let out = build_info(&mut client, &config, None, None, config.access).expect("info");
        assert_eq!(out.folders.delimiter.as_deref(), Some("."));
        assert_eq!(out.folders.delimiter_source, "config");
        // What the server said is kept: the two disagreeing is the
        // thing worth seeing.
        assert_eq!(out.folders.server_delimiter.as_deref(), Some("/"));
    }

    #[test]
    fn info_narrowed_for_a_run_reports_both_levels() {
        let mut client = mock_client();
        let config = Config {
            access: crate::config::AccessLevel::Survey,
            ..mock_config()
        };
        let out = build_info(&mut client, &config, None, None, crate::config::AccessLevel::Full)
            .expect("info");
        assert_eq!(out.access.effective, "survey");
        assert_eq!(out.access.configured, "full");
    }

    #[test]
    fn a_special_use_is_a_bare_word_and_normalizes() {
        // The same two-way rule as `flag`: a bare word without --wire,
        // the atom a listing prints with it, and neither the other way
        // round.
        for (given, want) in [("archive", "\\Archive"), ("TRASH", "\\Trash"), ("junk", "\\Junk")] {
            assert_eq!(
                parse_use_attr(Some(given), false).unwrap(),
                Some(want.to_string()),
                "{}",
                given
            );
        }
        assert_eq!(
            parse_use_attr(Some("\\Archive"), true).unwrap(),
            Some("\\Archive".to_string())
        );
        let err = parse_use_attr(Some("\\Archive"), false)
            .expect_err("the wire form is --wire's");
        assert!(err.to_string().contains("write it as archive"), "{}", err);
        let err =
            parse_use_attr(Some("archive"), true).expect_err("--wire means the wire form");
        assert!(err.to_string().contains("\\Archive"), "{}", err);
    }

    #[test]
    fn a_use_attribute_outside_rfc_6154_is_refused() {
        // A closed set is also what keeps an arbitrary string out of
        // the CREATE ... (USE (...)) command line.
        for bad in ["bogus", "seen", "x) (y", "\\All ("] {
            let err = parse_use_attr(Some(bad), false).expect_err(bad);
            assert!(
                err.to_string().contains("RFC 6154") || err.to_string().contains("wire form"),
                "{}: {}",
                bad,
                err
            );
        }
        // No --use at all is the ordinary case; --wire alone says
        // nothing about anything.
        assert_eq!(parse_use_attr(None, false).unwrap(), None);
        let err = parse_use_attr(None, true).expect_err("--wire without --use");
        assert!(err.to_string().contains("no --use was given"), "{}", err);
    }

    #[test]
    fn a_keyword_spelled_like_a_system_flag_is_refused() {
        // Stored as a keyword it is inert, reads like the flag in a
        // listing, and slips past the access level governing the real
        // one -- `flag add 5 deleted` is gated, `tag add 5 Deleted` was
        // not.
        for bad in ["Deleted", "seen", "FLAGGED", "draft", "answered", "Recent"] {
            let err = parse_flag_names(&[bad.to_string()], false, false)
                .expect_err(bad);
            assert!(err.to_string().contains("backslash"), "{}: {}", bad, err);
        }
        // An ordinary keyword is untouched.
        assert_eq!(
            parse_flag_names(&["invoice".into()], false, false).unwrap(),
            vec!["invoice"]
        );
    }

    #[test]
    fn a_listing_prints_what_can_be_typed_back() {
        let flags = vec![
            "\\Seen".to_string(),
            "invoice".to_string(),
            "r&AOk-gie".to_string(),
        ];
        let shown = render_names(&flags, false);
        assert_eq!(shown[0], "seen", "the bare word `flag add` takes");
        assert_eq!(shown[1], "invoice");
        assert_eq!(shown[2], "régie", "and `tag add régie` encodes back to it");
        // --wire prints the atoms, which `add --wire` takes back.
        assert_eq!(render_names(&flags, true), flags);
    }

    #[test]
    fn what_cannot_round_trip_is_glossed_rather_than_rewritten() {
        // Typing either of these back would produce a different atom,
        // so the listing shows the atom and explains it.
        let flags = vec!["$label1".to_string(), "r=c3=a9gie".to_string()];
        let shown = render_names(&flags, false);
        assert!(shown[0].starts_with("$label1 (Thunderbird tag 1"), "{}", shown[0]);
        assert_eq!(shown[1], "r=c3=a9gie (\"régie\", Thunderbird tag key)");
        // ... while a name that says what it is gets no paraphrase.
        assert_eq!(render_names(&["NonJunk".to_string()], false), vec!["NonJunk"]);
        assert_eq!(
            render_names(&["$hasattachment".to_string()], false),
            vec!["$hasattachment"]
        );
        assert_eq!(render_names(&flags, true), flags);
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
            parse_flag_names(&["seen".into(), "SEEN".into()], true, false).unwrap(),
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
            parse_flag_names(&["deleted".into()], true, false).unwrap(),
            vec!["\\Deleted"]
        );
        assert_eq!(
            parse_flag_names(&["invoice".into()], false, false).unwrap(),
            vec!["invoice"]
        );
    }

    #[test]
    fn parse_flag_names_rejects_recent_and_unknown_system_flags() {
        assert!(parse_flag_names(&["recent".into()], true, false).is_err());
        assert!(parse_flag_names(&["bogus".into()], true, false).is_err());
        assert!(parse_flag_names(&["".into()], true, false).is_err());
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
    }

    // An empty list is not this function's call to refuse: it is a
    // precondition of the commands that want one (`flag add`/`tag
    // add`, enforced earlier by `split_args` in main.rs), not of
    // `append`'s optional `--flag`. See `parse_flag_names`'s doc
    // comment.
    #[test]
    fn parse_flag_names_accepts_empty_list() {
        assert_eq!(
            parse_flag_names(&[], true, false).unwrap(),
            Vec::<String>::new()
        );
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
            junk: None,
        };
        let value: serde_json::Value = serde_json::from_str(&serde_json::to_string(&out).unwrap())
            .expect("parse");
        assert_eq!(value["folder"], "INBOX");
        assert_eq!(value["uid"], 5);
        assert_eq!(value["count"], 2);
        assert_eq!(value["flags"][0], "\\Seen");
        assert_eq!(value["flags"][1], "invoice");
        assert!(value.get("junk").is_none(), "silent unless the family says something");
    }

    #[test]
    fn the_junk_family_reaches_the_json() {
        let flags = vec!["Junk".to_string(), "$NotJunk".to_string()];
        let out = FlagListOutput {
            folder: "INBOX",
            uid: 5,
            count: 2,
            flags: &flags,
            junk: Some("contradictory"),
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&out).unwrap()).expect("parse");
        assert_eq!(value["junk"], "contradictory");
    }

    // ------------------------------------------- the folder listing

    fn folder(name: &str, delim: Option<&str>, no_inf: bool, attrs: &[&str]) -> FolderInfo {
        FolderInfo {
            name: name.to_string(),
            delimiter: delim.map(|d| d.to_string()),
            no_inferiors: no_inf,
            attrs: attrs.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[test]
    fn a_plain_listing_prints_names_that_can_be_typed_back() {
        // The name is what goes into -f, so nothing may sit beside it.
        let f = folder("Sent Items", Some("/"), false, &["\\Sent"]);
        assert_eq!(folder_line(&f, false), "  - Sent Items");
    }

    #[test]
    fn long_adds_the_delimiter_and_the_list_attributes() {
        let f = folder("Sent Items", Some("/"), false, &["\\Sent"]);
        assert_eq!(folder_line(&f, true), "  - Sent Items (delim='/' \\Sent)");
        let inbox = folder("INBOX", Some("/"), true, &[]);
        assert_eq!(folder_line(&inbox, true), "  - INBOX (delim='/' \\Noinferiors)");
    }

    #[test]
    fn a_mailbox_the_server_described_with_nothing_gets_no_empty_parentheses() {
        // An empty "()" would read as something withheld.
        let bare = folder("Odd", None, false, &[]);
        assert_eq!(folder_line(&bare, true), "  - Odd");
        let empty_delim = folder("Odd", Some(""), false, &[]);
        assert_eq!(folder_line(&empty_delim, true), "  - Odd");
    }
}
