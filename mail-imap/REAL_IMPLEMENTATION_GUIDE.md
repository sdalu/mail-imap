# Real IMAP Implementation Guide

This guide explains how to properly implement real IMAP functionality for the mail-imap project.

## Current State

The project currently has a mock implementation that demonstrates the API structure but doesn't connect to real IMAP servers. The actual implementation requires proper integration with the `imap` crate.

## Required Dependencies

Add these to your `Cargo.toml`:

```toml
[dependencies]
imap = "2.4"
tokio = { version = "1.0", features = ["full"] }
tokio-native-tls = "0.3"
native-tls = "0.2"
```

## How to Implement Real IMAP Functionality

The key challenge is that the `imap` crate has a complex API that requires careful handling. Here's the recommended approach:

### 1. Connection Setup

```rust
use imap::Client;
use std::net::TcpStream;
use tokio_native_tls::TlsConnector;

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
}
```

### 2. Core Operations

Each operation needs to properly call the IMAP commands:

- `list_folders()` → `client.list()` command
- `search_emails()` → `client.search()` command  
- `get_email()` → `client.fetch()` command
- `move_email()` → `client.copy()`, `client.store()`, and `client.expunge()` sequence
- `tag_email()` → `client.store()` command

## Important Considerations

1. **API Compatibility**: Different versions of the `imap` crate have different APIs
2. **Error Handling**: The IMAP protocol has many potential failure points
3. **Async/Await**: All operations must be properly async
4. **Memory Management**: Large email bodies need careful handling

## Testing Approach

1. Create a test configuration with a real IMAP server
2. Test each operation individually
3. Handle authentication failures gracefully
4. Test connection timeouts and network issues

## Example Usage

```bash
# List folders
cargo run -- --config your-config.json folders

# Search emails
cargo run -- --config your-config.json search "subject:important"

# Read email
cargo run -- --config your-config.json read 123
```

Note: This implementation requires careful attention to the specific API version of the imap crate being used in your environment.