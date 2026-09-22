use clap::Parser;
use std::process;

mod cli;
mod config;
mod imap;

use cli::select::{parse_selections, Selection, UidItem};
use cli::FolderSpec;

/// Text shown under every command that takes a message selection.
const SELECTION_HELP: &str = "\
A message selection is [FOLDER::]UIDS — 5, 1,4,7, 1-9 (a UID range), '*' \
(every message), 9- or 9-* (from 9 to the end), Archive::1-5. '::' binds \
the folder, so a single ':' is free to be IMAP's own range operator \
(INBOX::1:5 works). A range is an \
interval of the UID space, so it may match fewer messages than its span; \
for a COUNT of messages use --last N / --first N instead, which no range \
can express. Without a folder the selection means the -f folder (or \
\"folder\" from the config).";

#[derive(Parser)]
#[clap(name = "mail-imap", version = env!("CARGO_PKG_VERSION"), author = "AI Assistant")]
struct Args {
    /// Configuration file path (or set MAIL_IMAP_CONFIG)
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

    /// Use the in-memory mock backend (no real server; for testing/demos)
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

    /// Narrow what this run may change: readonly, organize or full.
    /// The config's "access-level" sets the ceiling; this can only lower
    /// it, never raise it
    #[clap(long = "access-level", global = true, value_name = "LEVEL")]
    access_level: Option<String>,

    /// Subcommand to execute
    #[clap(subcommand)]
    command: Command,
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

/// The messages a command works on: explicit selections, or a count of
/// the newest / oldest messages of each selected folder. A count is a
/// flag rather than a selection token because `last:20` would collide
/// with the `FOLDER:UIDS` split.
#[derive(clap::Args)]
struct Sel {
    /// Message selection(s): 5, 1,4,7, 1-9, 9-, '*', Archive::1-5
    #[clap(value_name = "SELECTION")]
    selection: Vec<String>,

    /// The N newest messages (the N highest UIDs) of each selected
    /// folder, instead of a selection
    #[clap(
        short = 'L',
        long = "last",
        value_name = "N",
        conflicts_with = "first",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    last: Option<u32>,

    /// The N oldest messages (the N lowest UIDs) of each selected folder
    #[clap(
        long = "first",
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    first: Option<u32>,
}

impl Sel {
    /// The parsed selections and the recency count; exactly one of the
    /// two is ever populated.
    fn resolve(&self, json: bool) -> (Vec<Selection>, Option<UidItem>) {
        let count = self.last.map(UidItem::Last).or(self.first.map(UidItem::First));
        if let Some(item) = count {
            if !self.selection.is_empty() {
                fail(
                    json,
                    &format!(
                        "--last/--first name a count of messages, so they cannot be \
                         combined with the message selection(s) '{}'",
                        self.selection.join(" ")
                    ),
                );
            }
            return (Vec::new(), Some(item));
        }
        if self.selection.is_empty() {
            fail(
                json,
                "no message selection given: name messages (5, 1-9, Archive::3) \
                 or ask for a count with --last N / --first N",
            );
        }
        match parse_selections(&self.selection) {
            Ok(s) => (s, None),
            Err(e) => fail(json, &format!("{:#}", e)),
        }
    }
}

#[derive(clap::Subcommand)]
enum Command {
    /// List folders, or change the folder tree (create / rename /
    /// subscribe — each needs access-level 'restructure')
    Folder {
        #[clap(subcommand)]
        action: Option<FolderAction>,
    },
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
    /// Create a mailbox
    Create {
        /// Mailbox name, with the server's hierarchy delimiter
        /// (Archive/2026)
        name: String,
        /// Declare an RFC 6154 special use at creation — the only
        /// moment IMAP allows it: \Archive, \Drafts, \Junk, \Sent,
        /// \Trash, \All, \Flagged. Needs CREATE-SPECIAL-USE
        #[clap(long = "use", value_name = "ATTR")]
        use_attr: Option<String>,
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
        #[clap(flatten)]
        sel: Sel,
    },
    /// Enable the given flags on the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    Add {
        #[clap(flatten)]
        sel: Sel,
        /// Flags to add (after `--`): `\Seen`, `\Answered`, `\Flagged`,
        /// `\Deleted` or `\Draft`
        #[clap(value_name = "FLAG", required = true, last = true)]
        flags: Vec<String>,
    },
    /// Disable the given flags on the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    Remove {
        #[clap(flatten)]
        sel: Sel,
        /// Flags to remove (after `--`, same forms as `flag add`)
        #[clap(value_name = "FLAG", required = true, last = true)]
        flags: Vec<String>,
    },
}

#[derive(clap::Subcommand)]
enum TagAction {
    /// List the keywords with an agreed meaning: the IANA registry,
    /// and the conventions no registry covers (no server needed)
    Known,
    /// List the custom keyword tags of the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    List {
        #[clap(flatten)]
        sel: Sel,
    },
    /// Add the given tags to the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    Add {
        #[clap(flatten)]
        sel: Sel,
        /// Send the tag names verbatim, as the atoms they already are —
        /// for a key copied out of a listing
        #[clap(long = "wire")]
        wire: bool,
        /// Tags (after `--`): custom IMAP keywords, e.g. `invoice`,
        /// `$Important`. A non-ASCII tag is encoded to modified UTF-7
        /// (régie becomes r&AOk-gie)
        #[clap(value_name = "TAG", required = true, last = true)]
        tags: Vec<String>,
    },
    /// Remove the given tags from the selected email(s)
    #[clap(after_help = SELECTION_HELP)]
    Remove {
        #[clap(flatten)]
        sel: Sel,
        /// Send the tag names verbatim (see `tag add --wire`)
        #[clap(long = "wire")]
        wire: bool,
        /// Tags to remove (after `--`, same forms as `tag add`)
        #[clap(value_name = "TAG", required = true, last = true)]
        tags: Vec<String>,
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
    /// Save one part of one message to a file
    #[clap(after_help = SELECTION_HELP)]
    Save {
        /// Message selection naming exactly one message (5, Archive::5)
        #[clap(value_name = "SELECTION")]
        selection: String,
        /// Part number (as listed by `part list`)
        part: u32,
        /// Destination file (default: the part's filename in the current directory)
        #[clap(short = 'o', long = "out")]
        out: Option<std::path::PathBuf>,
    },
}

fn main() {
    let args = Args::parse();

    let mut config = if args.mock {
        // Mock mode needs no server, so a missing config is fine.
        config::load_config(args.config_file.as_deref()).unwrap_or_default()
    } else {
        match config::load_config(args.config_file.as_deref()) {
            Ok(cfg) => cfg,
            Err(e) => fail(args.json, &format!("Configuration error: {}", e)),
        }
    };
    if args.mock {
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
            None => cli::list_folders(&config, json, debug),
            Some(FolderAction::Create { name, use_attr }) => {
                cli::folder_create(&config, name, use_attr.as_deref(), json, debug)
            }
            Some(FolderAction::Rename { from, to }) => {
                cli::folder_rename(&config, from, to, json, debug)
            }
            Some(FolderAction::Subscribe { name }) => {
                cli::folder_subscribe(&config, name, true, json, debug)
            }
            Some(FolderAction::Unsubscribe { name }) => {
                cli::folder_subscribe(&config, name, false, json, debug)
            }
        },
        Command::Search { query } => {
            cli::search_emails(&config, query, &folder_spec(&args), json, debug)
        }
        Command::Read { sel } => {
            let (selections, recency) = sel.resolve(json);
            cli::read_emails(
                &config,
                &folder_spec(&args),
                &selections,
                recency,
                json,
                debug,
            )
        }
        Command::Count => cli::mailbox_counts(&config, &folder_spec(&args), json, debug),
        Command::Uid => cli::folder_uids(&config, &folder_spec(&args), json, debug),
        Command::Thread { sel } => {
            let (selections, recency) = sel.resolve(json);
            cli::thread_uids(
                &config,
                &folder_spec(&args),
                &selections,
                recency,
                json,
                debug,
            )
        }
        Command::Unread => cli::unread(&config, &folder_spec(&args), json, debug),
        Command::Part { action } => match action {
            PartsAction::List { sel } => {
                let (selections, recency) = sel.resolve(json);
                cli::parts_list(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    recency,
                    json,
                    debug,
                )
            }
            PartsAction::Save {
                selection,
                part,
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
                    out.clone(),
                    json,
                    debug,
                )
            }
        },
        Command::Flag { action } => match action {
            FlagAction::List { sel } => {
                let (selections, recency) = sel.resolve(json);
                cli::flag_list(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    recency,
                    false,
                    json,
                    debug,
                )
            }
            FlagAction::Add { sel, flags } => {
                let (selections, recency) = sel.resolve(json);
                cli::change_flags(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    recency,
                    flags,
                    true,
                    false,
                    true,
                    json,
                    debug,
                )
            }
            FlagAction::Remove { sel, flags } => {
                let (selections, recency) = sel.resolve(json);
                cli::change_flags(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    recency,
                    flags,
                    true,
                    false,
                    false,
                    json,
                    debug,
                )
            }
        },
        Command::Tag { action } => match action {
            TagAction::Known => cli::tags_known(json),
            TagAction::List { sel } => {
                let (selections, recency) = sel.resolve(json);
                cli::flag_list(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    recency,
                    true,
                    json,
                    debug,
                )
            }
            TagAction::Add { sel, tags, wire } => {
                let (selections, recency) = sel.resolve(json);
                cli::change_flags(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    recency,
                    tags,
                    false,
                    *wire,
                    true,
                    json,
                    debug,
                )
            }
            TagAction::Remove { sel, tags, wire } => {
                let (selections, recency) = sel.resolve(json);
                cli::change_flags(
                    &config,
                    &folder_spec(&args),
                    &selections,
                    recency,
                    tags,
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
