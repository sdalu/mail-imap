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
- **`parse_flag_names` refuses an empty list**, which is a precondition
  of `flag add` / `tag add` rather than a fact about parsing names —
  and one `split_args` already enforces earlier, with a better message.
  It made `append --flag` optional awkward (the handler calls the
  parser only when names were given), and a unit test pins the bail, so
  moving it to the call sites that want it is a small change with its
  own test to update. Worth doing on its own, not inside a feature.
- **`info` advertises capabilities with no command behind them**
  (`README.md:314`): `QUOTA`, `IDLE`. Either give them commands
  (`quota`; a `watch`) or say in DESIGN.md that the line reports what
  the *server* is, not what this tool will do with it.

---

Done and out of this list: the `SEARCH` charset declaration,
`part save --all` / `-o -`, §3 (`folder delete` and
`folder list --subscribed`), §4 (`copy` and `expunge`), §5 (`append`)
and §6 (`part strip`). What they
left behind is recorded where it belongs rather than here: the untested
charset fallback in DESIGN.md under *Declaring a charset on `SEARCH`*,
why `fetch_part` is the trait's primitive under *MIME parsing*, where
the line falls between `restructure` and `full` under *Access level*,
why `expunge` never marks `\Deleted` itself under *Copying and
expunging*, why a lone LF is normalised before an `append` under
*Putting a message in*, and why `part strip` writes before it deletes
under *Stripping a part*.

The numbers of what remains do not close up as entries leave: §1, §2
and §7 keep the numbers they were given. Nothing cites another entry
any more — §6 was the last that did — but renumbering now would break
every reference to this file from a commit message, which is where the
reasoning for each of these lives.
