# mail-imap - IMAP Email Client

A command-line tool for querying and reading emails via the IMAP protocol.
Designed for programmatic / AI use.

It is **passively read-only**: nothing ever moves or deletes mail, and
reads use `BODY.PEEK[]` so fetching a message does not even mark it
`\Seen`. The only modifications are the explicit `flag` / `tag` commands
(`UID STORE`), which you have to ask for.

It connects to a **real IMAP server** using the `imap` crate (over implicit TLS
on port 993, STARTTLS, or plain TCP). An **in-memory mock backend** is kept so
the tool can be built, demoed and unit-tested without a reachable server.

## Features

- List mailboxes/folders
- Search emails in one or more folders (pass any IMAP `SEARCH` query)
- Read email(s) by UID selection (comma-separated list, no ranges)
- Show message counts / status of mailboxes (IMAP `STATUS`)
- List the message UIDs of a folder
- List the message UIDs of the thread containing a given message
  (server-side RFC 5256 `THREAD` when the server advertises
  `THREAD=REFERENCES`, otherwise client-side reconstruction from
  Message-ID / References headers)
- List unread emails of a folder
- Sort search/unread results by uid/date/arrival/size/subject/from/to/cc
  (`-S`/`--sort`): server-side `UID SORT` (RFC 5256) when the server
  advertises `SORT`, client-side sorting otherwise
- List the MIME parts of an email, and save one part to a file
- Enable/disable message flags (`\Seen`, `\Answered`, `\Flagged`,
  `\Deleted`, `\Draft`, or keywords such as `junk`), add/remove custom
  keyword tags, and inspect them: `flag` / `tag` commands (`UID STORE`;
  `\Recent` is server-managed and cannot be set)
- JSON output mode (`-j`) for programmatic use
- Configuration via JSON file
- RFC 2047 subject decoding (UTF-8, Latin-1, B & Q encodings)

## Usage

### Commands

| Command | Syntax | Description |
|---------|--------|-------------|
| `folder` | `folder` | List mailboxes/folders |
| `search` | `search <QUERY> [FOLDER...]` | Search emails with any IMAP `SEARCH` query; one or more folders (comma-separated or repeated, default `-f`/config); most recent first unless `-S` |
| `read` | `read <UID[,UID...]>` | Read email(s) by UID (comma-separated list, no ranges) |
| `count` | `count [FOLDER]` | Message counts / status of all mailboxes or one (alias: `status`) |
| `uid` | `uid` | List the message UIDs of the folder |
| `thread` | `thread <UID>` | List the UIDs of every message in the thread containing the given message |
| `unread` | `unread [FOLDER...]` | List unread emails of one or more folders (`search UNSEEN`) |
| `part list` | `part list <UID[,UID...]>` | List the MIME parts of email(s) |
| `part save` | `part save <UID> <PART> [-o <FILE>]` | Save one MIME part to a file (default: the part's filename in the current directory) |
| `flag list` | `flag list <UID[,UID...]>` | List the flags (system flags + keyword tags) of email(s) |
| `flag add` | `flag add <UID[,UID...]> <FLAG...>` | Enable flags on email(s): `\Seen`, `\Answered`, `\Flagged`, `\Deleted`, `\Draft` or keywords (e.g. `junk`) |
| `flag remove` | `flag remove <UID[,UID...]> <FLAG...>` | Disable flags on email(s) |
| `tag list` | `tag list <UID[,UID...]>` | List the custom keyword tags of email(s) (system flags omitted) |
| `tag add` | `tag add <UID[,UID...]> <TAG...>` | Add custom keyword tags (plain keywords, no system flags) |
| `tag remove` | `tag remove <UID[,UID...]> <TAG...>` | Remove custom keyword tags |

```bash
# List folders
mail-imap --config incal.conf folder

# Search emails (IMAP SEARCH query, e.g. "UNSEEN", "FROM bob", "SUBJECT invoice",
# "SINCE 01-Jan-2026", or any combination of terms)
mail-imap --config incal.conf search "SINCE 01-Jan-2026"

# Search across several folders (IMAP can only search the selected mailbox,
# so the tool iterates over the folders and aggregates; the -M/--max cap
# applies to the total):
mail-imap --config incal.conf search UNSEEN INBOX "Sent Items" Archive
mail-imap --config incal.conf search UNSEEN "INBOX,Sent Items"  # comma form

# List every email in the folder: the query "ALL" matches all messages.
# Results are capped by --max / "max" in the config (default 50; 0 =
# unlimited). Just the UIDs of all messages: use `uid` instead.
mail-imap --config incal.conf -f INBOX search ALL
mail-imap --config incal.conf -f INBOX -M 200 search ALL   # raise the cap for this run

# Read an email by UID (default folder from config, or -f).
# UID selection is a comma-separated list; ranges like "1-5" are not supported.
mail-imap --config incal.conf -f INBOX read 12345
mail-imap --config incal.conf -f INBOX read 12345,67890

# Message counts / status (all mailboxes, or one)
mail-imap --config incal.conf count
mail-imap --config incal.conf count INBOX
mail-imap --config incal.conf status INBOX   # "status" is an alias of "count"

# List the message UIDs of the folder
mail-imap --config incal.conf -f INBOX uid

# List the UIDs of every message in the thread containing UID 12345
# (no server THREAD extension needed; reads Message-ID / References /
# In-Reply-To of the folder's messages as raw header literals).
mail-imap --config incal.conf -f INBOX thread 12345

# List unread emails of the folder (or several: unread INBOX Archive)
mail-imap --config incal.conf -f INBOX unread

# List the MIME parts of one or more emails
mail-imap --config incal.conf -f INBOX part list 12345
mail-imap --config incal.conf -f INBOX part list 12345,67890

# Save one part to a file (default: the part's filename, else
# uid<N>_part<M>, in the current directory; -o to choose a path)
mail-imap --config incal.conf -f INBOX part save 12345 2 -o /tmp/invoice.pdf

# Sort search/unread results (default: most recent first). Server-side
# UID SORT (RFC 5256) when the server advertises SORT, else client-side.
mail-imap --config incal.conf -S -date -f INBOX search ALL
mail-imap --config incal.conf -S "subject,-size" -f INBOX search UNSEEN

# Enable/disable message flags (\Seen, \Answered, \Flagged, \Deleted,
# \Draft, or keywords; \Recent is server-managed and rejected)
mail-imap --config incal.conf -f INBOX flag list 12345,67890
mail-imap --config incal.conf -f INBOX flag add 12345,67890 '\Flagged'
mail-imap --config incal.conf -f INBOX flag remove 12345 '\Seen' junk

# Add/remove custom keyword tags (plain keywords only, no \system flags)
mail-imap --config incal.conf -f INBOX tag list 12345
mail-imap --config incal.conf -f INBOX tag add 12345 invoice $Important
mail-imap --config incal.conf -f INBOX tag remove 12345 invoice

# JSON output (machine-readable: one compact JSON object on stdout)
mail-imap --config incal.conf -j -f INBOX search "SINCE 01-Jan-2026"

# Run against the in-memory mock (no server needed) for testing/demos
mail-imap --mock folder
mail-imap --mock search invoice
```

### JSON output (`-j`)

Each command prints one compact JSON object to stdout:

| Command | Shape |
|---------|-------|
| `folder` | `{"count", "folders": [...]}` |
| `search` | one folder: `{"folder", "query", "count", "results": [...]}`; several folders: `{"folders": [...], "query", "count", "results": [...]}`. Each result includes `"folder"` (its mailbox) and `"parts"` (number of MIME parts) |
| `read` | one `{"folder", "uid", "content"}` object per selected UID |
| `count` / `status` | `{"all", "counts": [{"name", "messages", "unseen", "recent", "uid_next", "uid_validity"}]}` |
| `uid` | `{"folder", "count", "uids": [1, 2, ...]}` |
| `thread` | `{"folder", "uid", "count", "uids": [1, 2, ...]}` (all UIDs of the thread containing `uid`, ascending, `uid` included) |
| `unread` | same shape as `search` (query fixed to `UNSEEN`, folder(s) + part counts included) |
| `part list` | one `{"folder", "uid", "count", "parts": [{"part", "content_type", "filename", "size"}]}` per selected UID |
| `part save` | `{"folder", "uid", "part", "file", "size"}` |
| `flag list` / `tag list` | one `{"folder", "uid", "count", "flags": [...]}` per selected UID (`\Recent` omitted; `tag list` keeps keywords only) |
| `flag add` / `flag remove` / `tag add` / `tag remove` | `{"folder", "count", "uids": [...], "added": [...], "removed": [...]}` (`added` populated by add, `removed` by remove) |

Errors are printed as `{"error": "..."}` on stderr with a non-zero exit code.

### Global flags

| Flag | Meaning |
|------|---------|
| `-c, --config <PATH>` | Config file (also reads `$MAIL_IMAP_CONFIG`, else `/etc/mail-imap.conf`) |
| `-f, --folder <NAME>` | Folder for commands that need one (default: `folder` from config / `INBOX`) |
| `--mock` | Use the in-memory mock backend instead of a real server |
| `-j, --json` | Output results as compact single-line JSON (errors as `{"error": ...}` on stderr) |
| `-M, --max <N>` | Cap search results for this run, overriding `max` from the config (`0` = unlimited) |
| `-S, --sort <SPEC>` | Sort `search`/`unread` results: comma-separated criteria (`uid`, `date`, `arrival`, `size`, `subject`, `from`, `to`, `cc`), first = primary, `-` prefix = descending (e.g. `-date`, `subject,-size`); overrides `sort` from the config. Default: most recent first |

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
    "sort": null,
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
| `max` | `50` | Max search results to fetch (`0` = unlimited); overridden by `-M/--max` on the command line |
| `sort` | `null` | Default sort spec for `search`/`unread` (same format as `-S/--sort`); overridden by `-S` on the command line |
| `mock` | `false` | Use the in-memory mock backend |

## Testing

```bash
# Unit + integration tests (run entirely against the mock backend, no server needed)
cargo test
```

The tests cover the mock backend end-to-end (list / search / read / count /
uid / unread / part), UID-selection parsing, the MIME parser, backend
selection, the RFC 2047 decoder, and that the real backend fails cleanly
rather than fabricating data when no server is reachable.

## Architecture

```
src/
  main.rs          CLI entry (clap)
  config/mod.rs    JSON config loading
  cli/mod.rs       command handlers / output formatting, UID selection parser
  imap/
    mod.rs         ImapBackend trait, shared types, backend selection
    real.rs        RealClient  — talks to a real IMAP server (imap crate)
    mock.rs        MockClient  — in-memory mock (original mockup), used by tests
    mime.rs        minimal MIME parser (parts, boundary, CTE decoding)
    sort.rs        --sort spec parsing + client-side result ordering
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
