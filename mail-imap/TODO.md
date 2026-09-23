# What is not here yet

Candidates, not commitments: gaps found by reading the command surface
against what IMAP offers and what this tool says it is for. Each entry
says what is missing, where the code would change, and what it costs.
An entry that turns out to be a deliberate boundary belongs in
DESIGN.md with its reason, not here — moving it there is a way of
closing it.

Ordered by how likely it is to bite.

## 2. Authentication is `LOGIN` and nothing else

*Half done.* `password-command` exists: the password can come from
`pass`, `gpg`, a keyring or anything else that prints it, and the
config need not hold it. What is left is the protocol half.

`establish_session` still calls `.login(user, password)`. There is no
`AUTHENTICATE`, so no XOAUTH2, no OAUTHBEARER, no SASL. Microsoft 365
has disabled basic auth; Gmail needs an app password. Between them
that is most of the accounts this tool would be pointed at, and it is
the reason this entry is still open.

What exists to build on: the fork has `Client::authenticate` and the
`Authenticator` trait (`../forks/rust-imap/src/client.rs:459`,
`src/authenticator.rs`), which is exactly the shape XOAUTH2 needs —
answer the server's challenge with
`user=<user>\x01auth=Bearer <token>\x01\x01`, base64-encoded. And
`password-command` already supplies the mechanism for getting a token
from outside, so a `token-command` is the same machinery under another
name rather than new machinery.

**The obstacle is not difficulty, it is verification.** GreenMail does
not speak XOAUTH2, so nothing in `tests/wire.rs` can cover the
handshake — this would be the first thing here that ships without a
gate behind it. The part that actually carries the bugs is the payload
construction, and that *is* unit-testable against the RFC 7628 form.
So: build it, test the payload exactly, and say plainly in DESIGN.md
that the exchange itself is unproven against a real provider until
someone runs it against one. Shipping that honestly is fine; shipping
it looking tested is not.

Note also that acquiring the token — the OAuth dance, refresh tokens,
a client secret — is deliberately *not* this tool's job. It takes a
token from a command, the way it takes a password from one.

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
- **Piping into `head` or `less` panics.** Rust ignores `SIGPIPE`, so
  a closed pipe surfaces as a failed write: `read '*' | head` ends in
  `failed printing to stdout: Broken pipe`. The fix is one line
  restoring the default disposition at startup, and it wants `libc` as
  a direct dependency — which is why it is a line here rather than
  something folded into an unrelated commit.
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
`folder list --subscribed`), §4 (`copy` and `expunge`), §5 (`append`),
§6 (`part strip`) and §1 (`read` showing the readable text). What they
left behind is recorded where it belongs rather than here: the untested
charset fallback in DESIGN.md under *Declaring a charset on `SEARCH`*,
why `fetch_part` is the trait's primitive under *MIME parsing*, where
the line falls between `restructure` and `full` under *Access level*,
why `expunge` never marks `\Deleted` itself under *Copying and
expunging*, why a lone LF is normalised before an `append` under
*Putting a message in*, and why `part strip` writes before it deletes
under *Stripping a part*, and why the message renderer sits in one
place rather than in each backend under *Reading a message*.

The numbers of what remains do not close up as entries leave: §1, §2
and §7 keep the numbers they were given. Nothing cites another entry
any more — §6 was the last that did — but renumbering now would break
every reference to this file from a commit message, which is where the
reasoning for each of these lives.
