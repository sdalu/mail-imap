# IMAP Implementation Guide

This document explains how to implement real IMAP functionality for the mail-imap project.

## Current State

The project currently has a mock implementation in `src/imap/mod.rs` that demonstrates the structure and API but doesn't connect to real IMAP servers.

## Real Implementation Steps

To implement real IMAP functionality, follow these steps:

### 1. Dependencies

Make sure you have the following in your `Cargo.toml`:

```toml
[dependencies]
imap = "2.4"
tokio = { version = "1.0", features = ["full"] }
tokio-native-tls = "0.3"
native-tls = "0.2"
```

### 2. Connection Logic

Replace the mock implementation with real connection logic:

```rust
use crate::config::Config;
use anyhow::Result;
use imap::Client;
use std::net::TcpStream;
use tokio_native_tls::TlsConnector;

pub struct ImapClient {
    client: Client<TcpStream>,
}

impl ImapClient {
    pub async fn connect(config: &Config) -> Result<Self> {
        let addr = format!("{}:{}", config.server, config.port);
        let stream = TcpStream::connect(addr).await?;
        
        let client = if config.ssl {
            let connector = TlsConnector::new()?;
            let client = Client::new(stream);
            let client = client.starttls(connector).await?;
            client
        } else {
            Client::new(stream)
        };
        
        client.login(&config.username, &config.password).await?;
        
        Ok(ImapClient { client })
    }
    
    // ... rest of the methods would use the real IMAP API
}
```

### 3. Key IMAP Operations

The following operations need to be implemented using the imap crate:

- `list_folders()` → IMAP LIST command
- `search_emails()` → IMAP SEARCH command  
- `get_email()` → IMAP FETCH command
- `move_email()` → IMAP COPY + STORE + EXPUNGE sequence
- `tag_email()` → IMAP STORE command

### 4. Error Handling

Use proper error handling with the `anyhow` crate for graceful error management.

## Testing

After implementing the real functionality, test with:

```bash
# Build the project
cargo build

# Run tests
cargo test

# Test with a real configuration file
cargo run -- --config your-config.json folders
```

## Sample Configuration

Create a configuration file like `real-config.json`:

```json
{
    "server": "imap.gmail.com",
    "port": 993,
    "username": "your-email@gmail.com",
    "password": "your-app-password",
    "ssl": true
}
```

Note: For Gmail, you'll need to use an App Password instead of your regular password.