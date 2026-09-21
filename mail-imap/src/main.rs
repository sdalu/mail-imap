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

    /// Folder to operate on (default: from config or INBOX).
    /// The field is not named `folder` because that collides with the
    /// positional `folder` argument of the `move` subcommand in clap.
    #[clap(short = 'f', long = "folder", global = true)]
    default_folder: Option<String>,

    /// Use the in-memory mock backend (no real server; for testing/demos)
    #[clap(long = "mock", global = true)]
    mock: bool,

    /// Output results as compact single-line JSON (for programmatic use)
    #[clap(short = 'j', long = "json", global = true)]
    json: bool,

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

#[derive(clap::Subcommand)]
enum Command {
    /// List folders
    Folders,
    /// Search emails (IMAP SEARCH query, e.g. "UNSEEN" or 'HEADER FROM "foo"')
    Search {
        /// IMAP search query
        query: String,
    },
    /// Read an email by UID
    Read {
        /// Email UID
        id: u32,
    },
    /// Move an email by UID to another folder
    Move {
        /// Email UID
        id: u32,
        /// Target folder
        folder: String,
    },
    /// Add or remove keyword tags on an email
    Tag {
        /// Email UID
        id: u32,
        /// What to do with the tags
        #[clap(subcommand)]
        action: TagAction,
    },
    /// Add or remove standard flags on an email (\Seen, \Answered, \Flagged).
    /// \Deleted, \Draft and \Recent are explicitly not supported.
    Flags {
        /// Email UID
        id: u32,
        /// What to do with the flags
        #[clap(subcommand)]
        action: FlagsAction,
    },
}

#[derive(clap::Subcommand)]
enum TagAction {
    /// Add tags (e.g. `tag 123 add important reviewed`)
    Add {
        /// Tags to add
        tags: Vec<String>,
    },
    /// Remove tags (e.g. `tag 123 remove reviewed`)
    Remove {
        /// Tags to remove
        tags: Vec<String>,
    },
}

#[derive(clap::Subcommand)]
enum FlagsAction {
    /// Add flags (e.g. `flags 123 add seen answered`)
    Add {
        /// Flags to add (seen, answered, flagged)
        flags: Vec<String>,
    },
    /// Remove flags (e.g. `flags 123 remove flagged`)
    Remove {
        /// Flags to remove (seen, answered, flagged)
        flags: Vec<String>,
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

    let json = args.json;
    let result = match &args.command {
        Command::Folders => cli::list_folders(&config, json),
        Command::Search { query } => cli::search_emails(&config, query, json),
        Command::Read { id } => cli::read_email(&config, *id, json),
        Command::Move { id, folder } => cli::move_email(&config, *id, folder, json),
        Command::Tag { id, action } => match action {
            TagAction::Add { tags } => cli::set_tags(&config, *id, &tags, &[], json),
            TagAction::Remove { tags } => cli::set_tags(&config, *id, &[], &tags, json),
        },
        Command::Flags { id, action } => match action {
            FlagsAction::Add { flags } => cli::set_flags(&config, *id, &flags, &[], json),
            FlagsAction::Remove { flags } => cli::set_flags(&config, *id, &[], &flags, json),
        },
    };

    if let Err(e) = result {
        fail(json, &e.to_string());
    }
}
