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
| `search` / `unread` | per folder: `SELECT` + `UID SEARCH <query>` + batched `UID FETCH` (envelope/flags/date/size/`BODYSTRUCTURE`); folders are searched in order and the results aggregated |
| `read` | `SELECT` + `UID FETCH <uid> (UID ENVELOPE FLAGS INTERNALDATE BODY.PEEK[RFC822])` |
| `count` / `status` | `LIST "" *` + `STATUS <folder> (MESSAGES UNSEEN RECENT UIDNEXT UIDVALIDITY)` per mailbox (or one folder when given) |
| `ids` | `SELECT` + `UID SEARCH *` |
| `thread` | `SELECT` + `UID SEARCH ALL` + batched `UID FETCH <uids> (UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID REFERENCES IN-REPLY-TO)])`, then client-side union-find over Message-IDs |
| `unread` | `SELECT` + `UID SEARCH UNSEEN` + batched `UID FETCH` (same path as `search`) |
| `parts list` | `SELECT` + `UID FETCH <uid> (UID BODY.PEEK[RFC822])`, then MIME part enumeration locally |
| `parts save` | same fetch, then the selected part is CTE-decoded and written to a file |

IMAP can only search the selected mailbox, so `search`/`unread` iterate over
the requested folders (positional args, comma-separated or repeated; default
the `-f`/config folder) on one connection. Results are grouped per folder,
most-recent first within each folder, and the total result count is capped
by `Config::max` (default 50; overridable per run with `-M/--max`,
`0` = unlimited). Each result carries the `folder` it came from; the JSON
output uses `"folder"` for a single folder and `"folders"` for several.

### Part counts in search results

Each search hit reports `parts`, the number of leaf MIME parts. It comes from
the `BODYSTRUCTURE` fetch item added to the same batched fetch: the server
returns the MIME tree without transferring any content, and
`count_leaf_parts` (`src/imap/real.rs`) sums the leaf nodes (multipart
containers recurse; `message/rfc822` and single parts count as one, matching
the local parser used by `parts list`). `unread` reuses this path, so it
includes part counts too.

The part count is best-effort: batches are fetched through a degradation
ladder (see "Resilient fetching" below), and a message whose MIME tree the
`imap` crate cannot parse is reported with `0` parts — the search itself is
never broken by it.

### Thread reconstruction (`thread <uid>`)

No IMAP extension is required: the thread is rebuilt **client-side**. The
tool fetches the `Message-ID`, `References` and `In-Reply-To` headers of
every message in the folder (batched `UID FETCH`, 100 UIDs at a time, using
a literal `BODY.PEEK[HEADER.FIELDS ...]` item — raw bytes that never pass
through the quoted-string parser, so unparseable server data cannot break
it), then runs union-find (`thread_component` in `src/imap/mod.rs`):

- messages are linked when one references the other's Message-ID;
- duplicate Message-IDs (cross-posted copies) merge into one node;
- the output is the connected component containing the requested UID,
  ascending, always including the UID itself.

A message without usable threading headers is its own thread. If a batch
still cannot be fetched/parsed it is skipped with a warning on stderr, so
the result may be incomplete but never fatal.

### Resilient fetching

Some servers emit `FETCH` responses with raw 8-bit bytes inside quoted
strings (e.g. Latin-1 in `ENVELOPE`). `imap-proto` only accepts 7-bit
characters there; the `imap` crate turns such a line into a fabricated
`Error::Bye` *and* leaves the stream desynced — which previously aborted a
whole search with "Bye Response: no explanation given". `RealClient`
countermeasures (`src/imap/real.rs`):

- `attempt_fetch` recognizes connection-poisoning errors (`Bye`,
  `TagMismatch`, `ConnectionLost`, `Io`), re-establishes the session
  (`reconnect`) and retries once;
- `fetch_chunk` degrades per batch: full items → without `BODYSTRUCTURE` →
  per-message → per-message via a literal `BODY.PEEK[HEADER.FIELDS
  (SUBJECT FROM DATE)]` fetch with the metadata rebuilt from raw bytes
  (`header_value` / `address_from_header`);
- individual unparseable messages are skipped with a stderr warning, and
  `read` falls back to an `ENVELOPE`-less fetch whose summary is built from
  the raw RFC822 header block;
- if a hard error still strikes mid-search, the results collected so far
  are reported with a warning instead of being thrown away.

### Read-only behaviour

No operation sends a command that mutates the mailbox: there is no `MOVE`,
`COPY`, `STORE`, or `EXPUNGE` anywhere. Message bodies are fetched with
`BODY.PEEK[RFC822]`, so even `read` and `parts` do not make the server set
`\Seen`.

### UID selection

Commands that take messages (`read`, `parts list`) accept a UID selection:
a single UID or a comma-separated list (`1,4,7`). Ranges (`1-7`) are rejected
by `parse_uids` (`src/cli/mod.rs`) with a dedicated error. The list is
deduplicated, input order is preserved, and each UID is fetched individually.

### MIME parsing (`mime.rs`)

`parts` works on the raw RFC822 bytes: headers are unfolded and parsed
(`Content-Type`, `Content-Disposition`, `Content-Transfer-Encoding`),
`multipart/*` bodies are split on their boundary lines (preamble and epilogue
discarded), and leaf parts are numbered in document order (1-based). Part
bytes are decoded per CTE: base64 (whitespace-tolerant, padding restored),
quoted-printable (hex escapes + soft line breaks), or raw for 7bit/8bit/binary.

### RFC 2047 subject decoding
ENVELOPE subjects may be encoded-words (e.g. `=?utf-8?Q?Votre=20facture?=`).
`decode_rfc2047` finds each well-formed encoded word in the value and decodes
it (UTF-8, ISO-8859-1/Latin-1, `B` and `Q` encodings, padding restored for
base64). Plain text is left untouched.

## Mock backend (`mock.rs`)

The original mockup, preserved. Returns a fixed set of folders and five sample
messages so every operation can be exercised offline. Two messages carry
extra part metadata (UID 3: spreadsheet, UID 5: PDF); `parts save` writes a
deterministic placeholder file whose size matches the part reported by
`parts list`. It is what the unit tests drive.

## Testing

`cargo test` runs entirely against the mock backend (no network). Coverage:
- backend selection (`mock` vs `real`)
- list / search / read / count / ids / unread / parts happy + error paths
- UID selection parsing (comma lists, dedup, range rejection, invalid input)
- MIME parser (plain, multipart, nested multipart, base64 / quoted-printable /
  binary decoding, missing boundary)
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
| `read` | one `{"folder", "uid", "content"}` per selected UID |
| `count` / `status` | `{"all", "counts": [Mailbox]}` |
| `ids` | `{"folder", "count", "uids": [u32]}` |
| `unread` | same as `search` (query `UNSEEN`) |
| `parts list` | one `{"folder", "uid", "count", "parts": [PartInfo]}` per selected UID |
| `parts save` | `{"folder", "uid", "part", "file", "size"}` |

`FolderInfo`, `SearchResult`, `Mailbox` and `PartInfo` derive
`serde::Serialize`; the other shapes are small output structs in
`src/cli/mod.rs`.

## Error handling

Connection, TLS, authentication and server (BAD/NO) errors surface as
`anyhow::Error` with context; the CLI prints them and exits non-zero.

`LOGOUT` is sent exactly once when the client is dropped (`RealClient` tracks
a `closed` flag so the outer and inner `Drop` impls cannot double-send it).
A `ConnectionLost` at logout is suppressed — it only means the connection was
already closed and there is nothing to log out. Other logout failures (server
BAD/NO) still print a warning.
