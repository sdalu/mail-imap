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
    /// flags and tags, and move mail to another folder. `\Deleted`
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

/// Where a run looks for its config, in order.
///
/// `--config` and `$MAIL_IMAP_CONFIG` are answers, not candidates: if
/// either names a file that is not there, that is an error rather than
/// a reason to go and read somebody else's config. Only the defaults
/// are searched, user file first:
///
/// ```text
///   --config PATH            named outright, must exist
///   $MAIL_IMAP_CONFIG        the same, from the environment
///   ~/.config/mail-imap.conf the user's own
///   /etc/mail-imap.conf      the machine's
/// ```
pub fn config_candidates(path: Option<&str>) -> Vec<String> {
    candidates_from(
        path,
        std::env::var("MAIL_IMAP_CONFIG").ok().as_deref(),
        user_config_dir().as_deref(),
    )
}

/// The candidate list, with the environment passed in rather than read,
/// so it can be tested without a process-wide mutation.
fn candidates_from(
    explicit: Option<&str>,
    env: Option<&str>,
    user_dir: Option<&str>,
) -> Vec<String> {
    if let Some(p) = explicit.filter(|p| !p.is_empty()) {
        return vec![p.to_string()];
    }
    if let Some(p) = env.filter(|p| !p.is_empty()) {
        return vec![p.to_string()];
    }
    let mut out = Vec::new();
    if let Some(dir) = user_dir.filter(|d| !d.is_empty()) {
        out.push(format!("{}/mail-imap.conf", dir.trim_end_matches('/')));
    }
    out.push(SYSTEM_CONFIG.to_string());
    out
}

const SYSTEM_CONFIG: &str = "/etc/mail-imap.conf";

/// `$XDG_CONFIG_HOME`, else `~/.config` -- which is what `~/.config`
/// means on a machine that sets it.
fn user_config_dir() -> Option<String> {
    user_config_dir_from(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

fn user_config_dir_from(xdg: Option<&str>, home: Option<&str>) -> Option<String> {
    if let Some(d) = xdg.filter(|d| !d.is_empty()) {
        return Some(d.to_string());
    }
    home.filter(|h| !h.is_empty())
        .map(|h| format!("{}/.config", h.trim_end_matches('/')))
}

/// A config as loaded: the settings, which named profile they came
/// from, and the file they were read out of.
///
/// The three travel together because `info` reports all three and they
/// have to agree: resolving the path a second time could name a
/// different file, and the profile is not recoverable from `Config`.
pub struct Loaded {
    pub config: Config,
    /// The profile in force, or `None` when the file has none.
    pub profile: Option<String>,
    pub path: String,
}

/// The key naming the profile to use when none is asked for.
const DEFAULT_KEY: &str = "default";

pub fn load_config(path: Option<&str>, profile: Option<&str>) -> Result<Loaded> {
    let candidates = config_candidates(path);
    let found = match candidates.iter().find(|p| Path::new(p).exists()) {
        Some(p) => p.clone(),
        // One candidate means it was named outright, so say which file
        // is missing rather than listing a search that never happened.
        None if candidates.len() == 1 => {
            bail!("config file not found: {}", candidates[0])
        }
        None => bail!(
            "no config file found: tried {} (name one with --config, or set MAIL_IMAP_CONFIG)",
            candidates.join(", ")
        ),
    };

    let content = fs::read_to_string(&found)
        .with_context(|| format!("could not read config file: {}", found))?;
    let (config, name) =
        parse_config(&content, profile).with_context(|| format!("in config file: {}", found))?;
    Ok(Loaded { config, profile: name, path: found })
}

/// Parse config text, which is UCL, and select a profile from it.
///
/// UCL is a superset of JSON, so a config written as a JSON object
/// parses unchanged and every example that predates UCL still works.
/// The parsed object is emitted back as JSON and handed to serde
/// rather than being walked key by key: that keeps one definition of
/// the field names, their aliases and their defaults -- the serde
/// attributes on `Config` -- instead of a second one that would drift
/// from it.
pub fn parse_config(content: &str, profile: Option<&str>) -> Result<(Config, Option<String>)> {
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
    let value: serde_json::Value =
        serde_json::from_str(&json).context("could not re-read the parsed config")?;

    let (selected, name) = select_profile(value, profile)?;

    let config = serde_json::from_value::<Config>(selected).context(
        "config parsed as UCL but is not valid for this tool \
         (unknown access level, or a field of the wrong type)",
    )?;
    Ok((config, name))
}

/// Pick the profile out of a parsed config.
///
/// A profile is a top-level key whose value is an object; no setting is
/// one, so the two cannot be confused. Scalars beside the profiles are
/// shared defaults a profile may override, which is what lets `max` or
/// `access-level` be written once for a whole file.
///
/// A file with no profiles is returned as it stands -- which is every
/// config written before profiles existed.
fn select_profile(
    value: serde_json::Value,
    wanted: Option<&str>,
) -> Result<(serde_json::Value, Option<String>)> {
    let serde_json::Value::Object(map) = value else {
        bail!("config is not a set of settings");
    };

    let profiles: Vec<String> = map
        .iter()
        .filter(|(_, v)| v.is_object())
        .map(|(k, _)| k.clone())
        .collect();

    if profiles.is_empty() {
        if let Some(name) = wanted {
            bail!(
                "no profile '{}': this config has no named profiles, so it \
                 describes one account and there is nothing to select",
                name
            );
        }
        return Ok((serde_json::Value::Object(map), None));
    }

    let default = map.get(DEFAULT_KEY).and_then(|v| v.as_str()).map(String::from);
    let name = match wanted.map(String::from).or(default) {
        Some(n) => n,
        // Choosing for the caller means guessing which account to reach,
        // and the wrong guess connects to the wrong mailbox.
        None => bail!(
            "the config has named profiles but none was selected: {} \
             (choose one with -p, or set {} = \"...\" in the config)",
            profiles.join(", "),
            DEFAULT_KEY
        ),
    };

    let Some(serde_json::Value::Object(chosen)) = map.get(&name) else {
        bail!("no profile '{}' in the config; it has {}", name, profiles.join(", "));
    };

    // Shared defaults first, the profile's own settings over them.
    let mut merged = serde_json::Map::new();
    for (k, v) in &map {
        if !v.is_object() && k != DEFAULT_KEY {
            merged.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in chosen {
        merged.insert(k.clone(), v.clone());
    }
    Ok((serde_json::Value::Object(merged), Some(name)))
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
            "it moves mail into folders that exist; it does not make them"
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

    /// The common case: no profile asked for, settings only.
    fn parse_one(content: &str) -> Result<Config> {
        parse_config(content, None).map(|(c, _)| c)
    }

    // --------------------------------------------------- UCL parsing

    #[test]
    fn the_config_is_ucl_not_json() {
        // The shape the file actually takes: bare keys, no braces, no
        // commas, unquoted enum value.
        let cfg = parse_one(
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
        let cfg = parse_one(
            r#"{"server":"s","username":"u","password":"p","access-level":"readonly","max":7}"#,
        )
        .expect("JSON config should parse as UCL");
        assert_eq!(cfg.server, "s");
        assert_eq!(cfg.access, AccessLevel::ReadOnly);
        assert_eq!(cfg.max, 7);
    }

    #[test]
    fn ucl_underscore_alias_and_older_level_spellings_still_read() {
        let cfg = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\n\
             access_level = non-destructive\n",
        )
        .expect("parse");
        assert_eq!(cfg.access, AccessLevel::Organize);
    }

    #[test]
    fn a_config_that_is_not_ucl_is_refused() {
        let err = parse_one("server = \"unterminated\nusername\n")
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
        let err = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\naccess-level = readonlyy\n",
        )
        .expect_err("an unknown level must be refused");
        assert!(format!("{:#}", err).contains("readonlyy"));
    }

    #[test]
    fn a_field_of_the_wrong_type_is_refused() {
        assert!(parse_one(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\nport = \"nope\"\n"
        )
        .is_err());
    }

    #[test]
    fn a_config_with_a_nul_byte_is_refused_rather_than_aborting() {
        // libucl builds a CString and unwraps; without the guard this
        // is a panic, not an error.
        let err = parse_one("server = \"s\"\0\n").expect_err("NUL must be refused");
        assert!(format!("{:#}", err).contains("NUL"));
    }

    #[test]
    fn a_value_that_looks_like_a_duration_stays_text() {
        // UCL reads a bare 30s as a duration; NO_TIME keeps it the
        // text it was written as, which is what a password needs.
        let cfg = parse_one("server = \"s\"\nusername = \"u\"\npassword = 30s\n")
            .expect("parse");
        assert_eq!(cfg.password, "30s");
    }

    // --------------------------------------- where the config lives

    #[test]
    fn config_is_looked_for_in_the_user_dir_then_the_machine() {
        assert_eq!(
            candidates_from(None, None, Some("/home/u/.config")),
            vec!["/home/u/.config/mail-imap.conf", "/etc/mail-imap.conf"]
        );
    }

    #[test]
    fn a_named_config_is_the_only_candidate() {
        // --config naming a file that is not there must fail on that
        // file, not quietly read the user's or the machine's instead.
        assert_eq!(
            candidates_from(Some("/tmp/x.conf"), Some("/env.conf"), Some("/home/u/.config")),
            vec!["/tmp/x.conf"]
        );
        assert_eq!(
            candidates_from(None, Some("/env.conf"), Some("/home/u/.config")),
            vec!["/env.conf"]
        );
    }

    #[test]
    fn the_machine_config_is_the_last_resort_even_with_no_home() {
        assert_eq!(candidates_from(None, None, None), vec!["/etc/mail-imap.conf"]);
    }

    #[test]
    fn an_empty_setting_is_not_a_setting() {
        // An unset variable and one set to "" arrive differently but
        // mean the same thing.
        assert_eq!(
            candidates_from(Some(""), Some(""), Some("")),
            vec!["/etc/mail-imap.conf"]
        );
    }

    #[test]
    fn the_user_dir_is_xdg_when_set_and_dot_config_otherwise() {
        assert_eq!(
            user_config_dir_from(None, Some("/home/u")),
            Some("/home/u/.config".to_string())
        );
        assert_eq!(
            user_config_dir_from(Some("/elsewhere"), Some("/home/u")),
            Some("/elsewhere".to_string())
        );
        assert_eq!(user_config_dir_from(None, None), None);
        assert_eq!(user_config_dir_from(Some(""), Some("")), None);
    }

    #[test]
    fn a_trailing_slash_does_not_double_up() {
        assert_eq!(
            candidates_from(None, None, Some("/home/u/.config/")),
            vec!["/home/u/.config/mail-imap.conf", "/etc/mail-imap.conf"]
        );
        assert_eq!(
            user_config_dir_from(None, Some("/home/u/")),
            Some("/home/u/.config".to_string())
        );
    }

    // -------------------------------------------------- profiles

    const TWO: &str = "max = 7\n\
                       access-level = readonly\n\
                       work {\n\
                         server = \"work.example\"\n\
                         username = \"me@work\"\n\
                         password = \"w\"\n\
                         access-level = organize\n\
                       }\n\
                       home {\n\
                         server = \"home.example\"\n\
                         username = \"me\"\n\
                         password = \"h\"\n\
                       }\n";

    #[test]
    fn a_profile_is_selected_by_name() {
        let (cfg, name) = parse_config(TWO, Some("work")).expect("parse");
        assert_eq!(name.as_deref(), Some("work"));
        assert_eq!(cfg.server, "work.example");
        assert_eq!(cfg.username, "me@work");
    }

    #[test]
    fn settings_beside_the_profiles_are_shared_and_may_be_overridden() {
        // max is written once for the file; access-level is written
        // once and then overridden by the profile that needs more.
        let (work, _) = parse_config(TWO, Some("work")).expect("parse");
        assert_eq!(work.max, 7, "shared default reaches the profile");
        assert_eq!(work.access, AccessLevel::Organize, "the profile wins");
        let (home, _) = parse_config(TWO, Some("home")).expect("parse");
        assert_eq!(home.max, 7);
        assert_eq!(home.access, AccessLevel::ReadOnly, "shared default stands");
    }

    #[test]
    fn the_default_key_chooses_when_nothing_is_asked_for() {
        let with_default = format!("default = \"home\"\n{}", TWO);
        let (cfg, name) = parse_config(&with_default, None).expect("parse");
        assert_eq!(name.as_deref(), Some("home"));
        assert_eq!(cfg.server, "home.example");
        // and -p still overrides it
        let (cfg, name) = parse_config(&with_default, Some("work")).expect("parse");
        assert_eq!(name.as_deref(), Some("work"));
        assert_eq!(cfg.server, "work.example");
    }

    #[test]
    fn the_default_key_is_not_mistaken_for_a_setting() {
        let with_default = format!("default = \"home\"\n{}", TWO);
        let (cfg, _) = parse_config(&with_default, None).expect("parse");
        assert_eq!(cfg.server, "home.example");
        assert_eq!(cfg.max, 7);
    }

    #[test]
    fn profiles_with_nothing_selected_is_refused_rather_than_guessed() {
        // Guessing an account means possibly reaching the wrong
        // mailbox, which is the failure worth refusing over.
        let err = parse_config(TWO, None).expect_err("must not guess");
        let text = format!("{:#}", err);
        assert!(text.contains("work") && text.contains("home"), "{}", text);
    }

    #[test]
    fn an_unknown_profile_is_refused_and_says_what_there_is() {
        let err = parse_config(TWO, Some("nope")).expect_err("must refuse");
        let text = format!("{:#}", err);
        assert!(text.contains("nope") && text.contains("work"), "{}", text);
    }

    #[test]
    fn a_config_without_profiles_is_unchanged_and_names_none() {
        // Every config written before profiles existed.
        let (cfg, name) =
            parse_config("server = \"s\"\nusername = \"u\"\npassword = \"p\"\n", None)
                .expect("parse");
        assert_eq!(name, None);
        assert_eq!(cfg.server, "s");
    }

    #[test]
    fn asking_for_a_profile_of_a_config_that_has_none_is_refused() {
        // Silently ignoring -p would let a typo run against whatever
        // single account the file describes.
        let err = parse_config(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\n",
            Some("work"),
        )
        .expect_err("must refuse");
        assert!(format!("{:#}", err).contains("work"));
    }
}
