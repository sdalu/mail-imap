# mail-imap design

Why mail-imap is shaped the way it is, and what it puts on the wire:
the two backends, the IMAP commands each operation sends, the
fallbacks when a server or its responses fall short, and how a command
line becomes per-folder UID groups.

## Backends

Operations are exposed through the `ImapBackend` trait (`src/imap/mod.rs`).
Two implementations exist and are selected by `Config::mock`:

| Backend      | File               | Used by                                                     |
| ------------ | ------------------ | ----------------------------------------------------------- |
| `RealClient` | `src/imap/real.rs` | The released binary (real servers)                           |
| `MockClient` | `src/imap/mock.rs` | Development builds only — the suite, and `check-examples.sh` |

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
| `info`                                                | `CAPABILITY` + `LIST "" *`; no mailbox is selected and nothing is changed                                                                                                                                                                                                                                                           |
| `folder list`                                         | `LIST "" *` (non-`\Noselect` names)                                                                                                                                                                                                                                                                                                 |
| `folder create`                                       | `CREATE <name>`, or `CREATE "<name>" (USE (<attr>))` (RFC 6154) when `--use` is given — and only when `CREATE-SPECIAL-USE` is advertised, else refused before anything is sent. `--use` takes a bare word (`archive`), or the atom with `--wire`                                                                                    |
| `folder rename`                                       | `RENAME <from> <to>` (INBOX refused outright, at every access level)                                                                                                                                                                                                                                                                |
| `folder subscribe` / `folder unsubscribe`             | `SUBSCRIBE <name>` / `UNSUBSCRIBE <name>`                                                                                                                                                                                                                                                                                           |
| `search` / `unread`                                   | per folder: `SELECT` + (`UID SORT <crit> UTF-8 <query>` when `--sort` is given and `SORT` is advertised, else `UID SEARCH <query>`) + batched `UID FETCH` (envelope/flags/date/size/`BODYSTRUCTURE`); folders are searched in order and the results aggregated                                                                      |
| `read`                                                | `SELECT` + `UID FETCH <uid> (UID FLAGS INTERNALDATE BODY.PEEK[])` — no `ENVELOPE`: the headers are read from the message itself                                                                                                                                                                                                                                                          |
| `count` / `status`                                    | `LIST "" *` + `STATUS <folder> (MESSAGES UNSEEN RECENT UIDNEXT UIDVALIDITY)` per mailbox (or one folder when given)                                                                                                                                                                                                                 |
| `uid`                                                 | per selected folder: `SELECT` + `UID SEARCH ALL`                                                                                                                                                                                                                                                                                    |
| `thread`                                              | per selected message: `SELECT` + `CAPABILITY`; if `THREAD=REFERENCES` is advertised: one `UID THREAD REFERENCES UTF-8 ALL` (server-side tree, flattened). Otherwise: `UID SEARCH ALL` + batched `UID FETCH <uids> (UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID REFERENCES IN-REPLY-TO)])`, then client-side union-find over Message-IDs |
| `unread`                                              | `SELECT` + `UID SEARCH UNSEEN` + batched `UID FETCH` (same path as `search`)                                                                                                                                                                                                                                                        |
| `part list`                                           | per selected message: `SELECT` + `UID FETCH <uid> (UID BODY.PEEK[])`, then MIME part enumeration locally                                                                                                                                                                                                                            |
| `part save`                                           | same fetch, then the selected part is CTE-decoded and written to a file                                                                                                                                                                                                                                                             |
| `flag list` / `tag list`                              | per selected message: `SELECT` + `UID FETCH <uid> (UID FLAGS)`                                                                                                                                                                                                                                                                      |
| `move`                                                | per selected folder: `SELECT` + `UID MOVE <uids> <target>` (RFC 6851) when `MOVE` is advertised, else `UID COPY` + `UID STORE +FLAGS (\Deleted)` + `UID EXPUNGE` (RFC 4315); a server with neither is refused                                                                                                                       |
| `flag add` / `flag remove` / `tag add` / `tag remove` | per selected folder: `SELECT` + `UID STORE <uids> +FLAGS (...)` / `-FLAGS (...)` per batch of 50                                                                                                                                                                                                                                    |

IMAP can only search the selected mailbox, so `search`/`unread` iterate over
the folders selected by `-f`/`-A` (repeatable, comma-separated, and IMAP
`LIST` patterns; default the `-f`/config folder) on one connection. Results are
grouped per folder, ordered by the `-S`/`--sort` criteria (default:
most-recent first within each folder), and the total result count is capped
by `Config::max` (default `0`, no cap; overridable per run with `-M/--max`,
`0` = unlimited). Each result carries the `folder` it came from; the JSON
output uses `"folder"` for a single folder and `"folders"` for several.

`read`, `thread`, `part list`/`part save`, `flag` and `tag` take message
*selections* rather than bare UIDs; each selection resolves to one or more
`(folder, uid)` pairs before any of the operations above run — see
[From a command line to per-folder UID groups](#from-a-command-line-to-per-folder-uid-groups).

### What `info` reports

`info` exists because everything else in this tool assumes the caller
already knows three things: what it is allowed to change, how to spell
a folder path on this account, and which of the degradation ladders
below this server will land on. An agent that has to discover those by
trying commands and reading refusals is an agent that has already
changed something by accident.

It costs `CAPABILITY` and one `LIST`, selects no mailbox, and changes
nothing.

- **The hierarchy delimiter is per mailbox on the wire** — every `LIST`
  response carries its own, and it may be `NIL` — and per *namespace*
  in practice (RFC 2342). So `info` reports every distinct delimiter
  the list showed, and names the one INBOX uses as the one to build a
  path with. More than one means more than one namespace, which is
  worth saying rather than averaging away.
- **`delimiter` in the config overrides it, and is advisory only.**
  Nothing in the tool rewrites a folder name — names go to the server
  as they are typed — so the field exists to be *reported*, for an
  account whose `LIST` says `NIL` or lies. When it disagrees with what
  the server said, `info` prints both: silently preferring one would
  hide exactly the case the field is for.
- **Capabilities are reported as the path taken, not as names.**
  `MOVE`/`UIDPLUS` become `moving mail: UID MOVE | UID COPY + UID
  EXPUNGE | refused`, `SORT` and `THREAD=REFERENCES` become
  `server-side`/`client-side`. The raw list is printed too, but the
  derived line is the one a caller can act on, and it is derived by the
  same code the operations use.
- **Two things are deliberately not in it.** `PERMANENTFLAGS` would
  need a mailbox selected, and `SELECT` is not free of side effects
  (it clears `\Recent`) — so what a mailbox will keep is still found
  out by `flag`/`tag`, which refuse rather than lie. `NAMESPACE`
  (RFC 2342) is not sent either; the delimiters seen in `LIST` are the
  evidence `info` has, and it says so rather than implying it asked.

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
`Error::Bye` *and* leaves the stream desynced, which aborts a whole search
with "Bye Response: no explanation given" unless it is caught. `RealClient`
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

### Declaring a charset on `SEARCH`

RFC 3501 lets a `SEARCH` carry an optional `CHARSET` before its keys,
and lets a server refuse a search containing non-ASCII bytes when none
was declared. So `uid_search_charset` (`real.rs`) is one more ladder:

- a **pure-ASCII query goes on the wire byte for byte**, with no prefix.
  This is the whole compatibility story — a server that dislikes
  `CHARSET` never sees one unless the search actually needs it, so
  nothing that worked before can start failing;
- a query carrying any non-ASCII byte is sent as
  `UID SEARCH CHARSET UTF-8 <query>`;
- if that comes back `BAD` or `NO`, it is retried once without the
  prefix. A server refusing the charset is not a failed search; it is a
  server that can only do ASCII, and the bare query is the best it can
  be asked.

`-d` names which form went out, and says so when the fallback fires.

**The fallback branch is covered by no test, and that is worth knowing
rather than discovering.** GreenMail accepts `CHARSET UTF-8`, so
`tests/wire.rs` never drives the retry; and the branch needs a live
session, so no unit test reaches it either. What the wire test does
prove is the half that can be proven against this server: the declared
form is accepted, not refused. GreenMail also does not *match* a
non-ASCII byte in a header — the message is demonstrably there, found
by the ASCII half of its own subject, and the accented search returns
nothing — so the matching half is unproven here too. The test says all
of this in its own comments rather than asserting something weaker and
looking complete.

### Partial results, and the rule that was applied unevenly

"Partial results beat none at all" is stated once in DESIGN and
implemented in three places, and until a review went looking it was
implemented in only one of them. The pattern that breaks it is always
the same: a `?` (or a bare `return Err`) inside a loop that is already
accumulating into a local `Vec`, which unwinds the function and takes
the accumulator with it.

- `search_in_folder` had it: a chunk that fails reports what earlier
  chunks found, with a warning.
- `search_folders` did not. One mailbox that cannot be selected —
  deleted or renamed by another client since the folder list was taken,
  which `-f '*'` makes ordinary — discarded every hit from every folder
  searched before it.
- `fetch_chunk`'s last-resort per-message walk did not. A hard failure
  on the twentieth UID threw away the nineteen already recovered, and
  if that was the folder's first chunk the caller's own partial-result
  guard then saw an empty list and reported a bare error.

All three now keep what they have and say what stopped them. The rule
is worth stating as a rule rather than a habit: **an accumulator in
scope is a reason not to use `?`.**

### Telling a stalled link from corrupt data

`attempt_fetch` reconnects once and retries. When the retry fails on a
*fresh* connection, the old code concluded the bytes themselves were at
fault and returned `Unparseable`. That was an assumption, and
`config.timeout` turned it into a wrong one: a read timeout arrives as
`imap::Error::Io`, which `is_poisoned` counts, so an ordinary slow
server was reported as "server response could not be parsed" — sending
someone to inspect a message for corruption when the answer was their
network or the `timeout` setting. Worse, the second error was dropped
entirely, so even `-d` could not recover it.

An `Io` failure on the fresh connection is now a hard error carrying
its own cause. Everything else that poisons twice really is the data,
and says so under `-d`.

### Access level

`Config::access` (`access-level` in the config file) is an ordered
ladder — `readonly` < `organize` < `restructure` < `full` — and the
command line may
only narrow it, never widen it, so a config that says `readonly` cannot
be argued out of it by an argument list.

Where the line falls between `restructure` and `full` is worth stating,
because `folder delete` looks like it belongs with the rest of the
folder tree and does not: creating, renaming and subscribing lose
nothing, and deleting loses a mailbox and everything in it. So
`check_folder_delete` is a separate gate from `check_folder_change`,
and it asks `AccessLevel::may_delete_folder` rather than comparing
levels itself — which is what keeps the gate and the `delete a folder`
line `info` prints from drifting into a tool that refuses what it
advertises.

Deleting also refuses two things no access level can authorise: `INBOX`,
which every server refuses anyway, and a mailbox that still holds
messages unless `--force` is given. The second is this tool's, not the
protocol's — a server deletes a full mailbox without comment, and the
refusal exists so that destroying mail takes a word that says so.

It is checked in `ImapClient`, the wrapper both backends go through,
and not in the CLI handlers. A handler can forget; a rule that lives in
one place cannot be forgotten by nine. `ImapClient::check_flag_change`
runs before `store_flags` reaches either backend, so the mock is held
to the same rule as the server and the refusals are testable offline.

Two asymmetries are deliberate:

- **Clearing is freer than setting.** Above `readonly` any flag may be
  cleared, including `\Deleted`: taking a flag off a message loses an
  annotation, never a message, and un-deleting is a rescue.
- **`\Deleted` is the only flag `organize` refuses to set.** `\Draft`
  is permitted — it marks a composition, it cannot cost anything.

The two rungs above `readonly` draw different lines. `organize` is
about messages: nothing is lost, and the folder tree is left exactly as
it was found — it moves mail into folders that exist, it does not make
them. `restructure` is about the tree: `create_folder`,
`rename_folder` and `set_subscribed` are gated by
`check_folder_change`, and still nothing is lost, since deleting a
mailbox would lose every message in it and belongs to `full`.

Two things sit outside the ladder on purpose:

- **Renaming INBOX is refused at every level.** RFC 3501 §6.3.5 gives
  it a special meaning — the server moves every message out into the
  new mailbox and leaves INBOX empty. Nothing is destroyed, but nobody
  means it by "rename", so it is an outright refusal rather than a rung.
- **A special-use attribute (RFC 6154) can only be declared at
  creation**, and only when the server advertises `CREATE-SPECIAL-USE`;
  `create_folder` checks the capability and says so rather than sending
  a command the server will reject. `--use` is spelled the way `flag`
  spells its names — a bare word (`archive`), with `--wire` for the
  atom a listing prints (`\Archive`) — and resolved against the seven
  RFC 6154 attributes before the connection is opened. That the set is
  closed is also what keeps an arbitrary string out of the
  `CREATE ... (USE (...))` command line, which is interpolated. The
  LIST attributes a server maintains itself — `\Noselect`,
  `\HasChildren`, `\Marked` — are not settable by any client and are
  not offered.

### Moving mail (`move`)

`UID MOVE` (RFC 6851) is one command and the server does the rest.
Without it the sequence is `UID COPY`, then `\Deleted` on the
originals, then `UID EXPUNGE` (RFC 4315) — **copy first**, so a failure
part-way leaves the messages in both places rather than in neither.

A server advertising neither `MOVE` nor `UIDPLUS` is refused rather
than served, because the only route left ends in a plain `EXPUNGE`,
which removes *every* message marked `\Deleted` in that mailbox — quite
possibly ones somebody else marked. Losing a stranger's mail to file
one of yours is not a trade this tool makes.

The fallback sets `\Deleted`, which `organize` refuses to set as a flag
operation. That is not a loophole: the gate is on the *operation*, and
`move`'s guarantee — the message still exists, in a different place —
holds because the copy is made first. What `organize` forbids is
marking a message for removal with nothing else to show for it.

The folder is the **last argument**, as `mv` has it: `move 1-5 Archive`.
clap cannot split a greedy variadic from a trailing required positional,
so the command takes one variadic and splits it itself.

That shape carries `mv`'s own hazard — `move 1 2` meaning two UIDs and
a forgotten folder — and needs no guard against it, because both ways
of forgetting already fail loudly: `move 1-5` leaves nothing to select,
and `move 1 2` is refused by the backend unless a mailbox really is
called `2`. A second spelling of the target (`--to`) would buy nothing
and cost a second way to write one thing.

The target mailbox must already exist: creating one is `restructure`'s
business, and doing it implicitly would let `organize` change the
folder tree by a side door. Moving to the folder the messages are
already in is refused as a no-op.

### Copying (`copy`) and expunging (`expunge`)

`copy` is the easy half: `UID COPY`, `organize`, the `mv` argument
order, and the same must-already-exist refusal `move` uses. It is not
`move` minus a step in one respect — **a target equal to the source
folder is allowed**, where `move` refuses it. The two refusals would
mean different things: moving mail into the folder it is already in
does nothing, so refusing catches a mistake; copying it there produces
a real second copy, which IMAP permits and this tool has no business
overruling.

`expunge` is the only command here that destroys mail, and two rules
keep it narrow.

**It removes only what is already marked, and never marks anything
itself.** `expunge_messages` fetches `(UID FLAGS)` for the named UIDs
first (`deleted_among`), expunges the subset carrying `\Deleted`, and
returns that subset — so the count reported is what went, not what was
asked for. If none of them is marked it refuses and names
`flag add <selection> deleted`. Marking and expunging in one step
would read as a convenience and would destroy mail the caller never
asked to lose; separating them means the destructive step can only ever
act on a decision already recorded on the server.

**It requires `UIDPLUS` and refuses without it.** `UID EXPUNGE`
(RFC 4315) is the only way to name which messages go. The alternative
is a plain `EXPUNGE`, which removes every `\Deleted` message in the
mailbox — including ones another client marked, possibly years ago —
and that is the same unbounded action `move_messages` already refuses
on a server without UIDPLUS. A command offering it would contradict
that refusal, so `info` reports which of the two the server can do on
its `expunging` line and `expunge` refuses rather than degrading.

For the same reason there is no form that expunges a mailbox's whole
`\Deleted` set. It composes instead: `search DELETED` gives the UIDs,
`expunge` takes them, and the unbounded version is never a keystroke
away from the bounded one.

### Putting a message in (`append`)

The one direction that did not exist. `APPEND` takes a mailbox, a
literal, and optionally flags and an internaldate; `ImapClient` gates it
on `AccessLevel::may_append` (`full`) and does two things to the content
before either backend sees it, so that a wire test calling the trait
directly exercises what the CLI gets.

**A lone LF is normalised to CRLF.** An IMAP literal is a byte count
followed by exactly that many bytes, and the protocol's lines end CRLF.
A message file written on a Unix host ends its lines with LF alone, so
appending it verbatim stores a message whose lines are unterminated by
the protocol's reckoning — and nothing local will say so, because every
local check is reading the same bytes it wrote. This is why the test for
it is a *wire* test (`a_lone_lf_is_normalized_to_crlf_before_appending`)
that reads the message back over a raw socket and inspects the bytes,
rather than a unit test asserting the normaliser against itself.

**The content is checked for an RFC 5322 shape** — a header block and a
blank line — and refused here if it has none. A server refuses garbage
too, in language considerably less useful.

`windows-1252` is decoded as itself rather than as ISO-8859-1. The two
agree everywhere except `0x80`–`0x9F`, where Latin-1 has C1 controls
and cp1252 has the punctuation Outlook actually emits — curly quotes,
dashes, the ellipsis. Aliasing them turns an ordinary message's
quotation marks into control characters, which reads as the sender's
fault rather than the reader's.

`--date` is ISO 8601 and sets the internaldate. Without it, nothing is
sent and the server's own default (the moment of the append) applies.
It is deliberately not read from the message's `Date:` header: that is
the sender's clock and records when the message was *written*, while
internaldate records when this mailbox *received* it. They differ by
however long the message sat somewhere else, and silently substituting
one for the other misdates every message an import brings in.

`--flag` goes through the same `parse_flag_names` the `flag` command
uses, so the spellings cannot drift apart. That parser refuses an empty
list, which is right for `flag add` and wrong here — `append` with no
flags is ordinary — so the handler calls it only when names were given.
The refusal is unreachable from the other call sites, which guard
earlier in `split_args`; it is a precondition of those commands rather
than a fact about parsing names, and it would read better at the call
sites that need it.

### Stripping a part (`part strip`)

The only operation here that destroys something *inside* a message,
and the only one that changes a UID.

**IMAP has no edit.** A message on a server is immutable, so the
sequence is: fetch it whole, rebuild it without the named parts
(`mime::strip_parts`), `APPEND` the result, mark the original
`\Deleted`, `UID EXPUNGE` it. `ImapClient::strip_part` composes that
out of trait methods the backends already implement rather than adding
a backend method of its own, which is also why the mock gets it without
knowing anything about it.

**Write first, delete last.** If the append succeeds and the removal
does not, the mailbox holds two copies and a person can choose between
them; the other order loses the message whenever the rebuild is wrong.
`move_messages` files its fallback the same way for the same reason,
and the errors on the second half say explicitly that both copies
exist, rather than reading like the strip did not happen.

**UIDPLUS is checked before anything is written**, not when the removal
is reached. The original goes with `UID EXPUNGE`; discovering at the
last step that the server cannot do it would mean having already
appended a copy that nothing then cleans up. The question is asked as
`can_expunge_by_uid` on the backend rather than read off
`capabilities()`, so each backend answers for itself — the mock
advertises no extensions and can still remove exactly the messages it
is handed, and refusing there would be refusing something that works.

**The copy carries the original's flags and internaldate.** Both would
be lost silently otherwise: the mail is still present, merely unread
again, or dated the day it was tidied. An archive dated by when it was
tidied has no history left.

**The rebuilt message is appended exactly as built.** `strip_part` uses
`ImapClient::append_exact`, not the `append_message` the `append`
command uses, and the difference is the whole byte-for-byte promise.
`append_message` normalises lone LFs to CRLF, which is right for a file
read off this host and wrong for bytes that came from a server: a
surviving part may legitimately carry a bare LF inside its own body,
and normalising it rewrites content nobody asked to touch. Composing
the two was the obvious thing to do and it was wrong — the promise was
false for several commits before a review caught it, and the wire test
that now guards it uses a survivor with embedded bare LFs, because the
earlier single-line fixture could not tell the bug from the fix.

**The part is replaced, not removed**, which is what keeps part numbers
stable — a `part list` taken before the strip still describes the
message. The stub names the file, its type, its decoded size and its
SHA-256, and the message gains one `X-Mail-Imap-Stripped` header per
part. The digest is of the *decoded* bytes, for two reasons: that is
the form a saved file is in, which is the whole point of being able to
match a kept attachment against the record; and the encoded form is
not stable, since a gateway may re-wrap base64 at a different line
length without changing the attachment at all.

One consequence worth stating because it surprises: **stripping a small
part makes the message bigger.** The stub and a 64-character digest
outweigh a few hundred bytes of attachment. That is correct, and it is
not what the command is for — the wire test uses a 4000-byte
attachment precisely so that it asserts the space is actually
reclaimed, which a toy fixture would have quietly failed to do.

### Reading a message (`read`)

`read` used to print a short header summary and then the raw
`BODY.PEEK[]` — which for a multipart message is boundary lines and
base64, the least readable thing this tool produced, from the command
an agent reaches for first.

**The rendering lives in one place, and it is not a backend.**
`ImapClient::read_message` fetches the raw message (`fetch_raw_message`)
and hands it to `mime::render_message`; `ImapBackend::get_email` is
gone. Before, each backend rendered its own summary, so the mock and a
real server produced their text by different code and could drift apart
without anything noticing. Now there is one renderer and the backends
only supply bytes.

Dropping `ENVELOPE` from the fetch came with that, and is a gain rather
than a compromise: the old path asked the server's ENVELOPE for the
headers and carried a fallback ladder for servers whose ENVELOPE the
parser could not read. Reading the headers out of the message never
needs that question asked.

What it shows, in order of preference: the `text/plain` leaf; else the
`text/html` leaf with its tags stripped; else a note naming what the
message holds and pointing at `part list` and `part save` — never a
wall of base64.

**A part that will not decode does not take the message with it.** The
attachment list needs each leaf's size, and asking for it used to mean
decoding every leaf — so one attachment whose declared encoding did not
match its bytes failed the whole render, and `read` showed nothing at
all, including the text part already decoded and in hand. Real mail is
full of mislabelled attachments; the size now falls back to what the
part occupies as it stands. `part list` had the same defect and the
same fix.

**The HTML stripper tracks quotes**, which is the one piece of
tokenising it cannot do without: a `>` inside a quoted attribute would
otherwise end the tag early and emit the rest as body text, so
`<div onclick="if(x>5)return;">Hello` rendered as `5)return;">Hello`.
Text the message never contained is a worse failure than text dropped,
and it is the failure a reader cannot detect. Every other leaf is listed afterwards as an attachment
line, so one command says what is in the message.

**The HTML stripper is deliberately poor.** Tags out, half a dozen
entities decoded, blank lines collapsed. It is not a renderer, it must
not grow into one, and the comment above it says so — the failure mode
here is a well-meaning improvement that turns a fallback into a
half-finished browser.

`--raw` restores the previous behaviour: the header summary, then the
body verbatim, and no attachment list. One difference from the old
output is worth stating rather than implying byte-identity — the
summary's `From`/`To`/`Subject`/`Date` are now read from the message's
own headers rather than reconstructed from the server's `ENVELOPE`, so
an address the server re-spelled will be spelled the way the message
spells it. The body under `--raw` is byte for byte what it always was.

JSON follows the same mode rather than a different one, per the rule in
*Output* that text may show less than JSON but never something
different: `content` is the same body the text shows, and a `source`
field (`text`, `html`, `raw`, `none`) says which leaf it came from, so
a caller knows what it was given.

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

- `flag add|remove <SELECTION...> <FLAG...>` accepts only the five
  IMAP-defined system flags, written as bare words — `seen`,
  `answered`, `flagged`, `deleted`, `draft`, case-free. The command owns
  exactly those five, so they need no sigil to be unambiguous, and a
  bare word needs none of the shell quoting `'\Seen'` does. `--wire`
  takes the form a listing prints (`\Seen`), for a name copied back out
  of one — the same bargain `tag --wire` makes. `recent` is rejected: it
  is maintained by the server and cannot be set by clients. A
  user-defined keyword is refused with a pointer to `tag`: the two
  commands own one kind of name each, which is what keeps a stray word —
  `flag add 5 deleted Trash` — from being stored as the keyword
  `Trash`.
- `tag add|remove <SELECTION...> <TAG...>` accepts only plain
  keywords. A `\`-prefixed name is refused with a hint to use `flag`,
  and so is a keyword spelled like a system flag with the backslash
  dropped (`Deleted`): stored as a keyword it is inert, reads like the
  flag in a listing, and slips past the access level that governs the
  real one.
- Keyword names are matched against the IANA *IMAP/JMAP Keywords*
  registry, transcribed into `src/cli/keywords.rs` (fetched 2026-09-22).
  The registry carries no description of its own — its columns are
  Keyword, Type, Usage, Scope, Comments (empty throughout) and
  Reference — so the one-line meaning beside each entry comes from the
  RFC that registers it (RFC 9979 defines seventeen of the twenty-five),
  quoted or trimmed to fit. The three whose reference is a person rather
  than an RFC have no normative text at all; theirs say what clients use
  them for. The registry's *Scope* column is why the five RFC 8621
  entries are excluded: it marks them JMAP-only.
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
  `tag add 5 régie` sends `r&AOk-gie`; sending the raw bytes would be
  a malformed command, not a nicety. Names are NFC-composed first
  (`unicode-normalization`): "régie" typed on a system that hands over
  `e` + U+0301 would otherwise encode to a different atom that looks
  identical in every listing, and a later `tag remove` spelled the other
  way would silently miss it. `--wire` composes nothing, being verbatim
  by definition. **Input is literal text and every
  `&` is escaped to `&-`**; `--wire` sends the names verbatim, which is
  how a key copied out of a listing goes back. Taking a name for a wire
  key whenever it happens to decode is undecidable from the name alone —
  `pen&ink-notes` decodes (to "pen詹notes") and `fish&chips-2024` does
  not, and no user can tell which without doing base64 by hand — so a
  name that *would* have decoded gets a note on stderr naming `--wire`
  rather than being taken for one. Listing decodes the other way:
  `r&AOk-gie ("régie", modified UTF-7)` in text output, raw in JSON.
- **Comparison is on the decoded text, folding ASCII case only.**
  `r&AOk-gie` ("régie") and `r&aok-gie` ("r檉gie") stay apart, because
  base64 is case-sensitive and those are two words. `R&-D` and `r&-d`
  fold, because they are two spellings of one ASCII word — which is why
  the rule keys on the decoded text and not on "contains `&`". `é`/`É`
  stay apart: IMAP asks for no Unicode case folding, so neither does
  this.
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
arguments work the same way — `read 1 4 7`), `1-9` (a UID
range), `9-` (from 9 to the end of the mailbox), `*` (every message,
and never a range endpoint), `last:20` / `first:5` (a count),
`Archive::1-5`
(folder-qualified). `parse_selection` (`src/cli/select.rs`) splits a
token at its last `::`; a token without one is wholly a UID spec, so a
folder whose name is itself a UID spec cannot be written this way and
needs `-f`.

Three forms IMAP itself allows are deliberately refused, each because
one concept with two spellings is what makes a grammar ambiguous:

- **`4:7`** — IMAP's range operator. Here `:` belongs to `last:` and
  `first:` alone; a range is `4-7`. Accepting both would put `:` back
  to doing two jobs, which is the problem `::` was introduced to solve.
- **`9-*` and `*-9`** — `*` means every message and is only ever that,
  never a range endpoint. "From 9 to the end of the mailbox" is `9-`.
  A `*` that is not alone in its item reads as a folder wildcard to
  anyone skimming, and IMAP's `*:9` reads as "up to 9" to half its
  readers when it means the opposite.
- **`-20`** — a range with no lower end. clap reads it as an option
  before the parser sees it, and `first:20` says it properly.

`last:N` / `first:N` (`UidItem::Last`/`First`) are a count rather than
an interval. They are spellable as tokens precisely because `::` binds
the folder: that frees the single `:` for a `key:value` item, which is
what a flag would otherwise have had to work around. Because a count
names no UID it is unambiguous across folders, so an *unqualified*
count is expanded by `selection_groups` into one selection per selected
folder, while `Archive::last:5` asks a count of one named folder — and
a single run can ask different counts of different folders, which the
flag never could. Counting is by UID, which ascends with arrival;
ordering by `Date:` is `-S`'s job.

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
mutation: `flag add 9999- deleted` must not fall through to the
newest message just because 9999 does not exist.

Folder wildcards (`*`, `%`) are refused inside a selection's folder part
(`Arch*::5` is an error) — `-f` is how several folders are reached.
`part save` resolves its selection like any other command but then
requires exactly one `(folder, uid)` pair, erroring otherwise.

### Junk, and PERMANENTFLAGS

Five keyword spellings carry two meanings (`keywords::JUNK`,
`NOT_JUNK`): the registry has `$Junk`/`$NotJunk`, Thunderbird writes
`Junk`/`NonJunk`, and Apple Mail writes both its own `Junk`/`NotJunk`
and the registry's. No standard says which wins, and the documented
result is a message carrying `\Seen Junk $NotJunk NotJunk` — junk and
not junk at once — because each client sets its own spelling and leaves
the rest alone.

`set_junk` does the opposite. Every spelling of the opposite meaning is
cleared unconditionally: clearing what is not set costs nothing, and
leaving it set is precisely what creates the contradiction. What is
*written* is decided by the server, through `PERMANENTFLAGS` (RFC 3501
§7.1): every spelling it will keep, which on a mailbox advertising `\*`
is all of them, and on one naming a closed list is only what it names.
Yahoo and AOL list `$Junk $NotJunk` without `\*`, which is why
Thunderbird's hardcoded `Junk` silently fails to stick there.

`Permanent` (`src/imap/mod.rs`) carries that reply: the names, whether
`\*` was among them, and whether the server said anything at all — RFC
3501 has a client assume every flag is permanent when it did not.
`change_flags` consults it for *any* keyword, not just these, and
refuses a name the mailbox will not keep rather than storing something
the server may drop at the end of the session. `flag list`/`tag list`
read the family back with `junk_state` and report `contradictory` when
both sides are present. Setting one spelling by hand (`tag add 5 Junk`)
is allowed but noted on stderr, since that is the move that creates the
contradiction in the first place.

### Telling selections from names

`flag`/`tag` `add`/`remove` take selections and names in one list, with
no separator and in any order — `flag add 1 4 seen`. `split_args`
(`src/main.rs`) partitions the list on "does this parse as a
selection?", which is a *total* test because `parse_flag_names` refuses
a name that would: neither can be mistaken for the other, so nothing
has to mark where one list ends. An empty half on either side is an
error naming which one is missing.

`-f`/`--folder` is repeatable and comma-separated, and accepts IMAP
`LIST` patterns (`*` crosses the hierarchy delimiter, `%` does not);
`folder_matches`/`expand_folders` (`src/cli/select.rs`) match patterns
against the mailbox list fetched by one `LIST "" *`, only when a pattern
is actually given (a literal name skips the `LIST` round trip and is
passed through even if the server never mentioned it). `-A`/
`--all-folders` is `-f '*'`. An empty folder name (`-f ''`) is refused.
With no `-f`/`-A`, the default is `Config::folder` (`INBOX`), except for
`count`, which defaults to every selectable mailbox.

**A `NIL` delimiter means no hierarchy, not the usual separator.** The
delimiter is per mailbox (see `info`, above), and a `LIST` entry may
report `NIL` for it. `folder_matches` therefore takes an
`Option<char>`, and `%` with `None` has no boundary to stop at, so it
matches whatever `*` would — decided per entry, so one `NIL` mailbox
does not widen `%` for the mailboxes that reported a delimiter. The
alternative was to substitute a plausible `/`, which is what the code
did and what made this wrong: RFC 3501's own `LIST` example is a
mailbox literally named `extended/notes` with a `NIL` delimiter, and
`-f 'extended/%'` refused to match it because `%` stopped at a slash
the server had said was part of the name. The config's `delimiter` is
*not* the fallback either — it is advisory and exists to be reported
(above), so letting it decide what matches would make a field that
rewrites nothing quietly rewrite which mailboxes a command touches.
The empty string some servers send in place of `NIL` reads as `NIL`
here, the same reading `info` already gives it.

### MIME parsing (`mime.rs`)

`part` works on the raw RFC822 bytes: headers are unfolded and parsed
(`Content-Type`, `Content-Disposition`, `Content-Transfer-Encoding`),
`multipart/*` bodies are split on their boundary lines (preamble and epilogue
discarded), and leaf parts are numbered in document order (1-based). Part
bytes are decoded per CTE: base64 (whitespace-tolerant, padding restored),
quoted-printable (hex escapes + soft line breaks), or raw for 7bit/8bit/binary.

**Bytes are the primitive, not the file.** `ImapBackend::fetch_part`
returns a part's decoded bytes; `save_part` is a *provided* method on
the trait written in terms of it, so a backend implements one and gets
the other. It is arranged that way round because the reverse cannot be
undone: a caller that wants the part on stdout (`part save -o -`) and
has only a write-to-a-path primitive has to invent a temporary file,
and the obvious temporary file — `std::env::temp_dir()`, named from the
pid and the UID — puts somebody's mail attachment in a world-readable
directory under a name any local user can guess, at a path a planted
symlink can redirect. `-o -` therefore never touches the filesystem;
the bytes go from the fetch to stdout.

### RFC 2047 subject decoding
ENVELOPE subjects may be encoded-words (e.g. `=?utf-8?Q?Votre=20facture?=`).
`decode_rfc2047` finds each well-formed encoded word in the value and decodes
it (UTF-8, ISO-8859-1/Latin-1, `B` and `Q` encodings, padding restored for
base64). Plain text is left untouched.

### Comparing keywords, in both places that do it

Two keywords are the same when their **decoded** text matches, folding
ASCII case only. Inside a modified-UTF-7 shift the base64 is data:
`r&AOk-gie` is "régie" and `r&aok-gie` is a different word, so folding
the atom's case is not a relaxation, it is a wrong answer.

That rule lives in two places — `cli::same_keyword`, for names the user
typed, and `imap::same_keyword`, used by `Permanent::keeps`. The second
was a raw `eq_ignore_ascii_case` until a review caught it, and it
mattered more than it looks: `keeps` decides whether a mailbox with a
closed `PERMANENTFLAGS` list will retain a keyword, so a false yes let
`tag add` write one the server kept for the session and dropped at the
next refresh — reporting success for a change that did not last, which
is the exact failure that check exists to prevent.

Two copies of one rule is a thing to watch. They are apart because the
layers are: `Permanent` belongs to the IMAP side and must not reach up
into the command layer for a comparison. If a third appears, the rule
should move to one place instead.

## Configuration (`config/mod.rs`)

The config file is **UCL**, not JSON. The file is hand-edited, lives
next to a FreeBSD userland whose own configuration is UCL, and wants
the things JSON refuses to give it: comments, bare keys, no commas, no
outer braces. A config nobody can annotate is a config whose fields
get re-derived from the README every time somebody opens it.

UCL being a **superset of JSON** is what made the switch cheap: every
config written before it, and every JSON example in the documents,
parses unchanged. There was no migration and there is no second
format to support — there is one parser, and JSON is a dialect it
already accepts.

**The parsed object is emitted back as JSON and handed to serde**
rather than being walked key by key:

```text
  file ──▶ libucl parse ──▶ emit JSON ──▶ serde_json ──▶ Config
                                             │
                  the serde attributes on Config are still the
                  single definition of every field name, alias
                  and default
```

Walking the UCL object by hand would mean a second description of the
same schema — field names, the `access-level`/`access_level` aliases,
the defaults, the level spellings — sitting beside the `#[serde(...)]`
attributes and drifting from them. The round trip costs one
serialisation of a file that is a few hundred bytes, and buys the
guarantee that there is nothing to keep in step.

**A profile is a top-level block; a setting never is.** No field of
`Config` is an object, so "object at top level" identifies a profile
with nothing to disambiguate — the file needs no `[profiles]` wrapper
and no marker key. Scalars beside the blocks are shared defaults that
a profile overrides, which is what lets `max` or `access-level` be
written once for a whole file; `default` names the profile to use when
`-p` does not, and is the one scalar excluded from the merge.

Selection happens on the parsed JSON, before serde: merging two
objects is something `serde_json::Value` already does, where doing it
after deserialisation would mean a `Config` with every field optional
and a second pass to fill the gaps.

**A shared setting is a default, not a ceiling — including
`access-level`.** A profile may raise it above the shared value, which
looks wrong beside `--access-level`, that may only narrow. The two are
different in the way that matters: the command line is typed by
whoever runs the tool, which may be an agent working from a prompt,
while the config file is written deliberately and already holds the
password. Anyone who can edit it can reach the account with any client
at all, so treating a line in it as a boundary to be enforced against
a line three below it protects nothing and costs a special case in the
merge. The guard is against the *tool* exceeding its instructions, and
the profile is where the instruction is written. A test pins this so
it is not read later as a bug.

**Nothing is selected for the caller.** A config with profiles and no
`-p` and no `default` is an error naming what it has. Guessing — the
first block, or the only one that looks complete — means guessing
which account to reach, and unlike a missing file, a connection to the
wrong mailbox does not announce itself.

**Where the file is looked for distinguishes an answer from a
candidate.** `--config` and `$MAIL_IMAP_CONFIG` name the file
outright, so a missing one is an error; only `~/.config/mail-imap.conf`
and `/etc/mail-imap.conf` are searched, user first. Falling back from a
named path would mean a run whose `--config` pointed at a typo silently
reaching a different account — the failure that matters here is not a
missing file but a connection to the wrong mailbox. `config_path`
resolves it in one place so `info` reports the file the rest of the run
actually read, and the resolution is a pure function of the
environment, which is what lets it be tested without mutating the
process.

Two narrowings are deliberate:

- **`NO_TIME`.** UCL reads a bare `30s` as a duration and would hand
  serde a number. Nothing in `Config` is a duration, so a value that
  merely looks like one — a password, a folder name — is kept as the
  text it was written as.
- **A NUL byte is refused before the parser sees it.** `libucl`'s
  Rust wrapper builds a `CString` and unwraps, so a file containing a
  NUL would abort the process rather than fail. The guard turns it
  into an ordinary error, and a test feeds it a NUL to prove so.

**An unparseable config is fatal, never a default.** Falling back to
`Config::default()` would widen what a config meant to narrow: a typo
in `readonly` would silently become `organize`, which may change mail.
The one exception is `--mock`, where no server is reached and the
whole config is optional — and there `info` reports which file it
actually read, or that it read none.

### Authentication, and the one thing here with no gate behind it

`auth` selects the mechanism: `login` (the default) or `xoauth2`.
Under XOAUTH2 the secret is an OAuth 2 access token, and it arrives
through exactly the machinery a password does — `password` or
`password-command` — because a token is a short-lived secret fetched
from somewhere else, which is what that machinery is for. Acquiring it
(the OAuth dance, refresh tokens, a client secret) is deliberately not
this tool's job.

The capability a mechanism needs is asked of `AuthMethod`
(`required_capability`) rather than spelled at the call site, so the
mechanism and its requirement stay one fact — the same reason the
access gates ask `AccessLevel` instead of comparing levels inline.

The payload is fixed and unforgiving:
`user=<user>\x01auth=Bearer <token>\x01\x01`, where `\x01` is one
byte, handed back raw because the `imap` crate base64-encodes whatever
the authenticator returns. Encoding it here too would send
double-encoded rubbish that a server can only report as a generic
failure. That is unit-tested byte for byte, including a count of the
0x01 separators, because they are invisible in a string literal and a
stray space would look identical in a diff.

**And that is as far as the proof goes.** The XOAUTH2 *exchange* has no
end-to-end test, and it is the only thing in this tree in that
position. GreenMail turned out to advertise `AUTH=XOAUTH2` and then
refuse the RFC 3501 challenge flow — it wants the initial response
inline, the RFC 4959 SASL-IR form, which the `imap` crate does not
send — so the throwaway server can neither complete an exchange nor
exercise the missing-capability refusal. What `tests/wire.rs` does
cover is that an advertised mechanism gets past the gate and that a
failure past it reaches the user wrapped in something actionable
rather than as a bare protocol error.

So: the first person to point this at Gmail or Microsoft 365 is the
test. That is written down in the README too, where a user will see it,
rather than only here.

### What `info` claims, and what it does not

`info`'s `advertises` line lists what the **server** said it can do. It
is not a list of what this tool will do with it, and the difference is
worth stating because the line names things no command here uses —
`QUOTA` and `IDLE` most obviously.

That is deliberate rather than an omission waiting to be filled. The
line exists so a caller can tell *why* an operation took the path it
took, or was refused: `filing` says `UID MOVE` because `MOVE` is on
that list, `expunging` says `refused` when `UIDPLUS` is not. Reporting
only the capabilities this tool consumes would make those explanations
unverifiable from the output. The lines directly above it — `filing`,
`expunging`, `sorting`, `threading`, `folder create --use` — are the
ones that say what this tool will do; `advertises` is the evidence
underneath them.

So `QUOTA` appearing there is not a promise of a `quota` command, and
`IDLE` is not a promise of a `watch`. If either is ever built, it joins
the lines above rather than changing what `advertises` means.

### Timeouts, and what they do not cover

`timeout` (seconds, default 30, `0` to disable) is set on the session
after it is established, and the route in is worth recording because it
is not obvious: `Session` and `Client` have no timeout of their own,
only the boxed `Connection` they hold does, via `SetReadTimeout`. So
`establish_session` takes the connection back with
`Client::into_inner()`, sets the timeout on it, and re-wraps it with
`Client::new()` — without a second `read_greeting()`, since
`ClientBuilder::connect()` has already consumed the greeting.

What it bounds is a server that accepts and then goes quiet
mid-response. Three things it does not bound, all stated in the code
beside it rather than left to be discovered:

- **the connect**, and for TLS the handshake, which `ClientBuilder`
  owns and hands back already finished;
- **a stalled write** — `SetReadTimeout` is read-only and has no
  counterpart, so a large `append` to a server that stopped reading
  hangs;
- **a server that never accepts at all.**

Closing those means patching the fork, which is a bigger decision than
this fix: the forks are rebased by hand and CHECKLIST.md wants that
debt shrinking rather than growing.

The dead pre-connect this replaced is worth a sentence, because it
looked like a timeout and was not: `establish_session` built a
`TcpStream` with `connect_timeout`, set `nodelay` on it, and dropped
it, after which `ClientBuilder::connect()` opened its own untimed
connection. It cost a round trip and bounded nothing.

### Where the password comes from

`password` and `password-command` are exclusive, and exactly one must
be set — neither is an error, not a default. `password-command` runs
through `sh -c`, so a user can write a pipeline without this tool
guessing at quoting rules it would get wrong.

Three rules are worth stating because each one guards a failure that
would otherwise be diagnosed as "wrong password":

- **Exactly one trailing newline is stripped** — not `trim()`, not
  `trim_end()`. `pass` and friends emit one newline; a password may
  legitimately end in a space, and taking it would send a different
  password than the one stored. There is a test for the trailing
  space, and it exists to stop either function being reintroduced.
- **Failure is failure.** A non-zero exit or empty output is an error
  naming the exit status and the command's stderr — stderr because
  that is where `gpg: decryption failed` appears. There is no fall
  back to an empty password, which would reach the server and be
  refused for a reason that has nothing to do with the real fault.
- **There is no timeout**, deliberately. `gpg` or `pass` may be sitting
  at a pinentry prompt, and that wait is the user's to end.

The password never reaches an output stream. `-d` says the command ran
and whether it succeeded, never what it printed, and the value is held
in `Secret`, whose `Debug` prints `<redacted>`. That last part is
structural rather than a thing to remember: the password used to be a
plain `String` in a `#[derive(Debug)]` struct, so one `{:?}` of a
`Config` anywhere would have put a credential on a stream. Nothing did,
so it was a trap and not a leak — and a trap is worth closing with a
type rather than a comment, since any secret field added later gets the
same protection by using it.

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

### It is a development aid, and does not ship

The mock sits behind a cargo feature, `mock`, which is on by default and
which the release build turns off (`F_yes = --no-default-features` in
the Makefile). So `make build` produces a binary where `--mock` is an
unknown argument and `--help` does not offer it, while
`make build RELEASE=no` keeps it — which is what `make tests`,
`scripts/check-examples.sh` and QUICKSTART's worked example all use.

A build without it does not silently fall back to a real server when a
config says `mock = true`: it refuses to start. `--mock` means *do not
touch my account*, and quietly touching it is the worst available
reading of that flag.

`make check` lints both configurations. `cfg`-gated code that compiles
one way and not the other is exactly the failure this arrangement
invites, and linting only the default would not see it.

### It is held to what a real server does

A fake is only worth having if it behaves like the thing it stands in
for, and for a long time this one did not — its behaviour was reasoned
about rather than checked. Measured against a real server, it was wrong
in four places, in two opposite directions:

| | real server | mock, before |
| --- | --- | --- |
| `UID STORE` / `UID MOVE` naming a UID that does not exist | `OK`, ignored (RFC 3501 §6.4.8) | refused |
| anything naming a mailbox that does not exist | error — it `SELECT`s first | answered anyway |

Which direction a fake is wrong in matters more than that it is wrong.
Being **stricter** than the real thing is the dangerous one, because it
turns a live defect into an offline pass: `tag junk` on a missing UID
reported success on the wire and could not be reproduced under `--mock`,
because the mock refused where the server shrugs. Being laxer is the
same mistake facing the other way — `-f Nonexistent` succeeded here and
failed on the wire.

`tests/wire.rs::the_mock_answers_like_a_real_server` is what stops this
drifting again. One list of probes is run against both backends and
they must agree on **which calls are refused** — not on the messages,
since a mock saying `no mailbox 'x' (mock)` and a server saying
`SELECT failed. No such mailbox` are the same answer in different
words, and not on content, since the two hold different mail. It runs
under `make tests-wire`, because checking a fake against a real server
needs the real server.

What the mock deliberately does *not* imitate: it advertises no
capabilities at all, which is the point — it stands in for the barest
server there is, so every degradation ladder is exercised offline. And
it holds the same five messages in every folder, a simplification the
fixtures depend on.

## Testing

`make tests` (or plain `cargo test`) runs the suite entirely against the
mock backend (no network). What it proves, and where:

- **the access-level ladder refuses what it says it refuses** —
  `readonly` changes nothing, `organize` sets anything but `\Deleted`
  and clears even that, `organize` leaves the folder tree alone,
  `restructure` changes the tree and still loses no message, renaming
  INBOX is refused at every level. Each refusal has a test that feeds it
  the forbidden operation and asserts the error — `src/lib.rs`, with the
  ladder's own ordering and both config spellings in `src/config/mod.rs`
- **what `info` reports**, including a config `delimiter` disagreeing
  with the server's and a run narrowed by `--access-level` showing both
  levels — `src/cli/mod.rs`
- backend selection (`mock` vs `real`), and that the real backend
  returns an error rather than fabricating data when unreachable
- list / search / read / count / uid / unread / part happy + error
  paths, and that a multi-folder search aggregates and caps the *total*
- thread reconstruction from Message-ID / References: a reply chain
  forms one thread, separate threads stay separate, `References` merges
  two, duplicate Message-IDs are merged, a message with no IDs is its
  own thread — `src/imap/mod.rs`
- what a mailbox will keep, read off `PERMANENTFLAGS`: a silent server
  and one advertising `\*` keep everything, a closed list keeps only
  what it names — `src/imap/mod.rs`
- sort spec parsing (keys, `-` reverse, invalid input) and client-side
  result sorting (single/multi key, descending, capped selection)
- flag/tag name validation (system flags normalized, `\Recent`
  rejected, tag mode refuses `\` flags, keyword charset, invisible
  characters, `--wire` refusing what cannot be an atom) and mock
  store/retrieve, including `flag list` via `message_flags`
- the keyword layer: the IANA registry transcribed once and
  consistently, the two tables not overlapping, JMAP spellings of system
  flags refused, the junk family read across every spelling, and a
  listing that prints what can be typed straight back — glossing what
  could not — `src/cli/keywords.rs`, `src/cli/mod.rs`
- modified UTF-7 (RFC 3501 §5.1.3) round trips, NFC composition before
  encoding, malformed sequences erroring rather than guessing, and
  Thunderbird's `=xx` tag keys read back as text — `src/cli/modutf7.rs`,
  `src/cli/tbkey.rs`
- message-selection grammar (lists, ranges in both notations, `*`/`n-*`,
  recency counts, folder-qualified tokens, the last-`:` split rule, UID
  0 and wildcard rejection, resolution against a folder's UID list) and
  folder-pattern matching/expansion (`*`/`%`, INBOX case-insensitivity,
  dedup, a pattern matching nothing) — `src/cli/select.rs`
- the JSON shape of `search` (one folder and several), `count`,
  `thread`, `part list`, `info`, `flag add`/`remove`, `flag list` and
  the junk family, asserted by serialising the output struct. The
  shapes not covered there — `read`, `uid`, `move`, `part save`,
  `folder` and its subcommands, `tag known` — are checked by reading
  the README table against the structs, which is not a gate
- MIME parser (plain, multipart, nested multipart, base64 /
  quoted-printable / binary decoding, missing boundary)
- RFC 2047 decoder (plain, Q, Q-with-underscore, B, Latin-1, mixed text)
  and header-value reading (case-insensitivity, folded continuations,
  raw 8-bit bytes kept)

Two things the suite does **not** prove, and they are the two that
break in practice: nothing in it fails because `src/imap/real.rs` sent
the wrong thing to a server, and nothing in it drives `src/main.rs`, so
a broken argument shape passes it. Both gates are in `CHECKLIST.md`.

To exercise the real backend manually:

```bash
cargo run --release -- -c incal.conf folder
cargo run --release -- -c incal.conf -f INBOX search "SINCE 01-Jan-2026"
```

## Output

Commands print human-readable text by default. With `-j`/`--json` each
command prints a single compact JSON object to stdout instead; errors
become `{"error": "..."}` on stderr (the exit code is unchanged either
way). The per-command shapes are the reference material, and they live
in one place: [JSON output (`-j`) in README.md](README.md#json-output--j).
What is decided here is how they are shaped.

- **A stream of objects, not one wrapping array**, for everything that
  can produce more than one. `emit_json` is `println!` of
  `serde_json::to_string`, so each object is exactly one line and a
  caller reads the output line by line. Each is printed as it is
  finished, so the first folder's results arrive before the last folder
  has been touched, and a run that dies half-way has still delivered
  what it completed. The unit differs by command, and follows what the
  command is *about*: one object per selected **message** for `read`,
  `thread`, `part list` and `flag list`/`tag list`, and one per
  **folder group** for `uid`, `move` and `flag`/`tag` add and remove,
  which act on a whole group in one `UID STORE` or `UID MOVE`.
- **`search`/`unread` are the exception, and emit one object** covering
  every folder, because their `count` is a total: `--max` is a budget
  spent across the folders in order, each one taking what the previous
  ones left (`search_folders`, `src/imap/real.rs`), so how many results
  there are is not known until the last folder has been searched.
  Ordering stays per folder — `-S` sorts within a folder, and the
  folders keep the order `-f` gave them. `info`, `count`, `folder`,
  `part save` and `tag known` emit one object because there is only
  ever one.
- **`"folder"` for a single folder, `"folders"` for several** — the same
  rule the text output follows, stated once above under the search
  iteration.
- **The wire form is what JSON carries**, everywhere the two differ:
  `tag list` prints `régie` as text but writes `r&AOk-gie` in JSON, and
  the keyword names in `added`/`removed` are the atoms that went to the
  server. Text output is for a reader, who benefits from decoding; JSON
  is for a caller, who has to be able to send back what it was given.
- **Text may show less than JSON, never something different.**
  `folder` prints bare names until `-l`/`--long` asks for the
  hierarchy delimiter and the LIST attributes, because the usual
  reason to run it is to find out what a mailbox is *called* so the
  name can go back into `-f`, and a name trailed by parentheses is
  worse to read off and to copy. JSON ignores `-l` and always carries
  every field: a caller cannot ask again, and a field that appears
  only under a flag is a field no caller can rely on.
- **The shapes are types, not `serde_json::json!` literals.** Every
  object above is a `#[derive(Serialize)]` struct in `src/cli/mod.rs`
  (or, for `FolderInfo`, `SearchResult`, `Mailbox` and `PartInfo`, the
  shared types in `src/imap/mod.rs` that the backends already fill in).
  A field renamed in the struct is a field renamed in the output, which
  is what keeps the README table checkable against the code rather than
  against a format string.

## Error handling

Connection, TLS, authentication and server (BAD/NO) errors surface as
`anyhow::Error` with context; the CLI prints them and exits non-zero.

`LOGOUT` is sent exactly once when the client is dropped (`RealClient` tracks
a `closed` flag so the outer and inner `Drop` impls cannot double-send it).
A `ConnectionLost` at logout is suppressed — it only means the connection was
already closed and there is nothing to log out. Other logout failures (server
BAD/NO) still print a warning.
