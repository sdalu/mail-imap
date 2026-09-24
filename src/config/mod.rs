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
/// How this tool proves who it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
pub enum AuthMethod {
    /// `LOGIN` with a username and a password. What every server took
    /// until recently, and what several large ones no longer do.
    #[default]
    #[serde(rename = "login")]
    Login,
    /// `AUTHENTICATE XOAUTH2`: the secret is an OAuth 2 access token
    /// rather than a password. Google and Microsoft both speak this
    /// one; it is not an IETF standard, which is why the name carries
    /// a vendor X.
    #[serde(rename = "xoauth2")]
    XOAuth2,
}

impl AuthMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMethod::Login => "login",
            AuthMethod::XOAuth2 => "xoauth2",
        }
    }

    /// The capability a server must advertise for this method, or
    /// `None` where none is needed.
    pub fn required_capability(self) -> Option<&'static str> {
        match self {
            AuthMethod::Login => None,
            AuthMethod::XOAuth2 => Some("AUTH=XOAUTH2"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Default)]
pub enum AccessLevel {
    /// Change nothing at all. Reads use `BODY.PEEK[]`, so even `\Seen`
    /// stays as it was.
    ///
    /// Named for what the level is *for* rather than for what it
    /// withholds, which is also what keeps the ladder in one register:
    /// `survey`, `organize`, `restructure` are what a run does, and
    /// `readonly` was the one rung describing a restriction instead.
    /// The older spellings still read, and only read: `survey` is what
    /// is reported back.
    #[serde(rename = "survey", alias = "readonly", alias = "read-only")]
    Survey,
    /// Read, plus the changes that keep every message: set and clear
    /// flags and tags, and move mail to another folder. `\Deleted`
    /// cannot be *set* here — it is the one flag whose point is removal
    /// — though it can be cleared, which rescues a message rather than
    /// losing one.
    #[default]
    #[serde(rename = "organize")]
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

    /// May a message be rewritten — fetched, rebuilt without one of its
    /// parts, put back, and the original removed? `full`, because the
    /// part that goes is gone: `part strip` is the only operation here
    /// that destroys something inside a message rather than the message
    /// itself.
    pub fn may_strip_part(self) -> bool {
        self >= AccessLevel::Full
    }

    /// May a mailbox be deleted? Not `may_change_folders`: creating and
    /// renaming lose nothing, deleting loses a mailbox and everything
    /// in it, which is what `full` is for.
    pub fn may_delete_folder(self) -> bool {
        self >= AccessLevel::Full
    }

    /// May `UID EXPUNGE` remove messages already marked `\Deleted`?
    /// Setting `\Deleted` itself already needs `full` (`may_set`), and
    /// removing a message so marked is the same destruction, so this
    /// asks for the same level rather than a lesser one.
    pub fn may_expunge(self) -> bool {
        self >= AccessLevel::Full
    }

    /// May a message be put into a mailbox (`APPEND`)? Held to the same
    /// rung as `may_expunge`/setting `\Deleted`: putting mail into an
    /// account is as far from "nothing is lost" as taking it out is, so
    /// nothing below `full` does either.
    pub fn may_append(self) -> bool {
        self >= AccessLevel::Full
    }

    /// May the folder tree be changed — created, renamed, subscribed?
    /// `organize` deliberately stops short: it moves messages between
    /// folders that already exist and leaves the tree alone.
    pub fn may_change_folders(self) -> bool {
        self >= AccessLevel::Restructure
    }

    /// May this flag be *set*? Clearing is unrestricted above
    /// `Survey`: taking a flag off a message never loses the message.
    pub fn may_set(self, flag: &str) -> bool {
        match self {
            AccessLevel::Survey => false,
            AccessLevel::Organize | AccessLevel::Restructure => {
                !flag.eq_ignore_ascii_case("\\Deleted")
            }
            AccessLevel::Full => true,
        }
    }

    /// The name this level is written with in the config file.
    pub fn as_str(self) -> &'static str {
        match self {
            AccessLevel::Survey => "survey",
            AccessLevel::Organize => "organize",
            AccessLevel::Restructure => "restructure",
            AccessLevel::Full => "full",
        }
    }

    /// Parse a level as written on the command line.
    pub fn parse(name: &str) -> Result<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "survey" | "readonly" | "read-only" => Ok(AccessLevel::Survey),
            "organize" => Ok(AccessLevel::Organize),
            "restructure" => Ok(AccessLevel::Restructure),
            "full" => Ok(AccessLevel::Full),
            other => bail!(
                "unknown access level '{}' (survey, organize, restructure or full)",
                other
            ),
        }
    }
}

/// `deny_unknown_fields` because a key this tool does not know is
/// almost always a typo, and the field it was meant to be then takes
/// its default silently. That is worst for `access-level`: the suite
/// already refuses an unknown *value* so a typo cannot widen what the
/// config meant to narrow, and a typo in the *key* has to be refused
/// for the same reason -- `acess-level = readonly` otherwise runs at
/// the default `organize`, which may change mail.
/// A value that must never be printed.
///
/// The password used to be a plain `String` inside a `#[derive(Debug)]`
/// struct, which meant one `{:?}` of a `Config` anywhere -- a debug
/// line, a panic message, a future `dbg!` -- would have put it on a
/// stream. Nothing did that, so it was a trap rather than a leak; this
/// makes it structural instead of a thing to remember, and any secret
/// field added later gets the same protection by using this type.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// The value itself. Named so that reaching for it is visible at
    /// the call site.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    /// The account password, written in the clear. Mutually exclusive
    /// with `password-command`; exactly one of the two must be set,
    /// checked once by [`Config::check_password`] right after parsing.
    #[serde(default)]
    pub password: Option<Secret>,
    /// A command run through `sh -c` whose standard output is the
    /// password, so the secret can live in `pass`, `gpg`, a keyring or
    /// a vault instead of this file. See [`Config::effective_password`]
    /// for how the output is read. Mutually exclusive with `password`.
    #[serde(default, rename = "password-command")]
    pub password_command: Option<String>,
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
    /// Cap on search results. `0` is no cap, and is the default.
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
    /// Seconds of silence allowed on the session socket once it is
    /// established, `0` for none. Bounds a server that accepts the
    /// connection and then stalls mid-response; it does not bound the
    /// connect itself (see `real.rs::establish_session`, where it is
    /// applied, for why).
    #[serde(default = "default_timeout")]
    pub timeout: u64,

    /// How to authenticate: `login` (default) or `xoauth2`. Under
    /// `xoauth2` the secret — `password`, or whatever
    /// `password-command` prints — is an OAuth 2 access token rather
    /// than a password.
    #[serde(default)]
    pub auth: AuthMethod,
}

fn default_port() -> u16 {
    993
}

fn default_timeout() -> u64 {
    30
}

fn default_ssl() -> bool {
    true
}

fn default_folder() -> String {
    "INBOX".to_string()
}

/// No cap. A search returns what it matched, and the caller decides
/// what to do with it; `-M`/`--max` is there when that is too much.
fn default_max() -> usize {
    0
}

impl Config {
    /// `password` and `password-command` are mutually exclusive, and
    /// one of them must be set -- there is no default password. Called
    /// once, right after parsing, so every other reader of `Config`
    /// (including a hand-built one such as `Config::default()`) can
    /// assume the check already happened rather than repeat it.
    fn check_password(&self) -> Result<()> {
        match (&self.password, &self.password_command) {
            (Some(_), Some(_)) => bail!(
                "both 'password' and 'password-command' are set; use exactly one"
            ),
            (None, None) => bail!(
                "neither 'password' nor 'password-command' is set; there is no \
                 default password"
            ),
            _ => Ok(()),
        }
    }

    /// The password to log in with: `password` verbatim, or the output
    /// of running `password-command` through the shell.
    ///
    /// `check_password` (run once, at parse time) has already ruled out
    /// both fields being set or neither -- the final `bail!` below is
    /// only for a `Config` assembled by hand elsewhere (tests,
    /// `Config::default()`) without going through it, so that path
    /// fails loudly instead of reaching a server with an empty password.
    pub fn effective_password(&self) -> Result<String> {
        if let Some(p) = &self.password {
            return Ok(p.expose().to_string());
        }
        if let Some(cmd) = &self.password_command {
            return run_password_command(cmd);
        }
        bail!("no password configured: set 'password' or 'password-command'")
    }
}

/// Run `command` through `sh -c` and read the password from its
/// standard output, so a user can write a pipeline or a command with
/// arguments without this tool parsing quoting rules of its own.
///
/// Deliberately no timeout: `gpg`, `pass` and similar may sit at a
/// pinentry prompt waiting on the user, and cutting that wait short
/// would be taking a decision -- whether to give up -- that belongs to
/// the person entering the passphrase, not to this tool.
///
/// Exactly one trailing `\n` is trimmed, and nothing else: `pass` and
/// its kin emit one, but a password may legitimately end in a space,
/// and `trim()`/`trim_end()` would silently hand back a different
/// password than the one stored.
///
/// The password itself never appears in any error this returns -- only
/// the exit status and stderr do, since stderr is where a diagnostic
/// such as `gpg: decryption failed` appears.
fn run_password_command(command: &str) -> Result<String> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .context("could not run password-command")?;

    if !output.status.success() {
        bail!(
            "password-command failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut password = String::from_utf8(output.stdout)
        .map_err(|_| anyhow::anyhow!("password-command did not print valid UTF-8"))?;
    if password.ends_with('\n') {
        password.pop();
    }
    if password.is_empty() {
        bail!("password-command produced an empty password");
    }
    Ok(password)
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: "localhost".to_string(),
            // Through the same functions serde uses for a field the
            // config file omits, rather than the same values written
            // again: two copies of a default drift, and the drift is
            // silent -- a built-in default of `ssl: true` beside a
            // parsed default of `false` differ only for the user whose
            // config left the field out.
            port: default_port(),
            username: "".to_string(),
            password: None,
            password_command: None,
            ssl: default_ssl(),
            starttls: false,
            insecure: false,
            folder: default_folder(),
            max: default_max(),
            sort: None,
            mock: false,
            delimiter: None,
            access: AccessLevel::default(),
            timeout: default_timeout(),
            auth: AuthMethod::Login,
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
///
/// Two lines of environment reading over `user_config_dir_from`, which
/// holds the rule and is tested. This wrapper is deliberately not:
/// asserting anything about it means setting process-wide environment
/// variables under a threaded test runner, which is a worse trade than
/// leaving a function this thin uncovered. Mutation testing reports it
/// as a survivor for that reason, not because the rule is untested.
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
    config.check_password()?;
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

    /// The defaults are documented values, and the README's config
    /// table is the other copy of them.
    ///
    /// Nothing tied the two together: every one of these `default_*`
    /// functions could be replaced with `0`, `false` or the empty
    /// string and the suite passed. The failures that invites are
    /// quiet ones -- a `default_ssl` of `false` sends the password to
    /// port 993 in the clear, and a `default_timeout` of `0` turns off
    /// the only bound on a server that stalls.
    #[test]
    fn the_defaults_are_the_ones_the_readme_documents() {
        let c = Config::default();
        assert_eq!(c.port, 993, "port");
        assert_eq!(c.timeout, 30, "timeout, in seconds");
        assert!(c.ssl, "implicit TLS, which is what port 993 means");
        assert!(!c.starttls, "not both at once");
        assert!(!c.insecure, "certificates are verified");
        assert_eq!(c.folder, "INBOX", "default folder");
        assert_eq!(c.max, 0, "no cap: a search returns what it matched");
        assert_eq!(c.access, AccessLevel::Organize, "neither survey nor full");
        assert_eq!(c.auth, AuthMethod::Login);
        assert!(!c.mock, "the real backend unless asked otherwise");

        // And a config file that simply omits those fields gets the
        // same answers. These are two code paths -- `Config::default()`
        // and serde's per-field defaults -- and they were two copies of
        // the same values until this test went in: `port`, `ssl` and
        // `folder` were written out again in the `Default` impl, where
        // they could drift from what a real config file resolves to
        // without a single test failing.
        let minimal: Config =
            serde_json::from_str(r#"{"server":"s","username":"u","password":"p"}"#)
                .expect("a config may name only the essentials");
        assert_eq!(minimal.port, c.port, "port");
        assert_eq!(minimal.timeout, c.timeout, "timeout");
        assert_eq!(minimal.ssl, c.ssl, "ssl");
        assert_eq!(minimal.folder, c.folder, "folder");
        assert_eq!(minimal.max, c.max, "max");
        assert_eq!(minimal.access, c.access, "access-level");
        assert_eq!(minimal.auth, c.auth, "auth");
    }

    /// `required_capability` is what stops an XOAUTH2 run against a
    /// server that never offered it, and the atom has to be exact --
    /// it is compared against what `CAPABILITY` returned. Both this
    /// and `as_str` (which `info` prints and the config parses back)
    /// could be replaced wholesale without the suite noticing.
    #[test]
    fn the_auth_methods_name_themselves_and_their_capability_exactly() {
        assert_eq!(AuthMethod::Login.as_str(), "login");
        assert_eq!(AuthMethod::XOAuth2.as_str(), "xoauth2");
        assert_eq!(AuthMethod::Login.required_capability(), None);
        assert_eq!(
            AuthMethod::XOAuth2.required_capability(),
            Some("AUTH=XOAUTH2"),
            "the atom is matched against CAPABILITY, so it is exact or it is useless"
        );
        // The names are also what the config spells, so the pair has
        // to round-trip or a written config stops meaning what it says.
        for m in [AuthMethod::Login, AuthMethod::XOAuth2] {
            let text = format!(r#"{{"server":"s","username":"u","password":"p","auth":"{}"}}"#, m.as_str());
            let parsed: Config = serde_json::from_str(&text).expect("auth name parses back");
            assert_eq!(parsed.auth, m);
        }
    }

    /// Naming a file that is not there says so; searching and finding
    /// nothing says what was searched.
    ///
    /// The two messages send a reader to different places -- one to a
    /// typo in their own `--config`, the other to the fact that a
    /// search happened at all and where it looked -- and the guard that
    /// tells them apart is `candidates.len() == 1`. Forcing it false
    /// survived the suite: a missing `--config foo.conf` would then be
    /// reported as "no config file found: tried foo.conf", which reads
    /// as a search that never happened.
    #[test]
    fn a_named_config_that_is_missing_is_not_reported_as_a_failed_search() {
        let err = match load_config(Some("/nonexistent/mail-imap-test.conf"), None) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("that path does not exist"),
        };
        assert!(
            err.contains("config file not found: /nonexistent/mail-imap-test.conf"),
            "{}",
            err
        );
        assert!(!err.contains("tried"), "nothing was searched: {}", err);
    }

    #[test]
    fn levels_are_ordered_from_least_to_most_permissive() {
        assert!(AccessLevel::Survey < AccessLevel::Organize);
        assert!(AccessLevel::Organize < AccessLevel::Restructure);
        assert!(AccessLevel::Restructure < AccessLevel::Full);
        assert_eq!(AccessLevel::default(), AccessLevel::Organize);
    }

    #[test]
    fn survey_permits_nothing() {
        let level = AccessLevel::Survey;
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
        assert!(!AccessLevel::Survey.may_change_folders());
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
        assert!(AccessLevel::Full.may_expunge());
        assert!(AccessLevel::Full.may_append());
    }

    #[test]
    fn only_full_may_expunge() {
        // Same rung as setting \Deleted itself: marking a message for
        // removal and removing it are both destruction.
        assert!(!AccessLevel::Survey.may_expunge());
        assert!(!AccessLevel::Organize.may_expunge());
        assert!(!AccessLevel::Restructure.may_expunge());
        assert!(AccessLevel::Full.may_expunge());
    }

    #[test]
    fn only_full_may_append() {
        assert!(!AccessLevel::Survey.may_append());
        assert!(!AccessLevel::Organize.may_append());
        assert!(!AccessLevel::Restructure.may_append());
        assert!(AccessLevel::Full.may_append());
    }

    #[test]
    fn levels_parse_under_both_spellings() {
        assert_eq!(AccessLevel::parse("survey").unwrap(), AccessLevel::Survey);
        assert_eq!(AccessLevel::parse(" SURVEY ").unwrap(), AccessLevel::Survey);
        // `readonly` and `read-only` were what this level was called
        // before the ladder was put in one register, and they still
        // read.
        assert_eq!(AccessLevel::parse("readonly").unwrap(), AccessLevel::Survey);
        assert_eq!(AccessLevel::parse("read-only").unwrap(), AccessLevel::Survey);
        // An alias is a way in, not a second name to report back: one
        // level answers with one spelling whichever way it was written.
        assert_eq!(AccessLevel::parse("readonly").unwrap().as_str(), "survey");
        assert_eq!(AccessLevel::parse("ORGANIZE").unwrap(), AccessLevel::Organize);
        // `non-destructive` was an older spelling of `organize` and is
        // gone: one level, one name. It is refused like any other
        // unknown value rather than silently meaning something.
        let err = AccessLevel::parse("non-destructive").expect_err("dropped spelling");
        assert!(err.to_string().contains("unknown access level"), "{}", err);
        assert!(err.to_string().contains("organize"), "and lists what to write: {}", err);
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
        assert_eq!(with.access, AccessLevel::Survey);
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
        assert_eq!(cfg.access, AccessLevel::Survey);
        assert_eq!(cfg.max, 7);
    }

    #[test]
    fn ucl_underscore_alias_and_older_level_spellings_still_read() {
        // `access_level` for `access-level`, and `readonly` /
        // `read-only` for `survey`: the spellings that remain.
        // (`non-destructive` was dropped -- one level, one name.)
        let cfg = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\n\
             access_level = read-only\n",
        )
        .expect("parse");
        assert_eq!(cfg.access, AccessLevel::Survey);
        // serde reads the alias too, which is a separate path from
        // `AccessLevel::parse` and so needs its own line: `--access-level
        // survey` goes through one, `access-level = survey` the other.
        let cfg = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\n\
             access-level = survey\n",
        )
        .expect("parse");
        assert_eq!(cfg.access, AccessLevel::Survey);
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
    fn an_unknown_key_is_refused_rather_than_silently_defaulted() {
        // The mirror of `an_unknown_access_level_fails_...`: a typo in
        // the KEY used to drop the setting and take the default, so
        // `acess-level = readonly` ran at `organize` -- which may
        // change mail. Both halves of the typo must fail the file.
        let err = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\nacess-level = readonly\n",
        )
        .expect_err("an unknown key must be refused");
        let text = format!("{:#}", err);
        assert!(text.contains("acess-level"), "{}", text);
        assert!(text.contains("access-level"), "it should name the real key: {}", text);
        // A key that is merely unknown, not a near-miss, fails too.
        assert!(parse_one(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\nnonsense = 1\n"
        )
        .is_err());
        // Both spellings of the real key still work.
        assert!(parse_one(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\naccess_level = full\n"
        )
        .is_ok());
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
        assert_eq!(cfg.password.as_ref().map(Secret::expose), Some("30s"));
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
        assert_eq!(home.access, AccessLevel::Survey, "shared default stands");
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
    fn a_search_is_uncapped_unless_the_config_says_otherwise() {
        // 0 is no cap, and is what a config that says nothing gets.
        let (c, _) = parse_one_pair("server = \"h\"\nusername = \"u\"\npassword = \"p\"\n");
        assert_eq!(c.max, 0);
        assert_eq!(Config::default().max, 0);
        let (c, _) =
            parse_one_pair("server = \"h\"\nusername = \"u\"\npassword = \"p\"\nmax = 20\n");
        assert_eq!(c.max, 20);
    }

    fn parse_one_pair(content: &str) -> (Config, Option<String>) {
        parse_config(content, None).expect("parse")
    }

    #[test]
    fn a_shared_setting_is_a_default_and_not_a_ceiling() {
        // Deliberate, and the reason is that the config file is
        // trusted: it already holds the password, so whoever can edit
        // it can reach the account by any means. access-level guards
        // against *this tool* doing more than was meant, and the
        // profile is where that intent is written -- so a profile
        // raising a shared level is the author saying so, not a leak.
        //
        // The command line is the other way round on purpose: it may
        // only narrow. That asymmetry is the point, not an oversight,
        // and this test exists so it is not "fixed".
        let cfg = "access-level = readonly\n\
                   server = \"h\"\nusername = \"u\"\npassword = \"p\"\n\
                   risky { access-level = full }\n";
        let (c, _) = parse_config(cfg, Some("risky")).expect("parse");
        assert_eq!(c.access, AccessLevel::Full);
    }

    #[test]
    fn every_setting_can_be_shared_not_a_chosen_few() {
        // The rule is structural -- any top-level scalar but `default`
        // -- so there is no list of shareable fields to keep in step
        // with `Config`.
        let cfg = "server = \"h\"\nport = 143\npassword = \"p\"\n\
                   ssl = false\nstarttls = true\ninsecure = true\n\
                   folder = \"Archive\"\nmax = 5\nsort = \"-date\"\n\
                   delimiter = \".\"\naccess-level = restructure\n\
                   me { username = \"me@h\" }\n";
        let (c, _) = parse_config(cfg, Some("me")).expect("parse");
        assert_eq!(c.username, "me@h");
        assert_eq!((c.port, c.ssl, c.starttls, c.insecure), (143, false, true, true));
        assert_eq!((c.folder.as_str(), c.max), ("Archive", 5));
        assert_eq!(c.sort.as_deref(), Some("-date"));
        assert_eq!(c.delimiter.as_deref(), Some("."));
        assert_eq!(c.access, AccessLevel::Restructure);
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

    // ---------------------------------------------- password-command

    #[test]
    fn password_and_password_command_together_is_refused_naming_both() {
        let err = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword = \"p\"\n\
             password-command = \"echo p\"\n",
        )
        .expect_err("both must be refused");
        let text = format!("{:#}", err);
        assert!(text.contains("password") && text.contains("password-command"), "{}", text);
    }

    #[test]
    fn neither_password_nor_password_command_is_refused() {
        // There is no default password: silently connecting with an
        // empty one would be worse than refusing to run at all.
        let err = parse_one("server = \"s\"\nusername = \"u\"\n").expect_err("must refuse");
        let text = format!("{:#}", err);
        assert!(text.contains("password") && text.contains("password-command"), "{}", text);
    }

    #[test]
    fn password_command_is_read_hyphenated() {
        let cfg = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword-command = \"printf hunter2\"\n",
        )
        .expect("parse");
        assert!(cfg.password.is_none());
        assert_eq!(cfg.password_command.as_deref(), Some("printf hunter2"));
    }

    #[test]
    fn debugging_a_config_never_prints_the_password() {
        // The point of the Secret newtype. If this ever fails, one
        // `{:?}` somewhere puts a credential on a stream.
        let cfg = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword = \"hunter2\"\n",
        )
        .expect("parse");
        let shown = format!("{:?}", cfg);
        assert!(
            !shown.contains("hunter2"),
            "the password reached a Debug output: {}",
            shown
        );
        assert!(shown.contains("redacted"), "{}", shown);
    }

    #[test]
    fn effective_password_returns_the_literal_password_unchanged() {
        let cfg = parse_one("server = \"s\"\nusername = \"u\"\npassword = \"hunter2\"\n")
            .expect("parse");
        assert_eq!(cfg.effective_password().expect("password").as_str(), "hunter2");
    }

    #[test]
    fn effective_password_runs_the_command_through_the_shell() {
        // A pipeline, not just a bare command: proof it really goes
        // through `sh -c` rather than being exec'd word-split.
        let cfg = parse_one(
            "server = \"s\"\nusername = \"u\"\n\
             password-command = \"printf hunter2 | cat\"\n",
        )
        .expect("parse");
        assert_eq!(cfg.effective_password().expect("password").as_str(), "hunter2");
    }

    #[test]
    fn effective_password_strips_exactly_one_trailing_newline() {
        // `pass` and friends emit one trailing newline; only it goes.
        let cfg = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword-command = \"printf 'hunter2\\n'\"\n",
        )
        .expect("parse");
        assert_eq!(cfg.effective_password().expect("password").as_str(), "hunter2");

        // A second, genuine newline in the output is part of the
        // password and must survive -- only ONE is ever stripped.
        let cfg = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword-command = \"printf 'hunter2\\n\\n'\"\n",
        )
        .expect("parse");
        assert_eq!(cfg.effective_password().expect("password").as_str(), "hunter2\n");
    }

    #[test]
    fn effective_password_keeps_a_trailing_space() {
        // trim()/trim_end() would silently produce a different password
        // than the one actually stored -- this is the case that guards
        // against reintroducing either.
        let cfg = parse_one(
            "server = \"s\"\nusername = \"u\"\npassword-command = \"printf 'hunter2 \\n'\"\n",
        )
        .expect("parse");
        assert_eq!(cfg.effective_password().expect("password").as_str(), "hunter2 ");
    }

    #[test]
    fn effective_password_fails_on_a_nonzero_exit_with_stderr_in_the_message() {
        let cfg = parse_one(
            "server = \"s\"\nusername = \"u\"\n\
             password-command = \"echo 'gpg: decryption failed' >&2; exit 2\"\n",
        )
        .expect("parse");
        let err = cfg.effective_password().expect_err("must fail");
        let text = format!("{:#}", err);
        assert!(text.contains("decryption failed"), "{}", text);
        assert!(text.contains('2'), "should mention the exit status: {}", text);
    }

    #[test]
    fn effective_password_fails_on_empty_output() {
        let cfg = parse_one("server = \"s\"\nusername = \"u\"\npassword-command = \"true\"\n")
            .expect("parse");
        let err = cfg.effective_password().expect_err("must fail");
        assert!(format!("{:#}", err).contains("empty"));
    }

    #[test]
    fn effective_password_fails_on_output_that_is_only_a_newline() {
        // After stripping the one trailing newline this is empty too.
        let cfg =
            parse_one("server = \"s\"\nusername = \"u\"\npassword-command = \"printf '\\n'\"\n")
                .expect("parse");
        let err = cfg.effective_password().expect_err("must fail");
        assert!(format!("{:#}", err).contains("empty"));
    }

    #[test]
    fn a_config_without_password_command_falls_back_to_password_when_both_absent_in_code() {
        // Guards effective_password's own defensive branch: a
        // hand-built Config (never through check_password) with
        // neither field set fails loudly rather than logging in with
        // an empty string.
        let cfg = Config { server: "s".to_string(), ..Config::default() };
        let err = cfg.effective_password().expect_err("must fail");
        assert!(format!("{:#}", err).contains("password"));
    }
}
