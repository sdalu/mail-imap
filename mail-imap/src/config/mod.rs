use anyhow::{bail, Context, Result};
use libucl::parser::Flags as ParserFlags;
use libucl::{Emitter, Parser};
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// How much this tool may change on the server.
///
/// The ladder is ordered: each level permits everything the one before
/// it does. Nothing here is a security boundary — an IMAP account can
/// be reached by any other client — it is a guard against *this* tool
/// doing more than the account holder meant it to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Default)]
pub enum AccessLevel {
    /// Change nothing at all. Reads use `BODY.PEEK[]`, so even `\Seen`
    /// stays as it was.
    #[serde(rename = "readonly", alias = "read-only")]
    ReadOnly,
    /// Read, plus the changes that keep every message: set and clear
    /// flags and keywords, and file mail into another folder. `\Deleted`
    /// cannot be *set* here — it is the one flag whose point is removal
    /// — though it can be cleared, which rescues a message rather than
    /// losing one.
    #[default]
    #[serde(rename = "organize", alias = "non-destructive")]
    Organize,
    /// Everything `organize` does, plus the folder tree: create a
    /// mailbox, rename one (never INBOX), subscribe and unsubscribe.
    /// Still keeps every message — deleting a mailbox does not.
    #[serde(rename = "restructure")]
    Restructure,
    /// Everything the tool can do.
    #[serde(rename = "full")]
    Full,
}

impl AccessLevel {
    /// May flags or keywords be changed at all?
    pub fn may_store_flags(self) -> bool {
        self >= AccessLevel::Organize
    }

    /// May a message be filed into another folder? This is `organize`'s
    /// own operation: the message keeps existing, in a different place.
    pub fn may_move(self) -> bool {
        self >= AccessLevel::Organize
    }

    /// May the folder tree be changed — created, renamed, subscribed?
    /// `organize` deliberately stops short: it moves messages between
    /// folders that already exist and leaves the tree alone.
    pub fn may_change_folders(self) -> bool {
        self >= AccessLevel::Restructure
    }

    /// May this flag be *set*? Clearing is unrestricted above
    /// `ReadOnly`: taking a flag off a message never loses the message.
    pub fn may_set(self, flag: &str) -> bool {
        match self {
            AccessLevel::ReadOnly => false,
            AccessLevel::Organize | AccessLevel::Restructure => {
                !flag.eq_ignore_ascii_case("\\Deleted")
            }
            AccessLevel::Full => true,
        }
    }

    /// The name this level is written with in the config file.
    pub fn as_str(self) -> &'static str {
        match self {
            AccessLevel::ReadOnly => "readonly",
            AccessLevel::Organize => "organize",
            AccessLevel::Restructure => "restructure",
            AccessLevel::Full => "full",
        }
    }

    /// Parse a level as written on the command line.
    pub fn parse(name: &str) -> Result<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "readonly" | "read-only" => Ok(AccessLevel::ReadOnly),
            "organize" | "non-destructive" => Ok(AccessLevel::Organize),
            "restructure" => Ok(AccessLevel::Restructure),
            "full" => Ok(AccessLevel::Full),
            other => bail!(
                "unknown access level '{}' (readonly, organize, restructure or full)",
                other
            ),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Use implicit TLS (server port is a TLS port, e.g. 993).
    #[serde(default = "default_ssl")]
    pub ssl: bool,
    /// Use STARTTLS on a plain port (e.g. 143) when ssl is false.
    #[serde(default)]
    pub starttls: bool,
    /// Accept invalid TLS certificates (for self-signed local servers).
    #[serde(default)]
    pub insecure: bool,
    /// Default folder for commands that need one.
    #[serde(default = "default_folder")]
    pub folder: String,
    /// Maximum number of search results to fetch (0 = no limit).
    #[serde(default = "default_max")]
    pub max: usize,
    /// Sort spec for search/unread (`-S`/`--sort` overrides), e.g.
    /// "-date" or "subject,-size". None = default (most recent first).
    #[serde(default)]
    pub sort: Option<String>,
    /// Use the in-memory mock backend instead of a real IMAP server.
    /// Intended for testing/demos; the released binary talks to a real server.
    #[serde(default)]
    pub mock: bool,
    /// The hierarchy delimiter to build folder paths with, when the
    /// server's own answer is not to be trusted or not given (a `LIST`
    /// reporting NIL). Purely advisory: `info` reports it, and nothing
    /// in the tool rewrites a folder name — names go to the server as
    /// they are typed.
    #[serde(default)]
    pub delimiter: Option<String>,
    /// How much this tool may change on the server. The command line
    /// may narrow it further, never widen it.
    #[serde(default, rename = "access-level", alias = "access_level")]
    pub access: AccessLevel,
}

fn default_port() -> u16 {
    993
}

fn default_ssl() -> bool {
    true
}

fn default_folder() -> String {
    "INBOX".to_string()
}

fn default_max() -> usize {
    50
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: "localhost".to_string(),
            port: 993,
            username: "".to_string(),
            password: "".to_string(),
            ssl: true,
            starttls: false,
            insecure: false,
            folder: "INBOX".to_string(),
            max: default_max(),
            sort: None,
            mock: false,
            delimiter: None,
            access: AccessLevel::default(),
        }
    }
}

/// The config file this run reads: `--config`, else
/// `$MAIL_IMAP_CONFIG`, else the system-wide default. Resolved in one
/// place so `info` can report the file the rest of the run used.
pub fn config_path(path: Option<&str>) -> String {
    match path {
        Some(p) => p.to_string(),
        None => match std::env::var("MAIL_IMAP_CONFIG") {
            Ok(p) => p,
            Err(_) => "/etc/mail-imap.conf".to_string(),
        },
    }
}

pub fn load_config(path: Option<&str>) -> Result<Config> {
    let path = config_path(path);

    if !Path::new(&path).exists() {
        anyhow::bail!(
            "config file not found: {} (pass one with --config or set MAIL_IMAP_CONFIG)",
            path
        );
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("could not read config file: {}", path))?;
    parse_config(&content).with_context(|| format!("in config file: {}", path))
}

/// Parse config text, which is UCL.
///
/// UCL is a superset of JSON, so a config written as a JSON object
/// parses unchanged and every example that predates UCL still works.
/// The parsed object is emitted back as JSON and handed to serde
/// rather than being walked key by key: that keeps one definition of
/// the field names, their aliases and their defaults -- the serde
/// attributes on `Config` -- instead of a second one that would drift
/// from it.
pub fn parse_config(content: &str) -> Result<Config> {
    // `libucl::Parser::parse` builds a CString and unwraps, so a NUL
    // byte in the file would abort the process rather than fail.
    if content.as_bytes().contains(&0) {
        bail!("config is not text: it contains a NUL byte");
    }

    // NO_TIME: UCL reads a bare `30s` as a duration. Nothing here is a
    // duration, so a value that merely looks like one -- a password, a
    // folder name -- is better kept as the text it was written as.
    let parsed = Parser::with_flags(ParserFlags::NO_TIME)
        .parse(content)
        .map_err(|e| anyhow::anyhow!("could not parse config as UCL: {}", e))?;

    let json = Emitter::JSONCompact
        .emit(&parsed)
        .context("could not re-encode the parsed config")?;

    serde_json::from_str::<Config>(&json).context(
        "config parsed as UCL but is not valid for this tool \
         (unknown access level, or a field of the wrong type)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_ordered_from_least_to_most_permissive() {
        assert!(AccessLevel::ReadOnly < AccessLevel::Organize);
        assert!(AccessLevel::Organize < AccessLevel::Restructure);
        assert!(AccessLevel::Restructure < AccessLevel::Full);
        assert_eq!(AccessLevel::default(), AccessLevel::Organize);
    }

    #[test]
    fn readonly_permits_nothing() {
        let level = AccessLevel::ReadOnly;
        assert!(!level.may_store_flags());
        assert!(!level.may_set("\\Seen"));
        assert!(!level.may_set("invoice"));
    }

    #[test]
    fn organize_keeps_every_message() {
        let level = AccessLevel::Organize;
        assert!(level.may_store_flags());
        assert!(level.may_set("\\Seen") && level.may_set("\\Answered"));
        assert!(level.may_set("\\Flagged") && level.may_set("junk"));
        // The one flag whose purpose is removal.
        assert!(!level.may_set("\\Deleted"));
        assert!(!level.may_set("\\deleted"), "case-insensitively");
    }

    #[test]
    fn organize_leaves_the_folder_tree_alone() {
        assert!(!AccessLevel::ReadOnly.may_change_folders());
        assert!(
            !AccessLevel::Organize.may_change_folders(),
            "it files mail into folders that exist; it does not make them"
        );
        assert!(AccessLevel::Restructure.may_change_folders());
        assert!(AccessLevel::Full.may_change_folders());
    }

    #[test]
    fn restructure_still_refuses_to_delete_a_message() {
        // It is a rung about the folder tree, not about losing mail.
        assert!(!AccessLevel::Restructure.may_set("\\Deleted"));
        assert!(AccessLevel::Restructure.may_store_flags());
    }

    #[test]
    fn full_permits_everything() {
        assert!(AccessLevel::Full.may_set("\\Deleted"));
    }

    #[test]
    fn levels_parse_under_both_spellings() {
        assert_eq!(AccessLevel::parse("readonly").unwrap(), AccessLevel::ReadOnly);
        assert_eq!(AccessLevel::parse("read-only").unwrap(), AccessLevel::ReadOnly);
        assert_eq!(AccessLevel::parse("ORGANIZE").unwrap(), AccessLevel::Organize);
        assert_eq!(
            AccessLevel::parse("non-destructive").unwrap(),
            AccessLevel::Organize
        );
        assert_eq!(
            AccessLevel::parse("restructure").unwrap(),
            AccessLevel::Restructure
        );
        assert_eq!(AccessLevel::parse(" full ").unwrap(), AccessLevel::Full);
        assert!(AccessLevel::parse("nope").is_err());
        assert!(AccessLevel::parse("").is_err());
    }

    #[test]
    fn the_config_reads_the_hyphenated_key_and_defaults_without_it() {
        let with: Config = serde_json::from_str(
            r#"{"server":"s","username":"u","password":"p","access-level":"readonly"}"#,
        )
        .expect("parse");
        assert_eq!(with.access, AccessLevel::ReadOnly);
        let without: Config =
            serde_json::from_str(r#"{"server":"s","username":"u","password":"p"}"#)
                .expect("parse");
        assert_eq!(without.access, AccessLevel::Organize);
        assert!(serde_json::from_str::<Config>(
            r#"{"server":"s","username":"u","password":"p","access-level":"nope"}"#
        )
        .is_err());
    }

    // --------------------------------------------------- UCL parsing

    #[test]
    fn the_config_is_ucl_not_json() {
        // The shape the file actually takes: bare keys, no braces, no
        // commas, unquoted enum value.
        let cfg = parse_config(
            "server = \"imap.example.com\"\n\
             username = \"user@example.com\"\n\
             password = \"secret\"\n\
             access-level = full\n\
             max = 200\n",
        )
        .expect("UCL config should parse");
        assert_eq!(cfg.server, "imap.example.com");
        assert_eq!(cfg.username, "user@example.com");
        assert_eq!(cfg.access, AccessLevel::Full);
        assert_eq!(cfg.max, 200);
        assert_eq!(cfg.port, default_port(), "untouched field keeps its default");
    }

    #[test]
    fn json_is_still_a_valid_config_because_ucl_is_a_superset() {
        // Every config written before the switch, and every JSON
        // example in the documents, has to keep working.
        let cfg = parse_config(
            r#"{"server":"s","username":"u","password":"p","access-level":"readonly","max":7}"#,
        )
        .expect("JSON config should parse as UCL");
        assert_eq!(cfg.server, "s");
        assert_eq!(cfg.access, AccessLevel::ReadOnly);
        assert_eq!(cfg.max, 7);
    }

    #[test]
    fn ucl_underscore_alias_and_older_level_spellings_still_read() {
        let cfg = parse_config(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\n\
             access_level = non-destructive\n",
        )
        .expect("parse");
        assert_eq!(cfg.access, AccessLevel::Organize);
    }

    #[test]
    fn a_config_that_is_not_ucl_is_refused() {
        let err = parse_config("server = \"unterminated\nusername\n")
            .expect_err("broken UCL must not parse");
        let text = format!("{:#}", err);
        assert!(
            text.contains("UCL"),
            "the error should say the file is not UCL, got: {}",
            text
        );
    }

    #[test]
    fn an_unknown_access_level_fails_the_whole_file_rather_than_defaulting() {
        // Silently falling back to `organize` would widen what a
        // config meant to narrow: a typo in `readonly` must not become
        // permission to change mail.
        let err = parse_config(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\naccess-level = readonlyy\n",
        )
        .expect_err("an unknown level must be refused");
        assert!(format!("{:#}", err).contains("readonlyy"));
    }

    #[test]
    fn a_field_of_the_wrong_type_is_refused() {
        assert!(parse_config(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\nport = \"nope\"\n"
        )
        .is_err());
    }

    #[test]
    fn a_config_with_a_nul_byte_is_refused_rather_than_aborting() {
        // libucl builds a CString and unwraps; without the guard this
        // is a panic, not an error.
        let err = parse_config("server = \"s\"\0\n").expect_err("NUL must be refused");
        assert!(format!("{:#}", err).contains("NUL"));
    }

    #[test]
    fn a_value_that_looks_like_a_duration_stays_text() {
        // UCL reads a bare 30s as a duration; NO_TIME keeps it the
        // text it was written as, which is what a password needs.
        let cfg = parse_config("server = \"s\"\nusername = \"u\"\npassword = 30s\n")
            .expect("parse");
        assert_eq!(cfg.password, "30s");
    }
}
