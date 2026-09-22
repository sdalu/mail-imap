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

- List mailboxes/folders, selected with `-f`/`-A` (literal names or IMAP
  `LIST` patterns such as `Archive/*`)
- Search emails in one or more folders (pass any IMAP `SEARCH` query)
- Read email(s) by message selection: `[FOLDER::]UIDS`, with lists, ranges
  and `*` for every message
- Show message counts / status of mailboxes (IMAP `STATUS`)
- List the message UIDs of the selected folder(s)
- List the message UIDs of the thread(s) containing given message
  selection(s) (server-side RFC 5256 `THREAD` when the server advertises
  `THREAD=REFERENCES`, otherwise client-side reconstruction from
  Message-ID / References headers)
- List unread emails of one or more folders
- Sort search/unread results by uid/date/arrival/size/subject/from/to/cc
  (`-S`/`--sort`): server-side `UID SORT` (RFC 5256) when the server
  advertises `SORT`, client-side sorting otherwise
- List the MIME parts of an email, and save one part to a file
- Enable/disable the IMAP-defined message flags (`\Seen`, `\Answered`,
  `\Flagged`, `\Deleted`, `\Draft`) with `flag`, add/remove
  user-defined keyword tags with `tag`, and inspect either (`UID STORE`;
  `\Recent` is server-managed and cannot be set)
- Knows the IANA *IMAP/JMAP Keywords* registry and the conventions no
  registry covers (Thunderbird's `$label1`…`$label5`, `Junk`/`NonJunk`):
  a known keyword is sent in its settled spelling, a convention keyword
  is glossed when listed, and `tag known` prints both tables
- Handles modified UTF-7 keywords (RFC 3501 §5.1.3): `tag add … -- régie`
  sends `r&AOk-gie`, and `tag list` decodes what it shows
- JSON output mode (`-j`) for programmatic use
- Configuration via JSON file
- RFC 2047 subject decoding (UTF-8, Latin-1, B & Q encodings)

## Usage

### Commands

`<SELECTION>` is a message selection; see
[Message selections](#message-selections) below. Every command taking
one also accepts `--last N` / `--first N` in its place (except `part
save`, which needs exactly one message). Every command also
takes the folder flags `-f`/`-A`; see [Folder selection](#folder-selection).

| Command          | Syntax                                     | Description                                                                                                |
| ---------------- | ------------------------------------------ | ---------------------------------------------------------------------------------------------------------- |
| `folder`         | `folder`                                   | List mailboxes/folders                                                                                     |
| `search`         | `search <QUERY>`                           | Search emails with any IMAP `SEARCH` query in the selected folder(s); most recent first unless `-S`        |
| `read`           | `read <SELECTION...>`                      | Read the selected email(s)                                                                                 |
| `count`          | `count`                                    | Message counts / status of the selected folder(s), or every mailbox if none is given (alias: `status`)     |
| `uid`            | `uid`                                      | List the message UIDs of the selected folder(s), one block per folder                                      |
| `thread`         | `thread <SELECTION...>`                    | List the UIDs of every message in the thread(s) containing the selected message(s)                         |
| `unread`         | `unread`                                   | List unread emails of the selected folder(s) (`search UNSEEN`)                                             |
| `part list`      | `part list <SELECTION...>`                 | List the MIME parts of the selected email(s)                                                               |
| `part save`      | `part save <SELECTION> <PART> [-o <FILE>]` | Save one MIME part of one message to a file (the selection must name exactly one message)                  |
| `flag list`      | `flag list <SELECTION...>`                 | List the flags (system flags + keyword tags) of the selected email(s)                                      |
| `flag add`       | `flag add <SELECTION...> -- <FLAG...>`     | Enable IMAP-defined flags on the selected email(s): `\Seen`, `\Answered`, `\Flagged`, `\Deleted`, `\Draft` |
| `flag remove`    | `flag remove <SELECTION...> -- <FLAG...>`  | Disable flags on the selected email(s)                                                                     |
| `tag list`       | `tag list <SELECTION...>`                  | List the custom keyword tags of the selected email(s) (system flags omitted)                               |
| `tag add`        | `tag add <SELECTION...> -- <TAG...>`       | Add custom keyword tags (plain keywords, no system flags)                                                  |
| `tag remove`     | `tag remove <SELECTION...> -- <TAG...>`    | Remove custom keyword tags                                                                                 |
| `tag registered` | `tag registered`                           | List the IANA-registered keywords (no server needed)                                                       |

### Message selections

Every command that works on messages (`read`, `thread`, `part`, `flag`,
`tag`) takes one or more **message selections**: `[FOLDER::]UIDSPEC`.

```text
5                 one UID in the default folder
1,4,7             a list (separate arguments also work: read 1 4 7)
1-9               a UID range (IMAP's own 1:9 is accepted too)
3,9-12            a mix
'*'               every message of the folder (quote it — the shell globs it)
9-*               from UID 9 to the end (only this way round)
9-                the same, without the character the shell expands
Archive::1-5       folder-qualified
Archive/2026::7    folder names with the hierarchy delimiter work
'Sent Items::3,4'  folder names with spaces work (quote the whole token)
```

Two flags stand in for a selection where what you want is a **count**
rather than an interval — the thing no UID range can express, since UIDs
are sparse:

```bash
# the 5 newest messages (the 5 highest UIDs) of each selected folder
mail-imap --config incal.conf -f INBOX read --last 5
mail-imap --config incal.conf -f INBOX,Archive flag list -L 5

# the 5 oldest
mail-imap --config incal.conf -f INBOX read --first 5
```

`--last`/`--first` count by UID, which ascends with arrival — they say
nothing about the `Date:` header; order search results by that with
`-S -date`. They name a count rather than a UID, so they are not
ambiguous across folders: they mean N *per selected folder*, and they
cannot be combined with an explicit selection.

A few rules are easy to get wrong:

- A range is an interval of the UID space, **not a count**: UIDs are
  sparse, so `1-9` may match nine messages, two, or none.
- Ranges and `*` are resolved against the folder's real UID list (one
  `UID SEARCH ALL` per folder, fetched once and only when a selection for
  that folder holds a range or `*`). **Every item must match at least one
  message**, so `flag add 5 999-*` is an error rather than quietly
  becoming `flag add 5`.
- A UID named explicitly is not checked against that list for read-only
  commands — the server reports it (`no email with UID n`). `flag` and
  `tag` do check first, because RFC 3501 has `UID STORE` ignore an
  unknown UID *without an error*, and reporting success for a typo would
  be a lie the server never told.
- `*-9` / `*:9` are refused. IMAP accepts the open range both ways round,
  but reversed it reads as "up to 9" to half its readers (it means 9 to
  the end) and, in a selection, is indistinguishable from a folder
  wildcard. Write `9-*` or `9-`.
- **Deliberate deviation from IMAP:** RFC 3501 folds `559:*` onto the
  last message when 559 is past the end of the mailbox. This tool does
  not: an open range past the end matches nothing and errors, so
  `flag add 9999-* -- '\Deleted'` cannot silently hit the newest message.
- UID `0` is refused (IMAP UIDs start at 1).
- Folder wildcards (`*`, `%`) are refused inside a selection; name one
  folder, or use `-f` to work on several.
- `::` binds the folder and nothing else does, which leaves a single `:`
  entirely to the UID spec, where it is IMAP's range operator. So
  `INBOX::1:5` is UIDs 1-5 of INBOX, `1:5` is the range 1-5, and
  `2026::5` is UID 5 of a folder named `2026` — none of them a guess.
  The split is at the **last** `::`, so a folder whose own name contains
  `::` is still writable. A single `:` where `::` was meant is refused
  with the spelling it should have had.
- Folder names are trimmed (`-f 'INBOX, Trash'` is two folders, not
  `INBOX` and `" Trash"`), and `INBOX` is folded to one spelling because
  IMAP defines that one name as case-insensitive. Every other name keeps
  its case, which the server cares about.
- A range with no lower end (`-20`) is refused, and the argument parser
  usually rejects it first as a stray option. Write `1-20`; note that
  `*-20` is valid and means the opposite — 20 to the end, as in IMAP.
- Selections are grouped per folder; folders keep the order they were
  first named in, UIDs keep input order, and duplicates are dropped.
- `part save` takes a selection naming exactly one message, else it
  errors.
- `flag add`/`remove` and `tag add`/`remove` still take the flag/tag
  names after a `--` separator. Each command owns one kind of name and
  refuses the other's: `flag` takes only the five IMAP-defined flags,
  `tag` only user-defined keywords. That is also what stops
  `flag add 5 -- '\Deleted' Trash` from storing a keyword `Trash` on
  message 5 while leaving the Trash folder alone; on the `tag` side, a
  name that reads as a message selection (`tag add 5 -- invoice 6`) is
  refused for the same reason.

When a selection has no folder of its own, it means the folder(s)
selected by `-f`/`-A` (or the config); if several folders are selected,
a bare UID is ambiguous and is refused with an error naming the folders
— it is not guessed at.

### Folder selection

`-f`/`--folder` is the one way to name folders, on every command:

- repeatable **and** comma-separated: `-f INBOX -f Archive` is the same
  as `-f INBOX,Archive`;
- accepts IMAP `LIST` patterns: `*` crosses the hierarchy delimiter, `%`
  does not, so `-f 'Archive/*'` and `-f 'Archive/%'` differ. A pattern is
  matched against the mailbox list; a pattern matching no mailbox is an
  error. A literal name is passed through untouched even if `LIST` did
  not report it;
- `-A`/`--all-folders` is exactly shorthand for `-f '*'`;
- `INBOX` matches case-insensitively (as IMAP requires); other names do
  not;
- with no `-f`/`-A` given, the default is the `folder` field of the
  config (`INBOX`) — except for `count`, which still defaults to every
  selectable mailbox.

```bash
# List folders
mail-imap --config incal.conf folder

# Search emails (IMAP SEARCH query, e.g. "UNSEEN", "FROM bob", "SUBJECT invoice",
# "SINCE 01-Jan-2026", or any combination of terms)
mail-imap --config incal.conf search "SINCE 01-Jan-2026"

# Search several folders with -f: repeatable or comma-separated, both
# the same (IMAP can only search one selected mailbox at a time, so the
# tool iterates over the folders and aggregates; the -M/--max cap
# applies to the total)
mail-imap --config incal.conf -f INBOX -f "Sent Items" search UNSEEN
mail-imap --config incal.conf -f "INBOX,Sent Items" search UNSEEN

# -f also takes IMAP LIST patterns ('S*' matches every mailbox starting
# with S; 'Archive/*' would cross the hierarchy delimiter, 'Archive/%'
# would not); -A/--all-folders is shorthand for -f '*'
mail-imap --config incal.conf -f 'S*' search UNSEEN
mail-imap --config incal.conf -A search UNSEEN

# List every email in the folder: the query "ALL" matches all messages.
# Results are capped by --max / "max" in the config (default 50; 0 =
# unlimited). Just the UIDs of all messages: use `uid` instead.
mail-imap --config incal.conf -f INBOX search ALL
mail-imap --config incal.conf -f INBOX -M 200 search ALL   # raise the cap for this run

# Read email(s) by message selection: [FOLDER::]UIDS -- separate
# arguments, a comma list, a range, or '*' for every message (quote it,
# the shell globs it). A folder-qualified selection reaches another
# folder without touching -f.
mail-imap --config incal.conf -f INBOX read 12345
mail-imap --config incal.conf -f INBOX read 12345 67890
mail-imap --config incal.conf -f INBOX read 12345,67890   # same thing
mail-imap --config incal.conf -f INBOX read 1-50
mail-imap --config incal.conf -f INBOX read '*'
mail-imap --config incal.conf read Archive::12345 "Sent Items::1-5"

# Message counts / status (every selectable mailbox with no -f/-A, or
# the selected folder(s))
mail-imap --config incal.conf count
mail-imap --config incal.conf -f INBOX count
mail-imap --config incal.conf -f INBOX status   # "status" is an alias of "count"

# List the message UIDs of the selected folder(s), one block per folder
mail-imap --config incal.conf -f INBOX uid
mail-imap --config incal.conf -A uid

# List the UIDs of every message in the thread(s) containing the
# selected message(s) (no server THREAD extension needed; reads
# Message-ID / References / In-Reply-To of the folder's messages as raw
# header literals)
mail-imap --config incal.conf -f INBOX thread 12345

# List unread emails of the selected folder(s)
mail-imap --config incal.conf -f INBOX unread
mail-imap --config incal.conf -f INBOX -f Archive unread

# List the MIME parts of one or more emails
mail-imap --config incal.conf -f INBOX part list 12345
mail-imap --config incal.conf -f INBOX part list 12345 67890

# Save one part to a file; the selection must name exactly one message
# (default: the part's filename, else uid<N>_part<M>, in the current
# directory; -o to choose a path)
mail-imap --config incal.conf -f INBOX part save 12345 2 -o /tmp/invoice.pdf

# Sort search/unread results (default: most recent first). Server-side
# UID SORT (RFC 5256) when the server advertises SORT, else client-side.
mail-imap --config incal.conf -S -date -f INBOX search ALL
mail-imap --config incal.conf -S "subject,-size" -f INBOX search UNSEEN

# Enable/disable message flags (\Seen, \Answered, \Flagged, \Deleted,
# \Draft, or keywords; \Recent is server-managed and rejected)
mail-imap --config incal.conf -f INBOX flag list 12345 67890
mail-imap --config incal.conf -f INBOX flag add 12345 67890 -- '\Flagged'
mail-imap --config incal.conf -f INBOX flag remove 12345 -- '\Seen'

# The keywords with an agreed meaning, for reference (no server needed):
# the IANA registry, then what clients write without one
mail-imap --mock tag known

# Add/remove custom keyword tags (plain keywords only, no \system flags);
# after 'flag'/'tag' add|remove the names follow a '--' separator
mail-imap --config incal.conf -f INBOX tag list 12345
mail-imap --config incal.conf -f INBOX tag add 12345 -- invoice '$Important'
mail-imap --config incal.conf -f INBOX tag remove 12345 -- invoice

# JSON output (machine-readable: one compact JSON object on stdout)
mail-imap --config incal.conf -j -f INBOX search "SINCE 01-Jan-2026"

# Run against the in-memory mock (no server needed) for testing/demos
mail-imap --mock folder
mail-imap --mock search invoice
```

### JSON output (`-j`)

Each command prints one compact JSON object to stdout:

| Command                                               | Shape                                                                                                                                                                                                                       |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `folder`                                              | `{"count", "folders": [...]}`                                                                                                                                                                                               |
| `search`                                              | one folder: `{"folder", "query", "count", "results": [...]}`; several folders: `{"folders": [...], "query", "count", "results": [...]}`. Each result includes `"folder"` (its mailbox) and `"parts"` (number of MIME parts) |
| `read`                                                | one `{"folder", "uid", "content"}` object per selected UID                                                                                                                                                                  |
| `count` / `status`                                    | `{"all", "counts": [{"name", "messages", "unseen", "recent", "uid_next", "uid_validity"}]}`                                                                                                                                 |
| `uid`                                                 | one `{"folder", "count", "uids": [1, 2, ...]}` object per selected folder                                                                                                                                                   |
| `thread`                                              | one `{"folder", "uid", "count", "uids": [1, 2, ...]}` object per selected message (all UIDs of the thread containing `uid`, ascending, `uid` included)                                                                      |
| `unread`                                              | same shape as `search` (query fixed to `UNSEEN`, folder(s) + part counts included)                                                                                                                                          |
| `part list`                                           | one `{"folder", "uid", "count", "parts": [{"part", "content_type", "filename", "size"}]}` per selected UID                                                                                                                  |
| `part save`                                           | `{"folder", "uid", "part", "file", "size"}`                                                                                                                                                                                 |
| `flag list` / `tag list`                              | one `{"folder", "uid", "count", "flags": [...]}` per selected UID (`\Recent` omitted; `tag list` keeps keywords only)                                                                                                       |
| `flag add` / `flag remove` / `tag add` / `tag remove` | one `{"folder", "count", "uids": [...], "added": [...], "removed": [...]}` object per selected folder (`added` populated by add, `removed` by remove)                                                                       |

Errors are printed as `{"error": "..."}` on stderr with a non-zero exit code.

### Global flags

| Flag                  | Meaning                                                                                                                                                                                                                                                                                      |
| --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `-c, --config <PATH>` | Config file (also reads `$MAIL_IMAP_CONFIG`, else `/etc/mail-imap.conf`)                                                                                                                                                                                                                     |
| `-f, --folder <NAME>` | Folder(s) to operate on: a literal name or an IMAP `LIST` pattern (`*` crosses the hierarchy delimiter, `%` does not). Repeatable and comma-separated. Default: `folder` from the config (`INBOX`), except for `count`, which defaults to every mailbox. An empty name (`-f ''`) is refused. |
| `-A, --all-folders`   | Every selectable mailbox of the account (shorthand for `-f '*'`)                                                                                                                                                                                                                             |
| `--mock`              | Use the in-memory mock backend instead of a real server                                                                                                                                                                                                                                      |
| `-j, --json`          | Output results as compact single-line JSON (errors as `{"error": ...}` on stderr)                                                                                                                                                                                                            |
| `-M, --max <N>`       | Cap search results for this run, overriding `max` from the config (`0` = unlimited)                                                                                                                                                                                                          |
| `-S, --sort <SPEC>`   | Sort `search`/`unread` results: comma-separated criteria (`uid`, `date`, `arrival`, `size`, `subject`, `from`, `to`, `cc`), first = primary, `-` prefix = descending (e.g. `-date`, `subject,-size`); overrides `sort` from the config. Default: most recent first                           |

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

| Field      | Default | Description                                                                                                  |
| ---------- | ------- | ------------------------------------------------------------------------------------------------------------ |
| `server`   | —       | IMAP host                                                                                                    |
| `port`     | `993`   | IMAP port                                                                                                    |
| `username` | —       | Login user                                                                                                   |
| `password` | —       | Login password (use an app password for e.g. Gmail)                                                          |
| `ssl`      | `true`  | Implicit TLS (typical for port 993)                                                                          |
| `starttls` | `false` | Upgrade a plain connection with STARTTLS (typical for port 143). Used when `ssl` is `false`.                 |
| `insecure` | `false` | Accept invalid TLS certificates (self-signed local servers)                                                  |
| `folder`   | `INBOX` | Default folder for commands that need one                                                                    |
| `max`      | `50`    | Max search results to fetch (`0` = unlimited); overridden by `-M/--max` on the command line                  |
| `sort`     | `null`  | Default sort spec for `search`/`unread` (same format as `-S/--sort`); overridden by `-S` on the command line |
| `mock`     | `false` | Use the in-memory mock backend                                                                               |

## Testing

```bash
# Build and run the suite against the mock backend (no server needed)
make tests

# Equivalent, plain cargo
cargo test
```

The suite covers the mock backend end-to-end (list / search / read / count /
uid / unread / part), the message-selection grammar and folder-pattern
matching, the MIME parser, backend selection, the RFC 2047 decoder, and
that the real backend fails cleanly rather than fabricating data when no
server is reachable.

## Architecture

```text
src/
  main.rs          CLI entry (clap)
  config/mod.rs    JSON config loading
  cli/
    mod.rs         command handlers / output formatting, per-folder UID groups
    select.rs      message selection grammar, folder-pattern matching/expansion
  imap/
    mod.rs         ImapBackend trait, shared types, backend selection
    real.rs        RealClient  — talks to a real IMAP server (imap crate)
    mock.rs        MockClient  — in-memory mock (original mockup), used by tests
    mime.rs        minimal MIME parser (parts, boundary, CTE decoding)
    sort.rs        --sort spec parsing + client-side result ordering
```

See `DESIGN.md` for details.

## Build & Install

The `Makefile` is the interface (`make` alone, or `make help`, prints
what it does):

| Target               | Does                                                                                                                                                            |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `make` / `make help` | Print the target list and the current build variables (`help` is the default target)                                                                            |
| `make check`         | Preflight: `clippy --all-targets -- -D warnings`, plus `check-version` (fails if a version literal reappears in `src/`, other than through `CARGO_PKG_VERSION`) |
| `make build`         | Build the binary (`RELEASE=no` for a debug build; release is the default)                                                                                       |
| `make tests`         | Build and run the suite (mock backend, no server)                                                                                                               |
| `make doc`           | Generate the API documentation (`cargo doc --no-deps`)                                                                                                          |
| `make install`       | Install the binary under `PREFIX` (`DESTDIR` for a staging prefix)                                                                                              |
| `make clean`         | Remove what a build here made (`cargo clean`)                                                                                                                   |

`cargo` commands still work directly:

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
