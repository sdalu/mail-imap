# mail-imap - IMAP Email Client

A command-line tool for querying and reading emails via the IMAP protocol.
Designed for programmatic / AI use.

**Reads change nothing**: they use `BODY.PEEK[]`, so fetching a message
does not even mark it `\Seen`. What it may change is capped by
`access-level` in the config — `readonly`, `organize` (the default),
`restructure` or `full` — and every changing command says which level it
needs. See [Access level](#access-level).

It connects to a **real IMAP server** using the `imap` crate (over implicit TLS
on port 993, STARTTLS, or plain TCP). An **in-memory mock backend** is kept so
the tool can be built, demoed and unit-tested without a reachable server.

## Quick start

Nothing to configure to see it work — the in-memory mock backend needs
no server and no config at all:

```bash
make build RELEASE=no
./target/debug/mail-imap --mock folder list
./target/debug/mail-imap --mock search invoice
./target/debug/mail-imap --mock -j info
```

For a real account, the smallest config that works is four fields:

```json
{
    "server": "imap.example.com",
    "username": "user@example.com",
    "password": "secret",
    "access-level": "readonly"
}
```

```bash
mail-imap -c myaccount.conf info
mail-imap -c myaccount.conf -f INBOX search UNSEEN
```

`info` first is the habit worth forming: it reports what this run may
change, how to spell a folder path on this account, and which wire path
each operation will take — none of which has to be discovered by trying
commands and reading refusals.

[QUICKSTART.md](QUICKSTART.md) walks the same path with output;
`mail-imap.1` is the man page. The rest of this file is the reference.

## Build & Install

Building needs a Rust toolchain, a C compiler and **cmake** — the UCL
config parser is a C library compiled from source as part of the
build and linked statically, so the finished binary has no runtime
dependency on it.

The `imap` and `imap-proto` crates are local forks under `../forks`,
carrying RFC 5256 `THREAD`/`SORT` support that upstream has not
released; a checkout without them does not build.

The `Makefile` is the interface (`make` alone, or `make help`, prints
what it does):

| Target           | Does                                                                                                                                                            |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `make` (`help`)  | Print the target list and the current build variables                                                                                                           |
| `make check`     | Preflight, running none of the project's code: clippy with warnings denied, the release number written only in `Cargo.toml`, and `mail-imap.1` well-formed mdoc |
| `make build`     | Build the binary (`RELEASE=no` for a debug build; release is the default)                                                                                       |
| `make tests`     | The whole suite: `tests-unit` (cargo test) then `tests-examples` (every documented command line, run against `--mock`)                                          |
| `make doc`       | Generate the API documentation (`cargo doc --no-deps`)                                                                                                          |
| `make install`   | Install the binary under `BINDIR` and `mail-imap.1` under `MANDIR` (`DESTDIR` stages both)                                                                      |
| `make uninstall` | Remove what `install` put down                                                                                                                                  |
| `make clean`     | Remove what a build here made (`cargo clean`)                                                                                                                   |
| `make options`   | Print the build knobs and their defaults                                                                                                                        |

Build knobs: `RELEASE` (`yes`), `PREFIX` (`/usr/local`), `BINDIR`
(`$PREFIX/bin`), `MANDIR` (`$PREFIX/share/man`), `DESTDIR` (empty),
`CARGO`. `make help` prints each with its current value.

`cargo` commands still work directly:

```bash
cargo build            # debug
cargo build --release  # release
cargo test             # tests (mock backend)
cargo run -- --help
```

## Features

- Report what this run can do and what the server is, in one call
  (`info`): the access level in force, the hierarchy delimiter to build
  folder paths with, the special-use mailboxes (`\Trash`, `\Junk`, …),
  and which wire path filing, sorting and threading will take here
- List mailboxes/folders, selected with `-f`/`-A` (literal names or IMAP
  `LIST` patterns such as `Archive/*`)
- Change the folder tree: create a mailbox (declaring an RFC 6154
  special use if the server takes one), rename one, subscribe and
  unsubscribe (`access-level` `restructure`)
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
- Move email(s) to another folder, named last as `mv` does: `UID MOVE`
  (RFC 6851) where the server has it, `UID COPY` + `UID EXPUNGE`
  (RFC 4315) where it does not, and a refusal where it has neither
  (`access-level` `organize`)
- Enable/disable the IMAP-defined message flags (`\Seen`, `\Answered`,
  `\Flagged`, `\Deleted`, `\Draft`) with `flag`, add/remove
  user-defined keyword tags with `tag`, and inspect either (`UID STORE`;
  `\Recent` is server-managed and cannot be set)
- Knows the IANA *IMAP/JMAP Keywords* registry and the conventions no
  registry covers (Thunderbird's `$label1`…`$label5`, `Junk`/`NonJunk`):
  a known keyword is sent in its settled spelling, any known keyword is
  glossed when listed, and `tag known` prints both tables with a
  one-line meaning for every entry
- Handles modified UTF-7 keywords (RFC 3501 §5.1.3): `tag add 5 régie`
  sends `r&AOk-gie` (NFC-composed first), and `tag list` decodes what it
  shows — including Thunderbird's own `=xx` tag keys
  (`r=c3=a9gie` → "régie")
- JSON output mode (`-j`) for programmatic use
- Configuration in UCL (a JSON superset, so JSON configs keep working)
- RFC 2047 subject decoding (UTF-8, Latin-1, B & Q encodings)

## Usage

### Commands

`<SELECTION>` is a message selection; see
[Message selections](#message-selections) below.  Every command also
takes the folder flags `-f`/`-A`; see [Folder selection](#folder-selection).

| Command              | Syntax                                            | Description                                                                                                                                                                     |
| -------------------- | ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `info`               | `info`                                            | What this run can do and what the server is: access level, hierarchy delimiter, special-use mailboxes, and the wire path filing / sorting / threading take here                 |
| `folder list`        | `folder list [-l]`                                | List mailboxes/folders; `-l`/`--long` adds each one's hierarchy delimiter and LIST attributes                                                                                   |
| `folder create`      | `folder create <FOLDER> [--use <ATTR>] [--wire]`  | Create a mailbox, optionally declaring an RFC 6154 special use at creation: `archive`, `junk`, `sent`, `trash`, `drafts`, `all`, `flagged` (needs `access-level` `restructure`) |
| `folder rename`      | `folder rename <FROM> <TO>`                       | Rename a mailbox; INBOX is refused at every level (needs `restructure`)                                                                                                         |
| `folder subscribe`   | `folder subscribe <FOLDER>`                       | Subscribe to a mailbox (needs `restructure`)                                                                                                                                    |
| `folder unsubscribe` | `folder unsubscribe <FOLDER>`                     | Unsubscribe from a mailbox (needs `restructure`)                                                                                                                                |
| `search`             | `search <QUERY>`                                  | Search emails with any IMAP `SEARCH` query in the selected folder(s); most recent first unless `-S`                                                                             |
| `read`               | `read <SELECTION...>`                             | Read the selected email(s)                                                                                                                                                      |
| `count`              | `count`                                           | Message counts / status of the selected folder(s), or every mailbox if none is given (alias: `status`)                                                                          |
| `uid`                | `uid`                                             | List the message UIDs of the selected folder(s), one block per folder                                                                                                           |
| `thread`             | `thread <SELECTION...>`                           | List the UIDs of every message in the thread(s) containing the selected message(s)                                                                                              |
| `unread`             | `unread`                                          | List unread emails of the selected folder(s) (`search UNSEEN`)                                                                                                                  |
| `part list`          | `part list <SELECTION...>`                        | List the MIME parts of the selected email(s)                                                                                                                                    |
| `part save`          | `part save <SELECTION> <PART> [-o\|--out <FILE>]` | Save one MIME part of one message to a file (the selection must name exactly one message)                                                                                       |
| `move`               | `move <SELECTION...> <FOLDER>`                    | File the selected email(s) into another folder, named last as `mv` does; it must already exist (needs `organize`)                                                               |
| `flag list`          | `flag list [--wire] <SELECTION...>`               | List the flags (system flags + keyword tags) of the selected email(s)                                                                                                           |
| `flag add`           | `flag add [--wire] <SELECTION...> <FLAG...>`      | Enable IMAP-defined flags on the selected email(s): `seen`, `answered`, `flagged`, `deleted`, `draft`                                                                           |
| `flag remove`        | `flag remove <SELECTION...> <FLAG...>`            | Disable flags on the selected email(s)                                                                                                                                          |
| `tag list`           | `tag list [--wire] <SELECTION...>`                | List the custom keyword tags of the selected email(s) (system flags omitted)                                                                                                    |
| `tag add`            | `tag add [--wire] <SELECTION...> <TAG...>`        | Add custom keyword tags (plain keywords, no system flags)                                                                                                                       |
| `tag remove`         | `tag remove <SELECTION...> <TAG...>`              | Remove custom keyword tags                                                                                                                                                      |
| `tag junk`           | `tag junk <SELECTION...>`                         | Mark junk: set every spelling the mailbox keeps, clear every spelling of the opposite                                                                                           |
| `tag notjunk`        | `tag notjunk <SELECTION...>`                      | Mark not junk (the same, the other way)                                                                                                                                         |
| `tag known`          | `tag known`                                       | List the keywords with an agreed meaning: the IANA registry and the conventions no registry covers (no server needed)                                                           |

### Grammar

A command line is the program, options, a command, and — for the
commands that work on messages — one or more *selections*:

```text
  mail-imap  -j -f INBOX,Archive/*  move  INBOX::1-5 last:3  Archive/2026
  └───┬───┘  └─────────┬─────────┘  └─┬┘  └───────┬───────┘  └─────┬────┘
      │                │              │           │                │
   program      global options     command   selections            │
                                                      the folder ──┘
                                                      (move only)
```

A selection names messages, and optionally the folder to find them in:

```text
  Archive/2026::1,4,7-9,last:3
  └─────┬────┘┬ └──────┬─────┘
        │     │        │
        │     │        └── UID spec: items, ','-separated
        │     │              5        one UID
        │     │              7-9      an interval
        │     │              9-       from 9 to the end of the mailbox
        │     │              *        every message, and '*' is only ever this
        │     │              last:3   the 3 newest, by UID
        │     │              first:3  the 3 oldest
        │     └── binds the folder to the spec; nothing else does
        └───── the folder. Without one, -f applies (or the config)
```

The whole grammar, in EBNF:

```text
invocation  = "mail-imap" , { option } , command ;

                          (* options are global: they may appear before
                             or after the command, in any order *)
option      = ( "-c" | "--config" ) , path
            | ( "-f" | "--folder" ) , pattern , { "," , pattern }
            | ( "-A" | "--all-folders" )
            | "--access-level" , level
            | ( "-M" | "--max" ) , number
            | ( "-S" | "--sort" ) , sort-spec
            | "-j" | "--json" | "-d" | "--debug" | "--mock"
            | "-h" | "--help" | "-V" | "--version" ;

command =
              (* what this run can do, and what the server is *)
              "info"

              (* mailboxes *)
            | "folder"
            | "folder" , "create" , folder ,
                       [ "--use" , special-use , [ "--wire" ] ]
            | "folder" , "rename" , folder , folder
            | "folder" , ( "subscribe" | "unsubscribe" ) , folder
            | "count" | "status"        (* one command, two names *)
            | "uid"

              (* finding messages *)
            | "search" , imap-query
            | "unread"
            | "thread" , selection , { selection }

              (* reading them *)
            | "read" , selection , { selection }
            | "part" , "list" , selection , { selection }
            | "part" , "save" , selection , part-number ,
                       [ ( "-o" | "--out" ) , path ]

              (* changing them: each needs an access-level *)
            | "move" , selection , { selection } , folder
            | "flag" , "list" , [ "--wire" ] , selection , { selection }
            | "flag" , ( "add" | "remove" ) , [ "--wire" ] , flag-args
            | "tag" , "known"           (* needs no server *)
            | "tag" , ( "junk" | "notjunk" ) , selection , { selection }
            | "tag" , "list" , [ "--wire" ] , selection , { selection }
            | "tag" , ( "add" | "remove" ) , [ "--wire" ] , tag-args ;

               (* no separator: a name that would read as a selection
                  is refused, so the two cannot be confused *)
flag-args   = selection , { selection } , flag , { flag } ;
tag-args    = selection , { selection } , keyword , { keyword } ;

selection   = [ folder , "::" ] , uid-spec ;
uid-spec    = item , { "," , item } ;
item        = uid
            | uid , "-" , uid      (* an interval                    *)
            | uid , "-"            (* uid to the end of the mailbox  *)
            | "*"                  (* every message; '*' is only this
                                      -- never a range endpoint      *)
            | ( "last" | "first" ) , ":" , count ;
uid         = digit , { digit } ;   (* 1 or more; 0 is refused       *)
count       = digit , { digit } ;   (* 1 or more                     *)

               (* ':' is last:/first: and nothing else: IMAP's own
                  "4:7" range is not accepted, "4-7" is. '::' binds
                  the folder, which is what frees ':' for a count. *)

pattern     = folder                (* or an IMAP LIST pattern: "*"  *)
            | wildcard-pattern ;    (* crosses the delimiter, "%" not *)
flag        = "seen" | "answered" | "flagged" | "deleted" | "draft" ;
                          (* case-free, and needing no shell quoting.
                             With --wire, the form a listing prints:
                             "\Seen", "\Answered", ... *)
keyword     = atom ;   (* non-ASCII is encoded to modified UTF-7     *)
level       = "readonly" | "organize" | "restructure" | "full" ;
special-use = "all" | "archive" | "drafts" | "flagged"
            | "junk" | "sent" | "trash" ;
                          (* case-free, and needing no shell quoting,
                             as the flags are. With --wire, the form a
                             listing prints: "\Archive", "\Junk", ... *)
```

### info

One call that answers what a caller would otherwise guess: what this
run may change, how to spell a folder path on this account, and which
wire path each operation will take here. It connects, asks `CAPABILITY`
and `LIST`, and changes nothing.

```console
$ mail-imap -c incal.conf info
mail-imap 0.1.0 (real backend)
  config      incal.conf
  account     user@example.com@imap.example.com:993 (implicit TLS)

Access level: organize
  set and clear flags and tags        yes
  move mail to another folder         yes
  create / rename / subscribe         no
  set \Deleted                        no

Folders
  delimiter   '/' (from the server)
  default     INBOX
  mailboxes   12
  \Archive    Archive
  \Drafts     Drafts
  \Junk       Spam
  \Sent       Sent
  \Trash      Trash

Search defaults
  max         50
  sort        (none: most recent first)

This server
  moving mail         UID MOVE
  sorting (-S)        server-side
  threading           server-side
  folder create --use available
  advertises          IDLE MOVE QUOTA SORT THREAD=REFERENCES UIDPLUS ...
```

What each block is for:

- **Access level** — the level in force, and the four things it governs,
  so an agent can tell `flag add` from `folder create` before trying
  one. A run narrowed with `--access-level` says so and names the
  ceiling the config still allows.
- **Folders** — the **hierarchy delimiter** to build a path with
  (`Archive/2026` vs `Archive.2026`), read off the mailbox list; see
  [Folder selection](#folder-selection). `delimiter` in the config
  overrides it, and `info` then prints what the server said too, since
  the two disagreeing is worth seeing. An account whose `LIST` reports
  more than one delimiter has more than one namespace, and `info` names
  them all. The **special-use** lines say which mailbox is the Trash on
  an account that does not call it "Trash". The default folder is
  flagged when it is not in the mailbox list at all — usually a typo in
  the config.
- **This server** — the path each operation actually takes here, rather
  than the capability names it is derived from: filing is `UID MOVE`,
  `UID COPY + UID EXPUNGE`, or refused; sorting and threading are
  server- or client-side; `folder create --use` is available or not.
  The raw capability list follows on the `advertises` line.

`-j` gives the same thing as one object:

```json
{"tool":{"name":"mail-imap","version":"0.1.0","backend":"real"},
 "config":{"path":"incal.conf","server":"imap.example.com","port":993,
           "tls":"implicit","insecure":false,"username":"user@example.com"},
 "access":{"effective":"organize","configured":"organize",
           "may":{"store_flags":true,"move_messages":true,
                  "change_folders":false,"set_deleted":false}},
 "folders":{"delimiter":"/","delimiter_source":"server","server_delimiter":"/",
            "delimiters_seen":["/"],"default":"INBOX","default_exists":true,
            "count":12,"special_use":{"\\Trash":"Trash","\\Junk":"Spam"}},
 "defaults":{"max":50,"sort":null},
 "server":{"capabilities":["IDLE","MOVE","SORT","UIDPLUS"],"filing":"UID MOVE",
           "sorting":"server","threading":"server","create_special_use":true}}
```

(shown wrapped; the real output is one line). `config.path` is `null`
when no config file was read, which `--mock` allows.

### Message selections

Every command that works on messages (`read`, `thread`, `part`, `flag`,
`tag`) takes one or more **message selections**: `[FOLDER::]UIDSPEC`.

```text
5                 one UID in the default folder
1,4,7             a list (separate arguments also work: read 1 4 7)
1-9               a UID range
3,9-12            a mix
'*'               every message of the folder (quote it — the shell globs it)
9-                from UID 9 to the end of the mailbox
last:20           the 20 newest — a count, not an interval
first:5           the 5 oldest
Archive::1-5      folder-qualified
Archive/2026::7   folder names with the hierarchy delimiter work
'Sent Items::3,4' folder names with spaces work (quote the whole token)
```

`last:N` and `first:N` ask for a **count** rather than an interval —
the thing no UID range can express, since UIDs are sparse:

```bash
# the 5 newest messages (the 5 highest UIDs) of the folder
mail-imap --config incal.conf -f INBOX read last:5

# the 5 oldest, and a mix with plain UIDs
mail-imap --config incal.conf -f INBOX read first:5
mail-imap --config incal.conf -f INBOX read 12345,last:3

# a count names no UID, so an unqualified one is not ambiguous across
# folders: it means N per selected folder
mail-imap --config incal.conf -f INBOX,Archive flag list last:5

# ... and a qualified one can ask a different count of each
mail-imap --config incal.conf read 'INBOX::last:10' 'Archive::first:2'
```

They count by UID, which ascends with arrival — they say nothing about
the `Date:` header; order search results by that with `-S -date`.

A few rules are easy to get wrong:

- A range is an interval of the UID space, **not a count**: UIDs are
  sparse, so `1-9` may match nine messages, two, or none.
- Ranges and `*` are resolved against the folder's real UID list (one
  `UID SEARCH ALL` per folder, fetched once and only when a selection for
  that folder holds a range or `*`). **Every item must match at least one
  message**, so `flag add 5 999-` is an error rather than quietly
  becoming `flag add 5`.
- A UID named explicitly is not checked against that list for read-only
  commands — the server reports it (`no email with UID n`). `flag` and
  `tag` do check first, because RFC 3501 has `UID STORE` ignore an
  unknown UID *without an error*, and reporting success for a typo would
  be a lie the server never told.
- `*` means every message and is only ever that: it is not a range
  endpoint, so `9-*` and `*-9` are both refused. "From 9 to the end of
  the mailbox" is `9-`. A `*` that is not alone in its item reads as a
  folder wildcard to anyone skimming, and IMAP's own `*:9` reads as
  "up to 9" to half of them when it means the opposite.
- `:` is `last:N` / `first:N` and nothing else. IMAP's own `4:7` range
  is **not** accepted — `4-7` is — because one concept with two
  spellings is what put `:` back to doing two jobs in the first place.
- **Deliberate deviation from IMAP:** RFC 3501 folds `559:*` onto the
  last message when 559 is past the end of the mailbox. This tool does
  not: an open range past the end matches nothing and errors, so
  `flag add 9999- deleted` cannot silently hit the newest message.
- UID `0` is refused (IMAP UIDs start at 1).
- Folder wildcards (`*`, `%`) are refused inside a selection; name one
  folder, or use `-f` to work on several.
- `::` binds the folder and nothing else does, which is what leaves a
  single `:` free for `last:` and `first:`. So `INBOX::1-5` is UIDs 1-5
  of INBOX and `2026::5` is UID 5 of a folder named `2026` — neither a
  guess. The split is at the **last** `::`, so a folder whose own name
  contains `::` is still writable.
- Folder names are trimmed (`-f 'INBOX, Trash'` is two folders, not
  `INBOX` and `" Trash"`), and `INBOX` is folded to one spelling because
  IMAP defines that one name as case-insensitive. Every other name keeps
  its case, which the server cares about.
- A range with no lower end (`-20`) is refused, and the argument parser
  usually rejects it first as a stray option. Write `1-20`, or
  `first:20` for a count. `*-20` is refused too: `*` is every message
  and never a range endpoint.
- Selections are grouped per folder; folders keep the order they were
  first named in, UIDs keep input order, and duplicates are dropped.
- `part save` takes a selection naming exactly one message, else it
  errors.
- `flag add`/`remove` and `tag add`/`remove` take selections and names
  in one list with no separator: a name that would read as a selection
  is refused, so the two cannot be confused. **Selections come first**,
  names after — the shapes would allow any order, but one order reads
  the same way every time, and a selection after a name is far likelier
  to be a mistake than an intention. Each command owns one kind of name
  and refuses the other's — `flag` takes only the five IMAP-defined
  flags, `tag` only user-defined
  keywords — which is also what stops `flag add 5 deleted Trash` from
  storing a keyword `Trash` on message 5 while leaving the Trash folder
  alone. Flags are bare words (`seen`, not `'\Seen'`, which the shell
  would make you quote); `--wire` takes the form a listing prints, for a
  name copied back out of one. A keyword spelled like a system flag
  without its backslash (`tag add 5 Deleted`) is refused: it would be
  inert, read like the flag in a listing, and slip past the access level
  that governs the real one.

When a selection has no folder of its own, it means the folder(s)
selected by `-f`/`-A` (or the config); if several folders are selected,
a bare UID is ambiguous and is refused with an error naming the folders
— it is not guessed at.

### Flags and tags

`flag` and `tag` take message selections and names in one list, with no
separator between them — nothing marks where one ends because nothing
has to: a name that would read as a selection is refused, so the two
shapes cannot collide.

```text
  mail-imap flag add   1-5  Archive::9   seen  flagged
                       └──────┬──────┘ │ └─────┬─────┘
                         selections    │     names
                                       │
                          the split ───┘

  The split is the first argument that is not a selection.
  Everything after it is a name, and a selection among them is an
  error rather than a re-ordering.
```

They own one kind of name each — the two kinds IMAP keeps in a single
list on every message:

```text
  IMAP keeps ONE list of flags on a message. Two kinds live in it:

  ┌──────────────────────────────────┬──────────────────────────────────┐
  │ system flags — begin with \      │ keywords — do not                │
  │                                  │                                  │
  │ \Seen      \Answered             │ invoice        $Important        │
  │ \Flagged   \Deleted              │ $label1        Junk              │
  │ \Draft                           │ r&AOk-gie      r=c3=a9gie        │
  │ (\Recent is the server's own)    │                                  │
  ├──────────────────────────────────┼──────────────────────────────────┤
  │ set by   flag add|remove         │ set by   tag add|remove          │
  │ typed    seen answered flagged   │ typed    as they are; a          │
  │           deleted draft          │           non-ASCII is encoded   │
  ├──────────────────────────────────┼──────────────────────────────────┤
  │ listed by  flag list             │ listed by  tag list  — this      │
  │   — both columns                 │     column only                  │
  ├──────────────────────────────────┴──────────────────────────────────┤
  │ --wire, on any of the four: the atom exactly as the server          │
  │ has it — \Seen, r&AOk-gie. Without it, a listing prints what        │
  │ add takes back, and add takes what a listing printed.               │
  └─────────────────────────────────────────────────────────────────────┘

  Neither command takes the other's names, which is what makes a
  stray word an error instead of a keyword nobody meant.
```

`--wire` means the same thing everywhere it appears: the atom exactly as
the server has it. On `add`/`remove` — and on `folder create --use`,
the one other place a backslashed atom reaches the command line — it is
what the names are taken as; on `list` it is what they are printed as.
Without it, a listing prints what you could type straight back —
`seen` for `\Seen`, `régie` for `r&AOk-gie` — and what could not be
typed back is glossed instead of
rewritten, since a Thunderbird key or a `$label1` would re-encode to a
different atom. JSON always carries the wire form.

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

### Examples

Every line below is a complete command. They assume a config at
`incal.conf`; drop `--config` to search `$MAIL_IMAP_CONFIG`,
`~/.config/mail-imap.conf` and `/etc/mail-imap.conf` in turn, and add
`--mock` to run any of them against the
in-memory backend with no server and no config at all.

#### Where am I, and what may this run do?

One call, before guessing any of it: the access level in force, the
hierarchy delimiter to build folder paths with, the special-use
mailboxes, and the wire path each operation takes on this server.

```bash
mail-imap --config incal.conf info
mail-imap --config incal.conf -j info
```

#### Folders

```bash
# List folders -- just the names, so they can be typed back into -f
mail-imap --config incal.conf folder list

# ... with each one's hierarchy delimiter and LIST attributes
# (\Sent, \Junk, \Noinferiors, ...)
mail-imap --config incal.conf folder list -l

# Change the folder tree (needs "access-level": "restructure")
mail-imap --config incal.conf folder create Archive/2026
mail-imap --config incal.conf folder create Archive --use archive
mail-imap --config incal.conf folder rename Spam Junk
mail-imap --config incal.conf folder subscribe Archive/2026
```

#### Searching

The query is any IMAP `SEARCH` expression — `UNSEEN`, `FROM bob`,
`SUBJECT invoice`, `SINCE 01-Jan-2026`, or any combination. `ALL`
matches every message; for bare UIDs use `uid` instead.

```bash
mail-imap --config incal.conf search "SINCE 01-Jan-2026"
mail-imap --config incal.conf -f INBOX search ALL
mail-imap --config incal.conf -f INBOX -M 200 search ALL   # raise the cap for this run
```

`-f` is repeatable and comma-separated, and both spellings mean the
same thing. IMAP can only search the selected mailbox, so the tool
iterates over the folders and aggregates; the `-M`/`--max` cap applies
to the total, not to each folder.

```bash
mail-imap --config incal.conf -f INBOX -f "Sent Items" search UNSEEN
mail-imap --config incal.conf -f "INBOX,Sent Items" search UNSEEN
```

`-f` also takes IMAP `LIST` patterns: `S*` matches every mailbox
starting with S, `Archive/*` crosses the hierarchy delimiter where
`Archive/%` would not. `-A`/`--all-folders` is shorthand for `-f '*'`.

```bash
mail-imap --config incal.conf -f 'S*' search UNSEEN
mail-imap --config incal.conf -A search UNSEEN
```

Sorting applies to `search` and `unread`, most recent first by default.
Server-side `UID SORT` (RFC 5256) where the server advertises `SORT`,
client-side otherwise.

```bash
mail-imap --config incal.conf -S -date -f INBOX search ALL
mail-imap --config incal.conf -S "subject,-size" -f INBOX search UNSEEN
```

#### Reading

A selection is `[FOLDER::]UIDS`: separate arguments, a comma list, a
range, or `*` for every message — quote it, the shell globs it. A
folder-qualified selection reaches another folder without touching
`-f`.

```bash
mail-imap --config incal.conf -f INBOX read 12345
mail-imap --config incal.conf -f INBOX read 12345 67890
mail-imap --config incal.conf -f INBOX read 12345,67890   # same thing
mail-imap --config incal.conf -f INBOX read 1-50
mail-imap --config incal.conf -f INBOX read '*'
mail-imap --config incal.conf read Archive::12345 "Sent Items::1-5"
```

#### Moving mail

The folder is named last, as `mv` has it. Needs `"access-level":
"organize"`, and the target folder has to exist already.

```bash
mail-imap --config incal.conf move 12345 Archive/2026
mail-imap --config incal.conf -f INBOX move last:20 Archive/2026
mail-imap --config incal.conf move 'INBOX::1-5' 'Spam::9' Trash
```

#### Counts, UIDs and threads

```bash
# Message counts / status (every selectable mailbox with no -f/-A, or
# the selected folder(s))
mail-imap --config incal.conf count
mail-imap --config incal.conf -f INBOX count
mail-imap --config incal.conf -f INBOX status   # "status" is an alias of "count"

# List the message UIDs of the selected folder(s), one block per folder
mail-imap --config incal.conf -f INBOX uid
mail-imap --config incal.conf -A uid

# List unread emails of the selected folder(s)
mail-imap --config incal.conf -f INBOX unread
mail-imap --config incal.conf -f INBOX -f Archive unread
```

Threading needs no server `THREAD` extension: without one the tool
reads the Message-ID / References / In-Reply-To headers of the
folder's messages as raw header literals and reconstructs the thread.

```bash
mail-imap --config incal.conf -f INBOX thread 12345
```

#### MIME parts

`part save` writes the part's own filename, else `uid<N>_part<M>`, into
the current directory; `-o` chooses a path. The selection must name
exactly one message.

```bash
mail-imap --config incal.conf -f INBOX part list 12345
mail-imap --config incal.conf -f INBOX part list 12345 67890
mail-imap --config incal.conf -f INBOX part save 12345 2 -o /tmp/invoice.pdf
```

#### Setting flags and tags

`flag` owns the IMAP-defined flags (`\Seen`, `\Answered`, `\Flagged`,
`\Deleted`, `\Draft`); `\Recent` is server-managed and rejected. `tag`
owns user-defined keywords and refuses system flags.

```bash
mail-imap --config incal.conf -f INBOX flag list 12345 67890
mail-imap --config incal.conf -f INBOX flag add 12345 67890 flagged
mail-imap --config incal.conf -f INBOX flag remove 12345 seen

mail-imap --config incal.conf -f INBOX tag list 12345
mail-imap --config incal.conf -f INBOX tag add 12345 invoice '$Important'
mail-imap --config incal.conf -f INBOX tag remove 12345 invoice
```

A listing prints what `add` takes back; `--wire` prints the atoms the
server sent, which `add --wire` takes back in turn.

```bash
mail-imap --config incal.conf -f INBOX flag list --wire 12345
```

The keywords with an agreed meaning — the IANA registry, then what
clients write without one — need no server:

```bash
mail-imap --mock tag known
```

#### JSON, and trying it without a server

```bash
# One compact JSON object on stdout
mail-imap --config incal.conf -j -f INBOX search "SINCE 01-Jan-2026"

# The in-memory mock backend: no server, no config
mail-imap --mock folder list
mail-imap --mock search invoice
```


### JSON output (`-j`)

Each command prints one compact JSON object to stdout:

| Command                                                  | Shape                                                                                                                                                                                                                       |
| -------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `info`                                                   | `{"tool", "config", "access", "folders", "defaults", "server"}` — see [`info`](#info) for the fields of each                                                                                                                |
| `folder list`                                            | `{"count", "folders": [{"name", "delimiter", "no_inferiors", "attrs"}]}` (`delimiter` is `null` for a mailbox reported with none; `attrs` carries `\Marked` and the RFC 6154 special uses)                                  |
| `folder create` / `rename` / `subscribe` / `unsubscribe` | `{"action", "folder"}`, plus `"to"` for a `rename` and `"use"` for a `create` that declared a special use                                                                                                                   |
| `search`                                                 | one folder: `{"folder", "query", "count", "results": [...]}`; several folders: `{"folders": [...], "query", "count", "results": [...]}`. Each result includes `"folder"` (its mailbox) and `"parts"` (number of MIME parts) |
| `read`                                                   | one `{"folder", "uid", "content"}` object per selected UID                                                                                                                                                                  |
| `count` / `status`                                       | `{"all", "counts": [{"name", "messages", "unseen", "recent", "uid_next", "uid_validity"}]}`                                                                                                                                 |
| `uid`                                                    | one `{"folder", "count", "uids": [1, 2, ...]}` object per selected folder                                                                                                                                                   |
| `thread`                                                 | one `{"folder", "uid", "count", "uids": [1, 2, ...]}` object per selected message (all UIDs of the thread containing `uid`, ascending, `uid` included)                                                                      |
| `unread`                                                 | same shape as `search` (query fixed to `UNSEEN`, folder(s) + part counts included)                                                                                                                                          |
| `part list`                                              | one `{"folder", "uid", "count", "parts": [{"part", "content_type", "filename", "size"}]}` per selected UID                                                                                                                  |
| `part save`                                              | `{"folder", "uid", "part", "file", "size"}`                                                                                                                                                                                 |
| `move`                                                   | one `{"folder", "to", "count", "uids": [...]}` object per selected folder (`folder` is where the messages came from, `to` where they went)                                                                                  |
| `flag list` / `tag list`                                 | one `{"folder", "uid", "count", "flags": [...]}` per selected UID (`\Recent` omitted; `tag list` keeps keywords only)                                                                                                       |
| `flag add` / `flag remove` / `tag add` / `tag remove`    | one `{"folder", "count", "uids": [...], "added": [...], "removed": [...]}` object per selected folder (`added` populated by add, `removed` by remove)                                                                       |
| `tag junk` / `tag notjunk`                               | the shape of `tag add` — one object per selected folder, `added` carrying every spelling set and `removed` every spelling cleared                                                                                           |
| `tag known`                                              | `{"count", "registered": [{"keyword", "means"}], "well_known": [{"keyword", "means"}]}`                                                                                                                                     |

Errors are printed as `{"error": "..."}` on stderr with a non-zero exit code.

### Global flags

Every one of these is global: it may appear before or after the
command.

- **`-c, --config <PATH>`** — the config file. Without it, the search
  order is `$MAIL_IMAP_CONFIG`, then `~/.config/mail-imap.conf`, then
  `/etc/mail-imap.conf`. A file named by `-c` or by the environment
  must exist: it is an answer, not a candidate, so a missing one is an
  error rather than a reason to read another account's config.
- **`-f, --folder <NAME>`** — folder(s) to operate on: a literal name,
  or an IMAP `LIST` pattern (`*` crosses the hierarchy delimiter, `%`
  does not). Repeatable and comma-separated, both meaning the same
  thing. Default: `folder` from the config (`INBOX`) — except `count`,
  which defaults to every mailbox. An empty name (`-f ''`) is refused.
- **`-A, --all-folders`** — every selectable mailbox of the account;
  exactly `-f '*'`.
- **`--access-level <LEVEL>`** — narrow what this run may change:
  `readonly`, `organize`, `restructure`, `full`. It can only lower what
  the config allows, never raise it. See
  [Access level](#access-level).
- **`-S, --sort <SPEC>`** — sort `search`/`unread` results.
  Comma-separated criteria (`uid`, `date`, `arrival`, `size`,
  `subject`, `from`, `to`, `cc`), first is primary, a `-` prefix makes
  one descending (`-date`, `subject,-size`). Overrides `sort` from the
  config. Default: most recent first.
- **`-M, --max <N>`** — cap search results for this run, overriding
  `max` from the config. `0` means unlimited.
- **`-j, --json`** — one compact single-line JSON object per result on
  stdout; errors become `{"error": ...}` on stderr. See
  [JSON output](#json-output--j).
- **`-d, --debug`** — trace the exchange on stderr: the backend and
  access level in force, the server's capabilities, and which threading
  and sort paths were taken.
- **`--mock`** — use the in-memory mock backend instead of a real
  server. Needs no config; an unreadable one is ignored.
- **`-h, --help`** — print help and exit. Also on every subcommand
  (`mail-imap flag add --help`).
- **`-V, --version`** — print the version and exit.

## Configuration

Where it is looked for, in order — the first that exists is the one
read, and `info` reports which:

```text
  -c PATH                     named outright ─┐  must exist; a missing
  $MAIL_IMAP_CONFIG           from the env   ─┘  one is an error

  ~/.config/mail-imap.conf    yours          ─┐  searched, in this
  /etc/mail-imap.conf         the machine's  ─┘  order
```

`$XDG_CONFIG_HOME` replaces `~/.config` where it is set.

The config file is [UCL](https://github.com/vstakhov/libucl) — the
Universal Configuration Language, the format `pkg.conf` and the rest
of FreeBSD's userland use. All fields except `server`, `username` and
`password` have defaults, so the smallest working config is the three
of them; [`example.conf`](example.conf) is a template with every field
written out at its default.

```nginx
server   = "imap.example.com"
port     = 993
username = "user@example.com"
password = "secret"

ssl      = true
starttls = false
insecure = false

folder = "INBOX"
max    = 50

# sort      = "-date"
# delimiter = "/"

access-level = organize
```

**UCL is a superset of JSON**, so a config written as a JSON object is
read unchanged — [`example-config.json`](example-config.json) is the
same settings in that form, and any config written before the switch
keeps working. What UCL adds is the part a hand-edited file wants:
comments, bare keys, no commas, no outer braces, and unquoted values
where they are unambiguous.

One deliberate narrowing: a bare `30s` is a *duration* in UCL, and
nothing here is a duration, so the parser is told not to read one. A
password of `30s` stays the text `30s` rather than becoming a number.

| Field          | Default    | Description                                                                                                               |
| -------------- | ---------- | ------------------------------------------------------------------------------------------------------------------------- |
| `server`       | —          | IMAP host                                                                                                                 |
| `port`         | `993`      | IMAP port                                                                                                                 |
| `username`     | —          | Login user                                                                                                                |
| `password`     | —          | Login password (use an app password for e.g. Gmail)                                                                       |
| `ssl`          | `true`     | Implicit TLS (typical for port 993)                                                                                       |
| `starttls`     | `false`    | Upgrade a plain connection with STARTTLS (typical for port 143). Used when `ssl` is `false`.                              |
| `insecure`     | `false`    | Accept invalid TLS certificates (self-signed local servers)                                                               |
| `folder`       | `INBOX`    | Default folder for commands that need one                                                                                 |
| `max`          | `50`       | Max search results to fetch (`0` = unlimited); overridden by `-M/--max` on the command line                               |
| `sort`         | `null`     | Default sort spec for `search`/`unread` (same format as `-S/--sort`); overridden by `-S` on the command line              |
| `mock`         | `false`    | Use the in-memory mock backend                                                                                            |
| `delimiter`    | `null`     | The hierarchy delimiter `info` reports, overriding the server's own answer. Advisory only: nothing rewrites a folder name |
| `access-level` | `organize` | How much this tool may change: `readonly`, `organize`, `restructure` or `full` — see [Access level](#access-level)        |

### Access level

`access-level` in the config says how much of the account this tool may
change. The levels are a ladder, each permitting everything below it:

| Level         | Permits                                                                              |
| ------------- | ------------------------------------------------------------------------------------ |
| `readonly`    | Nothing changes. Reads use `BODY.PEEK[]`, so even `\Seen` stays as it was            |
| `organize`    | *(default)* Read, plus set and clear flags and tags, and move mail to another folder |
| `restructure` | That, plus the folder tree: `folder create`, `rename`, `subscribe`, `unsubscribe`    |
| `full`        | Everything the tool can do, including setting `\Deleted`                             |

The two lines the ladder draws: `organize` is about **messages** —
nothing is lost, so `\Deleted` cannot be *set* (it can be cleared,
which rescues one), and the folder tree is left exactly as it was
found. `restructure` is about the **tree** — it may be added to and
renamed, and still nothing is lost, which is why deleting a mailbox
sits in `full`. [DESIGN.md](DESIGN.md#access-level) has the reasoning
and where the gate lives.

Renaming INBOX is refused at *every* level, `full` included.

`--access-level LEVEL` narrows a single run. It can only lower what the
config allows, never raise it, so a config saying `readonly` cannot be
talked out of it on the command line.

Two older spellings are still accepted wherever a level is named:
`read-only` for `readonly`, `non-destructive` for `organize`. The config
key is read as either `access-level` or `access_level`.

The two places differ in how forgiving they are, and deliberately.
`--access-level` trims and lowercases what it is given, so `FULL` and
` full ` both work. The config file does not: the value must be one of
the spellings exactly as written above, and anything else — a typo, a
capital — makes the whole file fail to parse rather than quietly
becoming a level nobody chose. A run that cannot read its access level
does not get to guess at it.

This is not a security boundary: the same account can be reached by any
other client with the same password. It is a guard against *this* tool
doing more than you meant it to — which matters most when an agent is
driving it.

Moving mail needs `MOVE` (RFC 6851) or `UIDPLUS` (RFC 4315) on the
server; with neither, `move` is **refused** rather than served. `info`
reports which of the two paths this account will take — see
[Moving mail](DESIGN.md#moving-mail-move) for why the third is a
refusal.

## Testing

```bash
# The whole suite: the Rust tests, then every documented command line
make tests

# Just the Rust tests
make tests-unit          # or plain: cargo test

# Just the documented command lines
make tests-examples      # or: ./check-examples.sh
```

Neither half needs a server or a config.

The suite runs entirely against the mock backend: it opens no socket and
needs no config. It covers the mock backend end-to-end (list / search /
read / count / uid / unread / thread / part), the access-level ladder —
every refusal has a test that feeds it the forbidden operation — the
message-selection grammar and folder-pattern matching, the keyword layer
(the IANA registry, modified UTF-7, Thunderbird's tag keys), what `info`
reports, the MIME parser, the RFC 2047 decoder, and that the real
backend fails cleanly rather than fabricating data when no server is
reachable. [DESIGN.md](DESIGN.md#testing) lists it in full.

`check-examples.sh` is the second half: it extracts every `mail-imap`
command line these documents print, replays it against `--mock`, and
fails if the CLI rejects one. The Rust suite calls the `cli::`
functions directly, so an argument shape broken in `src/main.rs` passes
it — this is what catches that, and it is why an example that stops
working is a build failure rather than a surprise for a reader.

What neither half proves: nothing here fails because the real backend
sent the wrong thing to a server, and a command shape that appears in
no document is checked by nothing. Both are covered by hand — see
[CHECKLIST.md](CHECKLIST.md).

## Architecture

```text
src/
  main.rs          CLI entry (clap)
  lib.rs           the same modules as a library, for the test suite
  config/mod.rs    JSON config loading, the access-level ladder
  cli/
    mod.rs         command handlers / output formatting, per-folder UID groups
    select.rs      message selection grammar, folder-pattern matching/expansion
    keywords.rs    the IANA keyword registry and the conventions around it
    modutf7.rs     modified UTF-7 (RFC 3501 5.1.3) encode/decode
    tbkey.rs       Thunderbird's '=xx' tag keys, read back as text
  imap/
    mod.rs         ImapBackend trait, shared types, backend selection
    real.rs        RealClient  — talks to a real IMAP server (imap crate)
    mock.rs        MockClient  — in-memory mock, used by the test suite
    mime.rs        minimal MIME parser (parts, boundary, CTE decoding)
    sort.rs        --sort spec parsing + client-side result ordering
```

See `DESIGN.md` for details.

## Security note

`*.conf` files are git-ignored because they contain credentials. Keep real
configs (e.g. `incal.conf`) out of version control.

## License

MIT
