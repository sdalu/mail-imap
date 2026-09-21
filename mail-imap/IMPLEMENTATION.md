# IMAP Implementation

This document describes how mail-imap implements real IMAP access.

## Backends

Operations are exposed through the `ImapBackend` trait (`src/imap/mod.rs`).
Two implementations exist and are selected by `Config::mock`:

| Backend | File | Used by |
|---------|------|---------|
| `RealClient` | `src/imap/real.rs` | The released binary (real servers) |
| `MockClient` | `src/imap/mock.rs` | Tests / demos (in-memory, the original mockup) |

The CLI always calls the trait; it does not know which backend is active.
`ImapClient::connect` picks one:

```rust
if config.mock { MockClient::connect(cfg) } else { RealClient::connect(cfg) }
```

## Real backend (`real.rs`)

Uses the `imap` crate (v2.4) over `std::net::TcpStream` + `native-tls`.

### Connection
1. Resolve the host and `TcpStream::connect_timeout` (10 s).
2. Branch on config:
   - `ssl: true` → implicit TLS via `TlsConnector::connect` (port 993 style).
   - `ssl: false, starttls: true` → plain connect then `Client::secure` (STARTTLS).
   - otherwise → plain TCP.
3. `read_greeting` then `login(username, password)`.

The session is stored as either a TLS or plain variant in an enum so a single
operation body can be shared by a `with_backend!` macro.

### Operations

| CLI command | IMAP commands used |
|-------------|--------------------|
| `folders` | `LIST "" *` (non-`\Noselect` names) |
| `search` | `SELECT` + `UID SEARCH <query>` + batched `UID FETCH` (envelope/flags/date/size) |
| `read` | `SELECT` + `UID FETCH <uid> (UID ENVELOPE FLAGS INTERNALDATE RFC822)` |
| `move` | `SELECT` + (`UID MOVE` if `MOVE` capability, else `UID COPY` + `UID STORE \Deleted` + `EXPUNGE`) |
| `tag add` | `SELECT` + `UID STORE <uid> +FLAGS (tag1 tag2 ...)` |
| `tag remove` | `SELECT` + `UID STORE <uid> -FLAGS (tag1 ...)` |
| `flags add` | `SELECT` + `UID STORE <uid> +FLAGS (\Seen \Answered ...)` |
| `flags remove` | `SELECT` + `UID STORE <uid> -FLAGS (\Flagged ...)` |

Search results are shown most-recent-first and capped by `Config::max` (default
50) to keep large mailboxes fast.

### Flag validation

The `flags` command only accepts the standard flags `\Seen`, `\Answered` and
`\Flagged` (given case-insensitively, with or without the leading backslash).
`\Deleted`, `\Draft` and `\Recent` are explicitly not supported and rejected
with a dedicated error, as are any other names. Normalization happens in
`normalize_flags` (`src/imap/mod.rs`) and is applied by both backends, so the
restriction holds regardless of which backend is active.

### RFC 2047 subject decoding
ENVELOPE subjects may be encoded-words (e.g. `=?utf-8?Q?Votre=20facture?=`).
`decode_rfc2047` finds each well-formed encoded word in the value and decodes
it (UTF-8, ISO-8859-1/Latin-1, `B` and `Q` encodings, padding restored for
base64). Plain text is left untouched.

## Mock backend (`mock.rs`)

The original mockup, preserved. Returns a fixed set of folders and five sample
messages so every operation can be exercised offline. It is what the unit
tests drive.

## Testing

`cargo test` runs entirely against the mock backend (no network). Coverage:
- backend selection (`mock` vs `real`)
- list / search / read / move / tag / flags happy + error paths
- RFC 2047 decoder (plain, Q, Q-with-underscore, B, Latin-1, mixed text)
- the real backend returns an error (no fabricated data) when unreachable

To exercise the real backend manually:

```bash
cargo run --release -- -c incal.conf folders
cargo run --release -- -c incal.conf -f INBOX search "SINCE 01-Jan-2026"
```

## Output

Commands print human-readable text by default. With `-j`/`--json` each command
prints a single compact JSON object to stdout instead; errors become
`{"error": "..."}` on stderr (exit code unchanged). JSON shapes:

| Command | Shape |
|---------|-------|
| `folders` | `{"count", "folders": [FolderInfo]}` |
| `search` | `{"folder", "query", "count", "results": [SearchResult]}` |
| `read` | `{"folder", "uid", "content"}` |
| `move` | `{"folder", "uid", "to"}` |
| `tag` / `flags` | `{"folder", "uid", "added", "removed"}` |

`FolderInfo` and `SearchResult` derive `serde::Serialize`; the other shapes are
small output structs in `src/cli/mod.rs`.

## Error handling

Connection, TLS, authentication and server (BAD/NO) errors surface as
`anyhow::Error` with context; the CLI prints them and exits non-zero.
