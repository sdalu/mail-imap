use clap::Parser;
use std::process;

mod cli;
mod imap;
mod config;

#[derive(Parser)]
#[clap(name = "mail-imap", version = "0.1.0", author = "AI Assistant")]
struct Args {
    /// Configuration file path
    #[clap(short = 'c', long = "config")]
    config_file: Option<String>,
    
    /// Subcommand to execute
    #[clap(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// List folders
    Folders,
    /// Search emails
    Search {
        /// Search query
        query: String,
    },
    /// Read an email
    Read {
        /// Email ID
        id: u32,
    },
    /// Move an email
    Move {
        /// Email ID
        id: u32,
        /// Target folder
        folder: String,
    },
    /// Tag an email
    Tag {
        /// Email ID
        id: u32,
        /// Tags to add
        tags: Vec<String>,
    },
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    
    // Load configuration
    let config = match config::load_config(args.config_file.as_deref()) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Configuration error: {}", e);
            process::exit(1);
        }
    };
    
    // Execute command
    match &args.command {
        Command::Folders => {
            if let Err(e) = cli::list_folders(&config).await {
                eprintln!("Error listing folders: {}", e);
                process::exit(1);
            }
        }
        Command::Search { query } => {
            if let Err(e) = cli::search_emails(&config, query).await {
                eprintln!("Error searching emails: {}", e);
                process::exit(1);
            }
        }
        Command::Read { id } => {
            if let Err(e) = cli::read_email(&config, *id).await {
                eprintln!("Error reading email: {}", e);
                process::exit(1);
            }
        }
        Command::Move { id, folder } => {
            if let Err(e) = cli::move_email(&config, *id, folder).await {
                eprintln!("Error moving email: {}", e);
                process::exit(1);
            }
        }
        Command::Tag { id, tags } => {
            if let Err(e) = cli::tag_email(&config, *id, tags).await {
                eprintln!("Error tagging email: {}", e);
                process::exit(1);
            }
        }
    }
}