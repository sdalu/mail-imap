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
    folder: Option<String>,

    /// Use the in-memory mock backend (no real server; for testing/demos)
    #[clap(long = "mock", global = true)]
    mock: bool,

    /// Subcommand to execute
    #[clap(subcommand)]
    command: Command,
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
    /// Tag an email by UID (adds keyword flags)
    Tag {
        /// Email UID
        id: u32,
        /// Tags to add
        tags: Vec<String>,
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
            Err(e) => {
                eprintln!("Configuration error: {}", e);
                process::exit(1);
            }
        }
    };
    if args.mock {
        config.mock = true;
    }
    if let Some(folder) = &args.folder {
        config.folder = folder.clone();
    }

    let result = match &args.command {
        Command::Folders => cli::list_folders(&config),
        Command::Search { query } => cli::search_emails(&config, query),
        Command::Read { id } => cli::read_email(&config, *id),
        Command::Move { id, folder } => cli::move_email(&config, *id, folder),
        Command::Tag { id, tags } => cli::tag_email(&config, *id, tags),
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}
