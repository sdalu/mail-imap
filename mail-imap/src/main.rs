use clap::Parser;
use std::process;

mod cli;
mod config;
mod imap;

#[derive(Parser)]
#[clap(name = "mail-imap", version = "0.1.0", author = "AI Assistant")]
struct Args {
    /// Configuration file path (or set MAIL_IMAP_CONFIG)
    #[clap(short = 'c', long = "config")]
    config_file: Option<String>,

    /// Folder to operate on (default: from config or INBOX)
    #[clap(short = 'f', long = "folder", global = true)]
    default_folder: Option<String>,

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

/// UID selection: a single UID or a comma-separated list (e.g. `1,4,7`).
/// Ranges are not supported.
type UidSpec = String;

/// Use the explicitly requested folders (deduplicated, order preserved),
/// falling back to the single configured folder.
fn resolve_folders(explicit: &[String], config: &config::Config) -> Vec<String> {
    if explicit.is_empty() {
        return vec![config.folder.clone()];
    }
    let mut out = Vec::new();
    for folder in explicit {
        if !out.contains(folder) {
            out.push(folder.clone());
        }
    }
    out
}

#[derive(clap::Subcommand)]
enum Command {
    /// List folders
    Folders,
    /// Search emails in one or more folders (IMAP SEARCH query: "ALL" for
    /// every message, "UNSEEN", 'HEADER FROM "foo"')
    Search {
        /// IMAP search query
        query: String,
        /// Folder(s) to search, comma-separated or repeated
        /// (default: the -f/config folder)
        #[clap(value_name = "FOLDER", value_delimiter = ',')]
        folders: Vec<String>,
    },
    /// Read email(s) by UID
    Read {
        /// UID selection: `5` or `1,4,7` (no ranges)
        uids: UidSpec,
    },
    /// Show message counts / status of mailboxes (IMAP STATUS)
    #[clap(alias = "status")]
    Count {
        /// Mailbox to show (default: all selectable mailboxes)
        folder: Option<String>,
    },
    /// List the message UIDs of the folder
    Ids,
    /// List unread emails of one or more folders
    Unread {
        /// Folder(s) to check, comma-separated or repeated
        /// (default: the -f/config folder)
        #[clap(value_name = "FOLDER", value_delimiter = ',')]
        folders: Vec<String>,
    },
    /// List or save MIME parts of an email
    Parts {
        /// What to do with the parts
        #[clap(subcommand)]
        action: PartsAction,
    },
}

#[derive(clap::Subcommand)]
enum PartsAction {
    /// List the MIME parts of the given email(s)
    List {
        /// UID selection: `5` or `1,4,7` (no ranges)
        uids: UidSpec,
    },
    /// Save one part to a file
    Save {
        /// Email UID
        uid: u32,
        /// Part number (as listed by `parts list`)
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
        match config::load_config(args.config_file.as_deref()) {
            Ok(cfg) => cfg,
            Err(_) => config::Config::default(),
        }
    } else {
        match config::load_config(args.config_file.as_deref()) {
            Ok(cfg) => cfg,
            Err(e) => fail(args.json, &format!("Configuration error: {}", e)),
        }
    };
    if args.mock {
        config.mock = true;
    }
    if let Some(folder) = &args.default_folder {
        config.folder = folder.clone();
    }
    if let Some(max) = args.max {
        config.max = max;
    }

    let json = args.json;
    let debug = args.debug;
    let result = match &args.command {
        Command::Folders => cli::list_folders(&config, json, debug),
        Command::Search { query, folders } => cli::search_emails(
            &config,
            query,
            resolve_folders(folders, &config),
            json,
            debug,
        ),
        Command::Read { uids } => cli::read_emails(&config, uids, json, debug),
        Command::Count { folder } => cli::mailbox_counts(&config, folder.as_deref(), json, debug),
        Command::Ids => cli::folder_uids(&config, json, debug),
        Command::Unread { folders } => {
            cli::unread(&config, resolve_folders(folders, &config), json, debug)
        }
        Command::Parts { action } => match action {
            PartsAction::List { uids } => cli::parts_list(&config, uids, json, debug),
            PartsAction::Save { uid, part, out } => {
                cli::parts_save(&config, *uid, *part, out.clone(), json, debug)
            }
        },
    };

    if let Err(e) = result {
        fail(json, &e.to_string());
    }
}
