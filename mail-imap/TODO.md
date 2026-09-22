# What is not here yet

Candidates, not commitments: gaps found by reading the command surface
against what IMAP offers and what this tool says it is for. Each entry
says what is missing, where the code would change, and what it costs.
An entry that turns out to be a deliberate boundary belongs in
DESIGN.md with its reason, not here — moving it there is a way of
closing it.

Ordered by how likely it is to bite.

## 1. `read` returns the raw MIME body

`get_email` builds a short header summary and then appends
`BODY.PEEK[]` verbatim (`src/imap/real.rs:714`). For a multipart
message that is boundary lines and base64 — the command an
AI caller reaches for first returns the least readable thing the tool
produces.

The parser to fix it is already here: `src/imap/mime.rs` has
`parse_message` and `Part::decoded()`, wired only into `part list` and
`part save` (`src/imap/real.rs:822,846`). What is missing is a `read`
that picks the `text/plain` leaf, falls back to `text/html` with the
tags stripped, and keeps today's behaviour under `--raw`.

Decide, when it is written, what JSON carries: text output may show
less than JSON but never something different (DESIGN.md, *Output*), so
`content` cannot quietly become the decoded text while the raw body
disappears.

## 2. Authentication is `LOGIN` and nothing else

`establish_session` calls `.login(user, password)`
(`src/imap/real.rs:117`). There is no `AUTHENTICATE`, so no XOAUTH2, no
OAUTHBEARER, no SASL. Microsoft 365 has disabled basic auth; Gmail
needs an app password. Between them that is most of the accounts this
tool would be pointed at.

The credential is also a plain string in the config
(`src/config/mod.rs:108`): no `password-command`, no environment
variable, no keyring. The README's security note is only that the file
is gitignored (`README.md:1065`), which protects the repository and not
the file.

A `password-command` field is the small half of this and is worth
doing on its own — it is one config field, one `std::process::Command`,
and it makes `pass`, `gpg` and a keyring all work without this tool
knowing about any of them.

## 4. No `expunge`, and no `copy`

`access-level full` exists for one thing — permitting `\Deleted` to be
set (`src/config/mod.rs:59`) — and nothing ever removes what it marks.
That interacts with the move fallback: on a server without UIDPLUS,
`move` refuses precisely because a plain `EXPUNGE` would take every
`\Deleted` message with it (`src/imap/real.rs:931-938`), including the
ones this tool marked. So the tool can leave a mailbox in a state its
own `move` then refuses to work in.

`copy` is missing for no reason at all: `uid_copy` is already called in
the move fallback (`src/imap/real.rs:966`), so the command is a handler
and a gate, not new wire work. It is `organize`'s operation as much as
`move` is — the message keeps existing, in one more place.

## 5. No `append`

Nothing puts a message *into* a mailbox: no draft upload, no `.eml`
import, no re-filing something saved elsewhere. Every other direction
is covered. `APPEND` takes an optional flag list and internaldate
(RFC 3501 §6.3.11), and both have to be passed or the imported message
arrives unflagged and dated now.

This is also the operation §6 below is built on.

## 6. `part strip`

Decided: the command is `part strip`, not `part remove`. Removing a
MIME part is not something IMAP can be asked to do, and a name that
implies otherwise sets up the surprise. What the command does is
replace a message with a copy of itself that no longer carries a
given part — and the name should say the narrow thing it is for.

The use is archives: a mailbox is 40 GB because of attachments nobody
will open again, and the mail itself is worth keeping. Thunderbird
calls it *Detach*.

### Shape

```
part strip <SELECTION> <PART>...        # part numbers as `part list` prints them
```

One message per invocation, as `part save` already requires: the part
numbers are read off a listing of *that* message, so a selection
naming several has no coherent reading.

### What it does on the wire

A message on an IMAP server is immutable — there is no command that
edits one in place. So:

1. `UID FETCH <uid> (FLAGS INTERNALDATE BODY.PEEK[])`;
2. rebuild the MIME with the named parts replaced by stubs;
3. `APPEND` the result to the same mailbox, carrying the original
   flags and internaldate (RFC 3501 §6.3.11 takes both — omit them and
   the archived message comes back unread and dated today);
4. read the new UID from `APPENDUID`;
5. `UID STORE <uid> +FLAGS (\Deleted)`, then `UID EXPUNGE <uid>`.

Write first, delete last, which is the order `move_messages` already
uses (`src/imap/real.rs:963`): if step 5 fails the mailbox holds two
copies and a human can choose, whereas the other order loses the
message when the rebuild is wrong.

**It requires UIDPLUS, and refuses without it.** Not for the reason
`move` does but for two: without `APPENDUID` there is no way to learn
which message was just written, and a plain `EXPUNGE` would take every
`\Deleted` message in the mailbox with it. Refusing is the same
judgement as `src/imap/real.rs:931-938`.

### The stub

The part is replaced, not deleted. A `text/plain` body naming what was
there — filename, content type, decoded size, the SHA-256 of what was
removed, and the date it was stripped — plus an `X-Mail-Imap-Stripped`
header on the message carrying the same, one line per stripped part:

```
X-Mail-Imap-Stripped: part=3; type="application/pdf";
    filename="invoice-2026-01.pdf"; size=4194304;
    sha256=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08;
    date="Tue, 22 Sep 2026 18:40:11 +0200"
```

Three reasons for the stub. Part numbers stay stable, so a `part list`
from before the strip still describes the message. A reader who goes
looking for the invoice finds out what happened to it, rather than
finding a message that never had one. And the digest makes the strip
**checkable**: the attachment saved to a disk archive can be matched
back to the message it came from, years later, by one `sha256 -q`
— without it, "size=4194304" is all anyone has, and the only way to
know whether the right file was kept is to remember.

**The digest is of the decoded bytes** — what `part save` would have
written, not the base64 as it sat on the wire. Two reasons: it is the
form a saved file is in, which is the whole point of being able to
compare; and the encoded form is not stable, since a server or a
gateway may re-wrap base64 at a different line length without changing
the attachment at all.

This is one crate — `sha2` — and it is worth the dependency for the
one property nothing else here provides: `part strip` is the only
operation that destroys something, and the digest is what makes the
destruction auditable after the fact.

### What the caller has to be told

**The UID changes.** This is the first operation here that does — `move`
changes the mailbox, `flag` changes the flags, neither invalidates a
UID a caller is holding. The text output and the JSON both have to
report old and new, and DESIGN.md's *Output* section needs the shape.

The rebuilt message also **fails DKIM**, and its `Received` chain no
longer describes it. Acceptable for an archive, and worth saying in
the README rather than leaving to be discovered.

### Access level

`full`. The ladder is about what may be lost, and this loses an
attachment — which is what `full` is already for (it exists to permit
`\Deleted`; `src/config/mod.rs:59`). A fifth level would be a config
compatibility break for one command.

The gate goes in `ImapClient` beside the others (`src/imap/mod.rs:363`),
not in the handler, and `info` gains a line for it — it is the one
operation that changes a UID, and `info` is where a caller finds out
what this run may do.

### What it needs first

§5 (`append`) and the expunge half of §4 — this command is both of them
plus a MIME writer. And the writer is the work: `src/imap/mime.rs` is a
parser only (`leaves`, `decoded`, `parse_message`, `decode_body`), so
what is missing is boundary generation, transfer-encoding and
`Content-Type` rewriting. Removal never changes a message's top-level
structure, so the writer needed here is the small one — it does not
have to promote a single-part message to `multipart/mixed`.

Before it ships it owes: a mock backend that accepts `APPEND` (the mock
is held to what a real server does — CLAUDE.md), a `tests/wire.rs` case
that strips a part from a real multipart message and checks the
surviving parts byte-for-byte, and a real account, because this is the
first path that can lose mail.

### Not the other direction

Adding a part to a message is composing mail, and this tool does not
send mail. What looks like a use for it — building a message with an
attachment to put in Drafts — is §5 and belongs to `append`.

## 7. Smaller, still real

- **No timeout on the session socket.**
  `TcpStream::connect_timeout` (`src/imap/real.rs:86`) builds a
  connection, sets `nodelay` on it, and drops it;
  `ClientBuilder::connect()` (line 110) then opens its own, untimed.
  A server that accepts and stalls hangs the CLI for good. The
  pre-connect is also a wasted round trip to delete.
- **No shell completions.** clap 4 is already a dependency;
  `clap_complete` plus a `make install` hook is cheap, and `install`
  already places the man page.
- **`info` advertises capabilities with no command behind them**
  (`README.md:314`): `QUOTA`, `IDLE`. Either give them commands
  (`quota`; a `watch`) or say in DESIGN.md that the line reports what
  the *server* is, not what this tool will do with it.

---

Done and out of this list: the `SEARCH` charset declaration,
`part save --all` / `-o -`, and §3 — `folder delete` and
`folder list --subscribed`. What they left behind is recorded where it
belongs rather than here: the untested charset fallback in DESIGN.md
under *Declaring a charset on `SEARCH`*, why `fetch_part` is the
trait's primitive under *MIME parsing*, and where the line falls
between `restructure` and `full` under *Access level*.

The numbers of what remains do not close up as entries leave. §6 cites
"§5 (`append`) and the expunge half of §4", and a renumbering that made
the list tidier would quietly make those citations point at the wrong
thing.
