# mail-imap design

Why mail-imap is shaped the way it is, and what it puts on the wire:
the two backends, the IMAP commands each operation sends, the
fallbacks when a server or its responses fall short, and how a command
line becomes per-folder UID groups.

## Backends

Operations are exposed through the `ImapBackend` trait (`src/imap/mod.rs`).
Two implementations exist and are selected by `Config::mock`:

| Backend      | File               | Used by                                        |
| ------------ | ------------------ | ---------------------------------------------- |
| `RealClient` | `src/imap/real.rs` | The released binary (real servers)             |
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

| CLI command                                           | IMAP commands used                                                                                                                                                                                                                                                                                                                  |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `folder`                                              | `LIST "" *` (non-`\Noselect` names)                                                                                                                                                                                                                                                                                                 |
| `search` / `unread`                                   | per folder: `SELECT` + (`UID SORT <crit> UTF-8 <query>` when `--sort` is given and `SORT` is advertised, else `UID SEARCH <query>`) + batched `UID FETCH` (envelope/flags/date/size/`BODYSTRUCTURE`); folders are searched in order and the results aggregated                                                                      |
| `read`                                                | `SELECT` + `UID FETCH <uid> (UID ENVELOPE FLAGS INTERNALDATE BODY.PEEK[])`                                                                                                                                                                                                                                                          |
| `count` / `status`                                    | `LIST "" *` + `STATUS <folder> (MESSAGES UNSEEN RECENT UIDNEXT UIDVALIDITY)` per mailbox (or one folder when given)                                                                                                                                                                                                                 |
| `uid`                                                 | per selected folder: `SELECT` + `UID SEARCH ALL`                                                                                                                                                                                                                                                                                    |
| `thread`                                              | per selected message: `SELECT` + `CAPABILITY`; if `THREAD=REFERENCES` is advertised: one `UID THREAD REFERENCES UTF-8 ALL` (server-side tree, flattened). Otherwise: `UID SEARCH ALL` + batched `UID FETCH <uids> (UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID REFERENCES IN-REPLY-TO)])`, then client-side union-find over Message-IDs |
| `unread`                                              | `SELECT` + `UID SEARCH UNSEEN` + batched `UID FETCH` (same path as `search`)                                                                                                                                                                                                                                                        |
| `part list`                                           | per selected message: `SELECT` + `UID FETCH <uid> (UID BODY.PEEK[])`, then MIME part enumeration locally                                                                                                                                                                                                                            |
| `part save`                                           | same fetch, then the selected part is CTE-decoded and written to a file                                                                                                                                                                                                                                                             |
| `flag list` / `tag list`                              | per selected message: `SELECT` + `UID FETCH <uid> (UID FLAGS)`                                                                                                                                                                                                                                                                      |
| `flag add` / `flag remove` / `tag add` / `tag remove` | per selected folder: `SELECT` + `UID STORE <uids> +FLAGS (...)` / `-FLAGS (...)` per batch of 50                                                                                                                                                                                                                                    |

IMAP can only search the selected mailbox, so `search`/`unread` iterate over
the folders selected by `-f`/`-A` (repeatable, comma-separated, and IMAP
`LIST` patterns; default the `-f`/config folder) on one connection. Results are
grouped per folder, ordered by the `-S`/`--sort` criteria (default:
most-recent first within each folder), and the total result count is capped
by `Config::max` (default 50; overridable per run with `-M/--max`,
`0` = unlimited). Each result carries the `folder` it came from; the JSON
output uses `"folder"` for a single folder and `"folders"` for several.

`read`, `thread`, `part list`/`part save`, `flag` and `tag` take message
*selections* rather than bare UIDs; each selection resolves to one or more
`(folder, uid)` pairs before any of the operations above run — see
[From a command line to per-folder UID groups](#from-a-command-line-to-per-folder-uid-groups).

### Sorting (`--sort`/`-S`)

The spec is a comma-separated list of criteria, first = primary, with a
`-` prefix reversing that criterion (RFC 5256 `REVERSE`): `-date`,
`subject,-size`, ... Valid keys: `uid`, `date`, `arrival`, `size`,
`subject`, `from`, `to`, `cc` (`src/imap/sort.rs`). A `sort` config field
provides a default; `-S` overrides it.

Real backend: when a spec is given, the server advertises `SORT`
(RFC 5256) and the spec is server-sortable (no `uid` key), the UID list
comes from a single `UID SORT <criteria> UTF-8 <query>` — the cap is
applied to the *sorted* list, so truncation is exact. On `UID SORT`
failure (including a poisoned stream, with reconnect) or without the
capability, it falls back to `UID SEARCH` + fetching the newest
`max` messages and sorting that page client-side by the same criteria
(possible caveat: for non-date criteria the capped selection is
recency-based, so it can differ from a true sorted-and-capped result).
`to`/`cc` need the server-side path and error out when `SORT` is not
advertised. Fetched FETCH responses arrive in sequence order, so the
final result order is restored from the computed UID order on every
path.

The `imap` fork (`forks/rust-imap`, `extensions/sort.rs`) provides
`Session::uid_sort` and the `SortCriterion`/`SortCharset` types.

### Part counts in search results

Each search hit reports `parts`, the number of leaf MIME parts. It comes from
the `BODYSTRUCTURE` fetch item added to the same batched fetch: the server
returns the MIME tree without transferring any content, and
`count_leaf_parts` (`src/imap/real.rs`) sums the leaf nodes (multipart
containers recurse; `message/rfc822` and single parts count as one, matching
the local parser used by `part list`). `unread` reuses this path, so it
includes part counts too.

The part count is best-effort: batches are fetched through a degradation
ladder (see "Resilient fetching" below), and a message whose MIME tree the
`imap` crate cannot parse is reported with `0` parts — the search itself is
never broken by it.

### Thread reconstruction (`thread <uid>`)

Server-side threading is preferred when available: if `CAPABILITY`
advertises `THREAD=REFERENCES` (RFC 5256), a single
`UID THREAD REFERENCES UTF-8 ALL` returns the whole thread tree and the
sub-tree containing the requested UID is flattened to its UIDs. This is one
round trip instead of fetching every message's headers. The parser for the
nested `* THREAD (…)` response is provided by the bundled `imap-proto` fork
and the `Session::thread` / `Session::uid_thread` commands by the `imap`
fork (see `../forks`, pending upstream releases).

On servers that do not advertise THREAD — or if the THREAD command fails,
returns no thread for the UID, or the connection is lost — the tool falls
back to rebuilding the thread **client-side**, so no extension is required.
The tool fetches the `Message-ID`, `References` and `In-Reply-To` headers of
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

### Passive read-only behaviour

Nothing implicitly mutates the mailbox: there is no `MOVE`, `COPY` or
`EXPUNGE` anywhere, and message bodies are fetched with
`BODY.PEEK[]`, so even `read` and `part` do not make the server set
`\Seen`. The only write commands are `flag` and `tag`, which send
`UID STORE ±FLAGS` exclusively (they cannot move, delete, or expunge) and
must be invoked explicitly.

### Flag and tag changes (`flag`, `tag`)

Both share one backend operation (`store_flags`) and one handler
(`change_flags` in `src/cli/mod.rs`); they differ only in validation:

- `flag add|remove <SELECTION...> -- <FLAG...>` accepts only the
  IMAP-defined system flags `\Seen`, `\Answered`, `\Flagged`,
  `\Deleted` and `\Draft` (matched case-insensitively and normalized by
  `parse_flag_names`). `\Recent` is rejected: it is maintained by the
  server and cannot be set by clients. A user-defined keyword is refused
  with a pointer to `tag`: the two commands own one kind of name each,
  which is what keeps a stray word after `--` — `flag add 5 --
  '\Deleted' Trash` — from being stored as the keyword `Trash`.
- `tag add|remove <SELECTION...> -- <TAG...>` accepts only plain
  keywords — any `\`-prefixed name is refused with a hint to use `flag`.
- Keyword names are matched against the IANA *IMAP/JMAP Keywords*
  registry, transcribed into `src/cli/keywords.rs` (fetched 2026-09-22).
  It is advisory, never a whitelist: an unregistered keyword is legal
  and passes through as typed. What it buys is the spelling — a
  registered keyword typed in another case is sent in its registered
  form, the same normalization `\seen` → `\Seen` gets — and a listing,
  `tag known`, which needs no server because the table is in the binary.
- Beside it sits a second table of keywords no registry covers but that
  real clients write: Thunderbird's `$label1`…`$label5` and its
  `Junk`/`NonJunk`. It is a convention, never validation. Thunderbird
  keeps a tag's name and colour in the user's `prefs.js` and sends only
  the key, so a `$label1` arriving here is unreadable on its own — which
  is why `flag list` / `tag list` gloss those names in their text output
  (`$label1 (Thunderbird tag 1, "Important" unless renamed)`) while the
  JSON keeps the raw keyword. Thunderbird's own traces carry both
  `$label1` and `$Label1`, which is a shipping client agreeing that
  keywords do not differ by case.
- Thunderbird's *custom* tag keys are **not** modified UTF-7, whatever
  the 2007 wiki says. `nsMsgTagService::AddTag` (comm-central,
  `mailnews/base/src/nsMsgTagService.cpp`) converts the tag to UTF-8,
  keeps ASCII alphanumerics and the printable symbols outside
  `` =()[]{}%*"\<>;& ``, writes every other byte as `=%02x`, then
  lowercases the whole key — so "régie" is `r=c3=a9gie`, and a key it
  writes can never contain `&`. A `r&aok-gie` on a real server is
  therefore an *older* Thunderbird's output: that scheme did use
  modified UTF-7, and the same wholesale lowercasing flattened its
  base64. Which is why a decoded gloss can read as nonsense ("r檉gie")
  while the tag was plainly "régie" — the case was lost before this
  tool ever saw the name, and no decoder can put it back.
- `src/cli/tbkey.rs` reads the current scheme back, so `tag list` shows
  `r=c3=a9gie ("régie", Thunderbird tag key)`. Decoding only: this tool
  writes IMAP's own modified UTF-7 rather than adopting one client's
  scheme, but what it owes a reader is the ability to read what that
  client wrote. The decoder refuses anything `AddTag` could not have
  emitted — a raw `&` or space, non-ASCII, a truncated `=xx`, bytes that
  are not UTF-8 — so an ordinary keyword is never glossed as if it were
  a key.
- That registry is shared with JMAP, which spells four IMAP system flags
  as `$`-keywords (RFC 8621 §4.1.1 changes their leading `\` to `$`),
  plus `$recent`. Those five are refused with the flag they stand for,
  since storing `$seen` as a keyword sets something no IMAP client reads.
- Keyword names are IMAP atoms, which are ASCII, so anything else
  travels as modified UTF-7 (RFC 3501 §5.1.3, `src/cli/modutf7.rs`).
  `tag add 5 -- régie` sends `r&AOk-gie`; sending the raw bytes would be
  a malformed command, not a nicety. Names are NFC-composed first
  (`unicode-normalization`): "régie" typed on a system that hands over
  `e` + U+0301 would otherwise encode to a different atom that looks
  identical in every listing, and a later `tag remove` spelled the other
  way would silently miss it. `--wire` composes nothing, being verbatim
  by definition. **Input is literal text and every
  `&` is escaped to `&-`**; `--wire` sends the names verbatim, which is
  how a key copied out of a listing goes back. An earlier rule passed a
  name through whenever it happened to decode, which is undecidable from
  the name alone — `pen&ink-notes` decodes (to "pen詹notes") and
  `fish&chips-2024` does not, and no user can tell which without doing
  base64 by hand. A name that would have decoded gets a note on stderr
  naming `--wire`. Listing decodes the other way:
  `r&AOk-gie ("régie", modified UTF-7)` in text output, raw in JSON.
- **Comparison is on the decoded text, folding ASCII case only.**
  `r&AOk-gie` ("régie") and `r&aok-gie` ("r檉gie") stay apart, because
  base64 is case-sensitive and those are two words. `R&-D` and `r&-d`
  fold, because they are two spellings of one ASCII word — keying the
  rule on "contains `&`" got that wrong. `é`/`É` stay apart: IMAP asks
  for no Unicode case folding, so neither does this.
- A keyword carrying an invisible character — NBSP, BOM, zero-width
  space, soft hyphen — is refused. They pass the atom check, being
  neither control nor ASCII space, and would make a keyword no listing
  can tell from its twin.
- Neither RFC 3501/9051 §2.3.2 nor RFC 5788 says in so many words that
  keywords are case-insensitive; the evidence is RFC 8621 §4.1.1
  ("Keywords are shared with IMAP", and JMAP servers "MUST return
  keywords in lowercase") and Thunderbird's own traffic, which carries
  both `$label1` and `$Label1` for one tag. The encoded case above is
  where that rule provably stops.
- The atom charset follows RFC 3501 §9 exactly: everything but
  `( ) { % * " \ ]`, space and control characters. A comma is legal and
  must be, since modified UTF-7 writes `,` inside its base64.
- The message selection is resolved to per-folder UID groups the same
  way as every other message command (see [From a command line to
  per-folder UID groups](#from-a-command-line-to-per-folder-uid-groups));
  unknown UIDs are rejected server-side (or by the mock).
- The real backend sends `UID STORE <uids> +FLAGS (…)` / `-FLAGS (…)`
  once per batch of 50 UIDs (removals first), with the same
  reconnect-once recovery used by fetches. Persistence of flag changes
  follows the server (`CHANGES=/PERMANENTFLAGS` behaviour is not forced).
- `flag list <SELECTION...>` reads them back with a light
  `UID FETCH <uid> (UID FLAGS)` per message; `\Recent` is filtered out so
  the output matches the `flags` field of search results.
  `tag list <SELECTION...>` shares the same path with an extra filter
  that keeps only the keywords (drops every `\`-prefixed system flag).

### From a command line to per-folder UID groups

Commands that take messages (`read`, `thread`, `part list`, `part save`,
`flag`, `tag`) accept one or more **message selections**, and every
command accepts `-f`/`-A` to name the folders it works on. Parsing and
resolving both — the selection grammar and the folder-pattern matching —
live in `src/cli/select.rs`, with their own unit tests; `resolve_groups`
and `default_folder` (`src/cli/mod.rs`) turn the parsed selections into
the per-folder UID groups every command operates on:

```text
  mail-imap  -f INBOX  -f 'Archive/*'    read  12345  Archive/2026::1-5
             └──────────┬───────────┘          └──────────┬───────────┘
                folder patterns                   message selections
                        │                                 │
                        ▼                                 ▼
         ┌────────────────────────────┐     ┌──────────────────────────┐
         │ expand * and % against     │     │ parse [FOLDER::]UIDS;    │
         │ the LIST reply             │     │ ranges stay symbolic     │
         └──────────────┬─────────────┘     └─────────────┬────────────┘
                        │                                 │
    the default folder  │                                 │  the folder named
    (a bare UID with    │                                 │  in the token
    several is refused) │                                 │
                        └────────────────┬────────────────┘
                                         ▼
                         ┌───────────────────────────────┐
                         │ per-folder UID groups         │
                         │   INBOX         12345         │
                         │   Archive/2026  1,2,5         │
                         └───────────────┬───────────────┘
                                         ▼
                          one SELECT + fetch per group
```

A selection is `[FOLDER::]UIDSPEC`: `5`, `1,4,7` (a list; separate CLI
arguments work the same way — `read 1 4 7`), `1-9` or `1:9` (a UID
range), `*` (every message), `9-*` or `9-` (from 9 to the end, the
second form needing no shell quoting), `Archive::1-5`
(folder-qualified). `parse_selection` (`src/cli/select.rs`) first tries
the whole token as a UID spec, which is what settles the IMAP forms
carrying a `:` of their own (`1:5`, `9:`); only a token that fails that
is split at its *last* `:` into folder and spec, so a folder name that
is itself a UID spec cannot be written this way. A range with no lower
end (`-20`) is refused: clap reads it as an option before the parser
sees it, and `*-20` already means 20 to the end.

`--last N` / `--first N` (`UidItem::Last`/`First`) are a count rather
than an interval, and are a flag rather than a selection token on
purpose: `last:20` would be split by the `FOLDER:UIDS` rule into a
folder called `last`. Because they name no UID they are unambiguous
across folders — `selection_groups` expands them into one selection per
selected folder — and they are refused alongside an explicit selection.
They count by UID, which ascends with arrival; ordering by `Date:` is
`-S`'s job.

A range or `*` is symbolic (`UidItem::Range`/`From`/`All`) until
`Selection::resolve` matches it against the folder's real UID list, and
an item matching nothing is an error — a selection never quietly
resolves to fewer messages than it names. A UID named explicitly
(`UidItem::One`) is not checked there: for the read-only commands the
server reports it better than we could. `change_flags` does check,
because RFC 3501 §6.4.8 has `UID STORE` ignore a non-existent UID
without an error, so an unchecked typo would be reported as a
successful mutation. `resolve_groups` groups
selections by folder (first-named order), fetches each group's UID list
at most once — one `UID SEARCH ALL` per folder, and only when a
selection of that folder holds a range or `*` — resolves every
selection of the group against it, and deduplicates while preserving
input order. A selection with no folder of its own takes the folder `-f`/
`-A` selected; `default_folder` refuses to guess when more than one
folder is selected, naming them in the error instead.

**Deliberate deviation from IMAP:** RFC 3501 folds an open range like
`559:*` onto the last message when 559 is past the end of the mailbox.
`Selection::resolve` does not: an open range or `*` matches only UIDs
present in the fetched list, so a range past the end resolves to nothing
and `resolve_groups` errors rather than silently operating on zero
messages picked by the server's own fold. This matters most for a
mutation: `flag add 9999-* -- '\Deleted'` must not fall through to the
newest message just because 9999 does not exist.

Folder wildcards (`*`, `%`) are refused inside a selection's folder part
(`Arch*:5` is an error) — `-f` is how several folders are reached.
`flag`/`tag` `add`/`remove` still take the flag/tag names as a trailing
list after `--` (clap allows only one variadic positional), so the
shape is `flag add 1 4 -- '\Seen'`; a name starting with `-` is rejected
with a hint, since it is usually a global option swallowed after `--`.
`part save` resolves its selection like any other command but then
requires exactly one `(folder, uid)` pair, erroring otherwise.

`-f`/`--folder` is repeatable and comma-separated, and accepts IMAP
`LIST` patterns (`*` crosses the hierarchy delimiter, `%` does not);
`folder_matches`/`expand_folders` (`src/cli/select.rs`) match patterns
against the mailbox list fetched by one `LIST "" *`, only when a pattern
is actually given (a literal name skips the `LIST` round trip and is
passed through even if the server never mentioned it). `-A`/
`--all-folders` is `-f '*'`. An empty folder name (`-f ''`) is refused.
With no `-f`/`-A`, the default is `Config::folder` (`INBOX`), except for
`count`, which defaults to every selectable mailbox.

### MIME parsing (`mime.rs`)

`part` works on the raw RFC822 bytes: headers are unfolded and parsed
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
extra part metadata (UID 3: spreadsheet, UID 5: PDF); `part save` writes a
deterministic placeholder file whose size matches the part reported by
`part list`. It is what the unit tests drive. Sorting (`-S`) is applied
to its fixed message set with the same client-side comparator the real
backend uses as fallback. `flag`/`tag` mutate an in-memory per-UID
keyword set (`message_flags`), which shows up in the `flags` of subsequent
mock `search` results.

## Testing

`make tests` (or plain `cargo test`) runs the suite entirely against the
mock backend (no network). Coverage:
- backend selection (`mock` vs `real`)
- list / search / read / count / uid / unread / part happy + error paths
- sort spec parsing (keys, `-` reverse, invalid input) and client-side
  result sorting (single/multi key, descending, capped selection)
- flag/tag name validation (system flags normalized, `\Recent` rejected,
  tag mode refuses `\` flags, keyword charset) and mock store/retrieve
  (incl. `flag list` via `message_flags`)
- message-selection grammar (lists, ranges in both notations, `*`/`n-*`,
  folder-qualified tokens, the last-`:` split rule, UID 0 and wildcard
  rejection, resolution against a folder's UID list) and folder-pattern
  matching/expansion (`*`/`%`, INBOX case-insensitivity, dedup, a
  pattern matching nothing) — `src/cli/select.rs`
- MIME parser (plain, multipart, nested multipart, base64 / quoted-printable /
  binary decoding, missing boundary)
- RFC 2047 decoder (plain, Q, Q-with-underscore, B, Latin-1, mixed text)
- the real backend returns an error (no fabricated data) when unreachable

To exercise the real backend manually:

```bash
cargo run --release -- -c incal.conf folder
cargo run --release -- -c incal.conf -f INBOX search "SINCE 01-Jan-2026"
```

## Output

Commands print human-readable text by default. With `-j`/`--json` each command
prints a single compact JSON object to stdout instead; errors become
`{"error": "..."}` on stderr (exit code unchanged). JSON shapes:

| Command                                               | Shape                                                                                                |
| ----------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `folder`                                              | `{"count", "folders": [FolderInfo]}`                                                                 |
| `search`                                              | `{"folder", "query", "count", "results": [SearchResult]}`                                            |
| `read`                                                | one `{"folder", "uid", "content"}` per selected UID                                                  |
| `count` / `status`                                    | `{"all", "counts": [Mailbox]}`                                                                       |
| `uid`                                                 | one `{"folder", "count", "uids": [u32]}` per selected folder                                         |
| `thread`                                              | one `{"folder", "uid", "count", "uids": [u32]}` per selected message                                 |
| `unread`                                              | same as `search` (query `UNSEEN`)                                                                    |
| `part list`                                           | one `{"folder", "uid", "count", "parts": [PartInfo]}` per selected UID                               |
| `part save`                                           | `{"folder", "uid", "part", "file", "size"}`                                                          |
| `flag list` / `tag list`                              | one `{"folder", "uid", "count", "flags": [String]}` per selected UID                                 |
| `flag add` / `flag remove` / `tag add` / `tag remove` | one `{"folder", "count", "uids": [u32], "added": [String], "removed": [String]}` per selected folder |

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
