use clap::Parser;
use std::process;

mod cli;
mod config;
mod imap;

use cli::select::{parse_selections, Selection};
use cli::FolderSpec;

/// Text shown under every command that takes a message selection.
const SELECTION_HELP: &str = "\
A message selection is [FOLDER::]UIDS — 5, 1,4,7, 1-9 (a UID range), '*' \
(every message, and '*' is only ever that), 9- (from 9 to the end of the \
mailbox), last:20 / first:5 (a count of messages, which no range can \
express), Archive::1-5. '::' binds the folder; a single ':' introduces a \
count and nothing else, so IMAP's own range operator is not taken — write \
1-5, not 1:5. A range is an \
interval of the UID space, so it may match fewer messages than its span. \
Without a folder a selection means the -f folder (or \
\"folder\" from the config).";

#[derive(Parser)]
#[clap(name = "mail-imap", version = env!("CARGO_PKG_VERSION"), author = "AI Assistant")]
struct Args {
    /// Configuration file path (or set MAIL_IMAP_CONFIG)
    /// Profile to use from a config that names several accounts.
    /// Without it the config's `default = "..."` decides; a config
    /// with no profiles needs neither
    #[clap(short = 'p', long = "profile", global = true, value_name = "NAME")]
    profile: Option<String>,

    #[clap(short = 'c', long = "config")]
    config_file: Option<String>,

    /// Folder(s) to operate on: a name, or an IMAP LIST pattern
    /// ('Archive/*' crosses the hierarchy, 'Archive/%' does not).
    /// Repeatable and comma-separated. Default: "folder" from the config
    /// (INBOX), except for `count`, which defaults to every mailbox.
    #[clap(
        short = 'f',
        long = "folder",
        global = true,
        value_name = "NAME",
        value_delimiter = ','
    )]
    folder: Vec<String>,

    /// Every selectable mailbox of the account (shorthand for -f '*')
    #[clap(short = 'A', long = "all-folders", global = true)]
    all_folders: bool,

    /// Use the in-memory mock backend (no real server; for testing/demos).
    /// Development builds only: the released binary is built without it
    #[cfg(feature = "mock")]
    #[clap(long = "mock", global = true)]
    mock: bool,

    /// Enable debug output (show connection attempts, login failures, etc.)
    #[clap(short = 'd', long = "debug", global = true)]
    debug: bool,

    /// Output results as compact single-line JSON (for programmatic use)
    #[clap(short = 'j', long = "json", global = true)]
    json: bool,

    /// Maximum number of search results to fetch (overrides the config
    /// "max"; 0 = unlimited)
    #[clap(short = 'M', long = "max", global = true)]
    max: Option<usize>,

    /// Sort search/unread results: comma-separated criteria
    /// (uid, date, arrival, size, subject, from, to, cc), first = primary,
    /// '-' prefix = descending; e.g. "-date" or "subject,-size".
    /// Overrides the config "sort"; default is most recent first. Uses
    /// server-side UID SORT (RFC 5256) when advertised, else sorts
    /// client-side.
    #[clap(
        short = 'S',
        long = "sort",
        global = true,
        allow_hyphen_values = true,
        value_name = "SPEC"
    )]
    sort: Option<String>,

    /// Narrow what this run may change: readonly, organize,
    /// restructure or full.
    /// The config's "access-level" sets the ceiling; this can only lower
    /// it, never raise it
    #[clap(long = "access-level", global = true, value_name = "LEVEL")]
    access_level: Option<String>,

    /// Subcommand to execute
    #[clap(subcommand)]
    command: Command,
}

impl Args {
    /// Did this run ask for the mock backend? A build without it has no
    /// such flag, so the answer is no and `--mock` is rejected by clap
    /// as an unknown option rather than accepted and ignored.
    #[cfg(feature = "mock")]
    fn wants_mock(&self) -> bool {
        self.mock
    }

    #[cfg(not(feature = "mock"))]
    fn wants_mock(&self) -> bool {
        false
    }
}

/// Print an error (as `{"error": ...}` JSON when `json`) and exit non-zero.
fn fail(json: bool, message: &str) -> ! {
    if json {
        eprintln!("{}", serde_json::json!({ "error": message }));
    } else {
        eprintln!("Error: {}", message);
    }
    process::exit(1);
}

/// The folders this run works on: the `-f` values, plus `-A`.
fn folder_spec(args: &Args) -> FolderSpec {
    let mut patterns = args.folder.clone();
    if args.all_folders {
        patterns.push("*".to_string());
    }
    FolderSpec::new(patterns)
}

/// The messages a command works on.
#[derive(clap::Args, Clone)]
struct Sel {
    /// Message selection(s): 5, 1,4,7, 1-9, 9-, '*', last:20, Archive::1-5
    #[clap(value_name = "SELECTION")]
    selection: Vec<String>,
}

/// Split the arguments of `flag`/`tag` add|remove into selections and
/// names.
///
/// No separator is needed: the two cannot be confused, since a name
/// that would read as a selection is refused by `parse_flag_names`.
/// Selections come first and names follow — the shapes would allow any
/// order, but one order reads the same way every time, and a selection
/// after a name is far likelier to be a mistake than an intention.
///
/// That refusal is what makes this split well-defined: a bare number
/// can only be a UID here. The CLI never reaches the check itself —
/// the token has been taken as a selection by then — so the cost is
/// that a tag cannot be *named* `2024`, and the "no name given" error
/// below says so rather than leaving the user to guess.
fn split_args(args: &[String], json: bool) -> (Vec<Selection>, Vec<String>) {
    let split = args
        .iter()
        .position(|a| cli::select::parse_selection(a).is_err())
        .unwrap_or(args.len());
    let (sel_args, names) = args.split_at(split);
    let (sel_args, names): (Vec<String>, Vec<String>) = (sel_args.to_vec(), names.to_vec());
    if let Some(stray) = names
        .iter()
        .find(|a| cli::select::parse_selection(a).is_ok())
    {
        fail(
            json,
            &format!(
                "'{}' is a message selection, and selections come before names: \
                 write them all first",
                stray
            ),
        );
    }
    if sel_args.is_empty() {
        fail(
            json,
            "no message selection given: name messages (5, 1-9, Archive::3) \
             or a count of them (last:20, first:5)",
        );
    }
    if names.is_empty() {
        // Every argument read as a selection, so nothing is left to be
        // a name. "No name given" alone hides why: a name that reads
        // as a selection is not accepted as one, so `tag add 5 2024`
        // has no reading in which 2024 is a tag.
        fail(
            json,
            &format!(
                "no flag or tag name given: every argument reads as a message selection \
                 (the last one is '{}'). A name that reads as a selection -- a bare \
                 number, a range, '*', last:N -- is not accepted as a flag or tag name, \
                 so there is no way to spell one here",
                args.last().map(String::as_str).unwrap_or(""),
            ),
        );
    }
    match parse_selections(&sel_args) {
        Ok(s) => (s, names),
        Err(e) => fail(json, &format!("{:#}", e)),
    }
}

impl Sel {
    fn resolve(&self, json: bool) -> Vec<Selection> {
        if self.selection.is_empty() {
            fail(
                json,
                "no message selection given: name messages (5, 1-9, Archive::3) \
                 or a count of them (last:20, first:5)",
            );
        }
        match parse_selections(&self.selection) {
            Ok(s) => s,
            Err(e) => fail(json, &format!("{:#}", e)),
        }
    }
}

#[derive(clap::Subcommand)]
enum Command {
    /// List folders, or change the folder tree (create / rename /
    /// subscribe / unsubscribe — each needs access-level 'restructure')
    Folder {
        /// What to do with the folders
        #[clap(subcommand)]
        action: FolderAction,
    },
    /// What this run can do and what the server is: the access level
    /// in force, the hierarchy delimiter to build folder paths with,
    /// the special-use mailboxes, and which wire path each operation
    /// takes here
    Info,
    /// Search emails in the selected folders (IMAP SEARCH query: "ALL"
    /// for every message, "UNSEEN", 'HEADER FROM "foo"')
    Search {
        /// IMAP search query
        query: String,
    },
    /// Read email(s) by message selection
    #[clap(after_help = SELECTION_HELP)]
    Read {
        #[clap(flatten)]
        sel: Sel,
    },
    /// Show message counts / status of mailboxes (IMAP STATUS)
    #[clap(alias = "status")]
    Count,
    /// List the message UIDs of the selected folder(s)
    Uid,
    /// List the UIDs of every message in the thread containing the
    /// selected message(s) (server-side RFC 5256 THREAD when advertised,
    /// else client-side reconstruction from Message-ID / References)
    #[clap(after_help = SELECTION_HELP)]
    Thread {
        #[clap(flatten)]
        sel: Sel,
    },
    /// List unread emails of the selected folder(s)
    Unread,
    /// Move email(s) to another folder, named last, as `mv` does:
    /// `move 1-5 Archive` (needs access-level 'organize'; the folder
    /// must already exist)
    #[clap(after_help = SELECTION_HELP)]
    Move {
        #[clap(flatten)]
        sel: Sel,
    },
    /// List or save MIME parts of an email
    Part {
        /// What to do with the parts
        #[clap(subcommand)]
        action: PartsAction,
    },
    /// Enable/disable the IMAP-defined message flags (\Seen,
    /// \Answered, \Flagged, \Deleted, \Draft) via UID STORE.
    /// User-defined keywords are the `tag` command's
    Flag {
        /// What to do with the flags
        #[clap(subcommand)]
        action: FlagAction,
    },
    /// Add/remove custom keyword tags on emails (plain keywords, no
    /// system flags) via UID STORE
    Tag {
        /// What to do with the tags
        #[clap(subcommand)]
        action: TagAction,
    },
}

#[derive(clap::Subcommand)]
enum FolderAction {
    /// List mailboxes
    List {
        /// Show each mailbox's hierarchy delimiter and LIST attributes
        /// (\Sent, \Junk, \Noinferiors, ...) beside its name
        #[clap(short = 'l', long = "long")]
        long: bool,
    },
    /// Create a mailbox
    Create {
        /// Mailbox name, with the server's hierarchy delimiter
        /// (Archive/2026)
        name: String,
        /// Declare an RFC 6154 special use at creation — the only
        /// moment IMAP allows it: archive, drafts, junk, sent, trash,
        /// all, flagged. Needs CREATE-SPECIAL-USE
        #[clap(long = "use", value_name = "ATTR")]
        use_attr: Option<String>,
        /// Take the --use attribute in the form a `folder` listing
        /// prints it (`\Archive`) rather than as a bare word
        #[clap(long = "wire")]
        wire: bool,
    },
    /// Rename a mailbox (INBOX is refused: renaming it empties it)
    Rename {
        /// The mailbox to rename
        from: String,
        /// Its new name
        to: String,
    },
    /// Subscribe to a mailbox
    Subscribe {
        /// The mailbox to subscribe to
        name: String,
    },
    /// Unsubscribe from a mailbox
    Unsubscribe {
        /// The mailbox to unsubscribe from
        name: String,
    },
}

#[derive(clap::Subcommand)]
enum FlagAction {
    /// List the flags (system flags and keyword tags) of the selected
    /// email(s)
    #[clap(after_help = SELECTION_HELP)]
    List {
        /// Print the atoms as the server sent them (`\Seen`), rather
        /// than in the form `flag add` takes back
        #[clap(long = "wire")]
        wire: bool,
        #[clap(flatten)]
        sel: Sel,
    },
    /// Enable the given flags on the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    Add {
        /// Take the flag names in the form a listing prints them
        /// (`\Seen`) rather than as bare words
        #[clap(long = "wire")]
        wire: bool,
        /// Message selection(s) and the flags to add, in any order:
        /// seen, answered, flagged, deleted, draft
        #[clap(value_name = "SELECTION|FLAG", required = true)]
        args: Vec<String>,
    },
    /// Disable the given flags on the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    Remove {
        /// Take the flag names in the form a listing prints them
        #[clap(long = "wire")]
        wire: bool,
        /// Message selection(s) and the flags to remove
        #[clap(value_name = "SELECTION|FLAG", required = true)]
        args: Vec<String>,
    },
}

#[derive(clap::Subcommand)]
enum TagAction {
    /// List the keywords with an agreed meaning: the IANA registry,
    /// and the conventions no registry covers (no server needed)
    Known,
    /// Mark the selected email(s) junk: set every spelling of it the
    /// mailbox keeps, and clear every spelling of the opposite
    #[clap(after_help = SELECTION_HELP)]
    Junk {
        #[clap(flatten)]
        sel: Sel,
    },
    /// Mark the selected email(s) not junk (the same, the other way)
    #[clap(name = "notjunk", after_help = SELECTION_HELP)]
    NotJunk {
        #[clap(flatten)]
        sel: Sel,
    },
    /// List the custom keyword tags of the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    List {
        /// Print the atoms as the server sent them, rather than in the
        /// form `tag add` takes back
        #[clap(long = "wire")]
        wire: bool,
        #[clap(flatten)]
        sel: Sel,
    },
    /// Add the given tags to the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    Add {
        /// Send the tag names verbatim, as the atoms they already are —
        /// for a key copied out of a listing
        #[clap(long = "wire")]
        wire: bool,
        /// Message selection(s) and the keywords to add, in any order
        /// (`invoice`, `$Important`; a non-ASCII tag is encoded to
        /// modified UTF-7, so régie becomes r&AOk-gie)
        #[clap(value_name = "SELECTION|TAG", required = true)]
        args: Vec<String>,
    },
    /// Remove the given tags from the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    Remove {
        /// Send the tag names verbatim (see `tag add --wire`)
        #[clap(long = "wire")]
        wire: bool,
        /// Message selection(s) and the keywords to remove
        #[clap(value_name = "SELECTION|TAG", required = true)]
        args: Vec<String>,
    },
}

#[derive(clap::Subcommand)]
enum PartsAction {
    /// List the MIME parts of the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    List {
        #[clap(flatten)]
        sel: Sel,
    },
    /// Save one part of one message to a file, or every part with --all
    #[clap(after_help = SELECTION_HELP)]
    Save {
        /// Message selection naming exactly one message (5, Archive::5)
        #[clap(value_name = "SELECTION")]
        selection: String,
        /// Part number (as listed by `part list`); omitted with --all
        #[clap(required_unless_present = "all")]
        part: Option<u32>,
        /// Save every part of the message instead of one
        #[clap(long = "all", conflicts_with = "part")]
        all: bool,
        /// Destination file (default: the part's filename in the current
        /// directory); with --all, a directory that must already exist
        /// (default: the current directory); '-' writes one part raw to
        /// stdout (refused with --all or -j/--json)
        #[clap(short = 'o', long = "out")]
        out: Option<std::path::PathBuf>,
    },
}

fn main() {
    let args = Args::parse();

    // `tag known` reads a table compiled into the binary: no account,
    // no server, and so no config either. Answering it before the
    // config is loaded is what keeps that true -- otherwise the one
    // command that needs nothing is the one refusing to run for want
    // of a file it never reads. (While --mock existed this was hidden:
    // it was the documented way to run this command, and it happened
    // to make a missing config non-fatal.)
    if let Command::Tag {
        action: TagAction::Known,
    } = &args.command
    {
        if let Err(e) = cli::tags_known(args.json) {
            fail(args.json, &format!("{:#}", e));
        }
        return;
    }

    // `info` reports which file the rest of the run read; under --mock
    // there may well be none, and saying so beats naming a path that
    // was never opened.
    let mut config_file: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut config = if args.wants_mock() {
        // Mock mode needs no server, so a missing config is fine.
        match config::load_config(args.config_file.as_deref(), args.profile.as_deref()) {
            Ok(loaded) => {
                config_file = Some(loaded.path);
                profile = loaded.profile;
                loaded.config
            }
            Err(_) => config::Config::default(),
        }
    } else {
        match config::load_config(args.config_file.as_deref(), args.profile.as_deref()) {
            Ok(loaded) => {
                config_file = Some(loaded.path);
                profile = loaded.profile;
                loaded.config
            }
            // {:#} so the cause travels with the context: the outer
            // frame names the file, the inner one says what is wrong
            // with it, and only the pair is actionable.
            Err(e) => fail(args.json, &format!("Configuration error: {:#}", e)),
        }
    };
    if args.wants_mock() {
        config.mock = true;
    }
    if let Some(max) = args.max {
        config.max = max;
    }
    if let Some(spec) = &args.sort {
        if let Err(e) = imap::parse_sort(spec) {
            fail(args.json, &format!("invalid --sort spec: {}", e));
        }
        config.sort = Some(spec.clone());
    }

    let json = args.json;
    let configured_access = config.access;
    if let Some(name) = &args.access_level {
        match config::AccessLevel::parse(name) {
            Ok(level) if level <= config.access => config.access = level,
            Ok(level) => fail(
                json,
                &format!(
                    "--access-level {} is wider than the config's '{}': the command \
                     line can only narrow what this tool may change",
                    level.as_str(),
                    config.access.as_str()
                ),
            ),
            Err(e) => fail(json, &format!("{}", e)),
        }
    }
    let debug = args.debug;

    let result = match &args.command {
        Command::Folder { action } => match action {
            FolderAction::List { long } => cli::list_folders(&config, json, debug, *long),
            FolderAction::Create {
                name,
                use_attr,
                wire,
            } => cli::folder_create(&config, name, use_attr.as_deref(), *wire, json, debug),
            FolderAction::Rename { from, to } => {
                cli::folder_rename(&config, from, to, json, debug)
            }
            FolderAction::Subscribe { name } => {
                cli::folder_subscribe(&config, name, true, json, debug)
            }
            FolderAction::Unsubscribe { name } => {
                cli::folder_subscribe(&config, name, false, json, debug)
            }
        },
        Command::Info => cli::info(
            &config,
            config_file.as_deref(),
            profile.as_deref(),
            configured_access,
            json,
            debug,
        ),
        Command::Search { query } => {
            cli::search_emails(&config, query, &folder_spec(&args), json, debug)
        }
        Command::Read { sel } => {
            let selections = sel.resolve(json);
            cli::read_emails(
                &config,
                &folder_spec(&args),
                &selections,
                json,
                debug,
            )
        }
        Command::Count => cli::mailbox_counts(&config, &folder_spec(&args), json, debug),
        Command::Uid => cli::folder_uids(&config, &folder_spec(&args), json, debug),
        Command::Thread { sel } => {
            let selections = sel.resolve(json);
            cli::thread_uids(
                &config,
                &folder_spec(&args),
                &selections,
                json,
                debug,
            )
        }
        Command::Unread => cli::unread(&config, &folder_spec(&args), json, debug),
        Command::Move { sel } => {
            // The last argument is the folder, as `mv` has it. A
            // forgotten one needs no guard: `move 1-5` leaves nothing
            // to select and says so, and `move 1 2` is refused by the
            // backend unless a mailbox really is called 2.
            let Some((to, rest)) = sel.selection.split_last() else {
                fail(
                    json,
                    "move needs the folder to file into as its last argument \
                     (move 1-5 Archive)",
                )
            };
            let selections = Sel {
                selection: rest.to_vec(),
            }
            .resolve(json);
            cli::move_messages(
                &config,
                &folder_spec(&args),
                &selections,
                to,
                json,
                debug,
            )
        }
        Command::Part { action } => match action {
            PartsAction::List { sel } => {
                let selections = sel.resolve(json);
                cli::parts_list(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    json,
                    debug,
                )
            }
            PartsAction::Save {
                selection,
                part,
                all,
                out,
            } => {
                let one = match parse_selections(std::slice::from_ref(selection)) {
                    Ok(s) => s,
                    Err(e) => fail(json, &format!("{:#}", e)),
                };
                cli::parts_save(
                    &config,
                    &folder_spec(&args),
                    &one[0],
                    *part,
                    *all,
                    out.clone(),
                    json,
                    debug,
                )
            }
        },
        Command::Flag { action } => match action {
            FlagAction::List { sel, wire } => {
                let selections = sel.resolve(json);
                cli::flag_list(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    false,
                    *wire,
                    json,
                    debug,
                )
            }
            FlagAction::Add { args: argv, wire } => {
                let (selections, names) = split_args(argv, json);
                cli::change_flags(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    &names,
                    true,
                    *wire,
                    true,
                    json,
                    debug,
                )
            }
            FlagAction::Remove { args: argv, wire } => {
                let (selections, names) = split_args(argv, json);
                cli::change_flags(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    &names,
                    true,
                    *wire,
                    false,
                    json,
                    debug,
                )
            }
        },
        Command::Tag { action } => match action {
            TagAction::Known => cli::tags_known(json),
            TagAction::Junk { sel } => {
                let selections = sel.resolve(json);
                cli::set_junk(&config, &folder_spec(&args), &selections, true, json, debug)
            }
            TagAction::NotJunk { sel } => {
                let selections = sel.resolve(json);
                cli::set_junk(&config, &folder_spec(&args), &selections, false, json, debug)
            }
            TagAction::List { sel, wire } => {
                let selections = sel.resolve(json);
                cli::flag_list(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    true,
                    *wire,
                    json,
                    debug,
                )
            }
            TagAction::Add { args: argv, wire } => {
                let (selections, names) = split_args(argv, json);
                cli::change_flags(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    &names,
                    false,
                    *wire,
                    true,
                    json,
                    debug,
                )
            }
            TagAction::Remove { args: argv, wire } => {
                let (selections, names) = split_args(argv, json);
                cli::change_flags(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    &names,
                    false,
                    *wire,
                    false,
                    json,
                    debug,
                )
            }
        },
    };

    if let Err(e) = result {
        fail(json, &format!("{:#}", e));
    }
}
