# mail-imap - IMAP Email Client

A command-line tool for querying, reading, moving and tagging emails via the
IMAP protocol. Designed for programmatic / AI use.

It connects to a **real IMAP server** using the `imap` crate (over implicit TLS
on port 993, STARTTLS, or plain TCP). An **in-memory mock backend** is kept so
the tool can be built, demoed and unit-tested without a reachable server.

## Features

- List mailboxes/folders
- Search emails (pass any IMAP `SEARCH` query)
- Read a full email by UID
- Move an email to another folder
- Tag an email with keyword flags
- Configuration via JSON file
- RFC 2047 subject decoding (UTF-8, Latin-1, B & Q encodings)

## Usage

```bash
# List folders
mail-imap --config incal.conf folders

# Search emails (IMAP SEARCH query, e.g. "UNSEEN", "FROM bob", "SUBJECT invoice",
# "SINCE 01-Jan-2026", or any combination of terms)
mail-imap --config incal.conf search "SINCE 01-Jan-2026"

# Read an email by UID (default folder from config, or -f)
mail-imap --config incal.conf -f INBOX read 12345

# Move an email by UID to another folder
mail-imap --config incal.conf -f INBOX move 12345 Archive

# Tag an email by UID (adds keyword flags)
mail-imap --config incal.conf -f INBOX tag 12345 important reviewed

# Run against the in-memory mock (no server needed) for testing/demos
mail-imap --mock folders
mail-imap --mock search invoice
```

### Global flags

| Flag | Meaning |
|------|---------|
| `-c, --config <PATH>` | Config file (also reads `$MAIL_IMAP_CONFIG`, else `/etc/mail-imap.conf`) |
| `-f, --folder <NAME>` | Folder for commands that need one (default: `folder` from config / `INBOX`) |
| `--mock` | Use the in-memory mock backend instead of a real server |

## Configuration

The config file is JSON. All fields except `server`, `username` and `password`
have defaults.

```json
{
    "server": "imap.example.com",
    "port": 993,
    "username": "user@example.com",
    "password": "secret",
    "ssl": true,
    "starttls": false,
    "insecure": false,
    "folder": "INBOX",
    "max": 50,
    "mock": false
}
```

| Field | Default | Description |
|-------|---------|-------------|
| `server` | — | IMAP host |
| `port` | `993` | IMAP port |
| `username` | — | Login user |
| `password` | — | Login password (use an app password for e.g. Gmail) |
| `ssl` | `true` | Implicit TLS (typical for port 993) |
| `starttls` | `false` | Upgrade a plain connection with STARTTLS (typical for port 143). Used when `ssl` is `false`. |
| `insecure` | `false` | Accept invalid TLS certificates (self-signed local servers) |
| `folder` | `INBOX` | Default folder for commands that need one |
| `max` | `50` | Max search results to fetch (`0` = unlimited) |
| `mock` | `false` | Use the in-memory mock backend |

## Testing

```bash
# Unit + integration tests (run entirely against the mock backend, no server needed)
cargo test
```

The tests cover the mock backend end-to-end (list / search / read / move / tag),
backend selection, the RFC 2047 decoder, and that the real backend fails cleanly
rather than fabricating data when no server is reachable.

## Architecture

```
src/
  main.rs          CLI entry (clap)
  config/mod.rs    JSON config loading
  cli/mod.rs       command handlers / output formatting
  imap/
    mod.rs         ImapBackend trait, shared types, backend selection
    real.rs        RealClient  — talks to a real IMAP server (imap crate)
    mock.rs        MockClient  — in-memory mock (original mockup), used by tests
```

See `IMPLEMENTATION.md` for details.

## Build & Install

```bash
cargo build            # debug
cargo build --release  # release
cargo test             # tests (mock backend)
cargo run -- --help
```

## Security note

`*.conf` files are git-ignored because they contain credentials. Keep real
configs (e.g. `incal.conf`) out of version control.

## License

MIT
