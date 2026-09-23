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

- **`info` advertises capabilities with no command behind them**
  (`README.md`): `QUOTA`, `IDLE`. Either give them commands (`quota`;
  a `watch`) or say in DESIGN.md that the `advertises` line reports
  what the *server* is, not what this tool will do with it. The second
  is a paragraph and is probably the honest answer; the first is two
  features nobody has asked for.
- **The connect phase is still unbounded.** `timeout` covers a server
  that accepts and then goes quiet, and cannot cover the dial or the
  TLS handshake, which `ClientBuilder` owns — nor a stalled write,
  since `SetReadTimeout` has no counterpart. Closing either means
  patching the `imap` fork, which is a bigger decision than the fix:
  the forks are rebased by hand and CHECKLIST.md wants that debt
  shrinking. Worth doing only if a real account actually hangs there.

---

Done and out of this list: the `SEARCH` charset declaration,
`part save --all` / `-o -`, §3 (`folder delete` and
`folder list --subscribed`), §4 (`copy` and `expunge`), §5 (`append`),
§6 (`part strip`), §1 (`read` showing the readable text), the
`password-command` half of §2, and four of §7 — the session read
timeout, SIGPIPE, shell completions, and `parse_flag_names` no longer
refusing an empty list. What they
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
