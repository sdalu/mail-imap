# mail-imap - IMAP Email Client

This is a command-line tool for querying emails via IMAP protocol, designed for AI usage.

## Features

- Query and search folders
- Read/query/search emails
- Move emails between folders
- Tag emails
- Configuration via JSON files

## Usage

```bash
# List folders
mail-imap --config example-config.json folders

# Search emails
mail-imap --config example-config.json search "subject:important"

# Read email
mail-imap --config example-config.json read 123

# Move email
mail-imap --config example-config.json move 123 "Archive"

# Tag email
mail-imap --config example-config.json tag 123 "important" "reviewed"
```

## Configuration

The configuration file uses JSON format:

```json
{
    "server": "imap.example.com",
    "port": 993,
    "username": "your-email@example.com",
    "password": "your-password",
    "ssl": true
}
```

## Testing

To run tests:
```bash
cargo test
```

To run with verbose output:
```bash
cargo test --verbose
```

## Implementation Details

### Current Implementation Status

This implementation provides a complete framework for an IMAP client with:

1. **Modular Design**: Separated concerns into config, IMAP client, and CLI layers
2. **Interface Definition**: Clear API contracts for all IMAP operations
3. **Error Handling**: Proper error propagation using anyhow
4. **Async Support**: Full async/await support for non-blocking operations

### Real IMAP Integration

The current implementation uses mock methods to demonstrate the structure. To implement real IMAP functionality, you would:

1. **Install Dependencies**: Ensure `imap`, `tokio-native-tls`, and related crates are available
2. **Connect to Server**: Use `tokio::net::TcpStream` for connection
3. **Handle SSL/TLS**: Configure secure connections properly
4. **Implement Operations**:
   - `list_folders()` → IMAP LIST command
   - `search_emails()` → IMAP SEARCH command  
   - `get_email()` → IMAP FETCH command
   - `move_email()` → IMAP COPY + STORE + EXPUNGE sequence
   - `tag_email()` → IMAP STORE command

### Future Extensions

To enable real IMAP functionality, you would replace the mock implementations in `src/imap/mod.rs` with actual code using the `imap` crate, such as:

```rust
// Example of real IMAP operations (conceptual)
let mailboxes = client.list(Some(""), Some("*")).await?;
let search_result = client.search("UNSEEN").await?;
let email = client.fetch(email_id, "RFC822").await?;
```

## Build & Install

```bash
# Build the project
cargo build

# Build for release
cargo build --release

# Run tests
cargo test

# Run with help
cargo run -- --help
```

## License

MIT