# What is not here yet

Candidates, not commitments: gaps found by reading the command surface
against what IMAP offers and what this tool says it is for. Each entry
says what is missing, where the code would change, and what it costs.
An entry that turns out to be a deliberate boundary belongs in
DESIGN.md with its reason, not here — moving it there is a way of
closing it.

Ordered by how likely it is to bite.

## 2. OAUTHBEARER, and a real provider account

*Mostly done.* `password-command` keeps the secret out of the config
file, and `auth = "xoauth2"` covers the mechanism Google and Microsoft
actually use. Two things are left, and neither is large.

**OAUTHBEARER (RFC 7628) is not implemented.** It is the standardised
form of the same idea, and XOAUTH2 is the vendor one that predates it.
The shape is the same — an `Authenticator` returning a fixed string —
but the string differs (`n,a=<user>,\x01host=<host>\x01port=<port>\x01auth=Bearer <token>\x01\x01`)
and it needs the host and port, which `establish_session` has. Worth
adding when something asks for it; nothing does today, and every extra
mechanism is more surface with no test behind it.

**The XOAUTH2 exchange has never run against a real provider.** This
is the honest gap, recorded here as well as in DESIGN.md and the
README: the suite's throwaway server advertises `AUTH=XOAUTH2` and
then refuses the challenge flow, so the handshake is covered by
nothing. The payload is unit-tested against the documented form and
the capability gate is covered on the wire. What is missing is one run
against Gmail or Microsoft 365, and it needs an account this tree does
not have.

## 7. Smaller, still real

- **Client-side `--sort date` and `--sort arrival` are the same
  ordering.** Server-side they are distinct (`UID SORT` is sent `DATE`
  or `ARRIVAL`), but `SearchResult` carries only the internaldate, so
  the client-side fallback cannot tell them apart and silently treats
  `date` as `arrival`. Closing it means fetching each hit's `Date:`
  header — a header fetch on the very path that exists to avoid one —
  so it is worth doing only for someone who actually needs sent-date
  ordering on a server without `SORT`.
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
timeout, SIGPIPE, shell completions, `parse_flag_names` no longer
refusing an empty list, and the NIL hierarchy delimiter `%` used to
mis-default to `/`. What they
left behind is recorded where it belongs rather than here: the untested
charset fallback in DESIGN.md under *Declaring a charset on `SEARCH`*,
why `fetch_part` is the trait's primitive under *MIME parsing*, where
the line falls between `restructure` and `full` under *Access level*,
why `expunge` never marks `\Deleted` itself under *Copying and
expunging*, why a lone LF is normalised before an `append` under
*Putting a message in*, and why `part strip` writes before it deletes
under *Stripping a part*, why the message renderer sits in one
place rather than in each backend under *Reading a message*, and why a
NIL delimiter makes `%` behave like `*` for that mailbox — and why the
config's `delimiter` is not the fallback — under *Telling selections
from names*.

The numbers of what remains do not close up as entries leave: §1, §2
and §7 keep the numbers they were given. Nothing cites another entry
any more — §6 was the last that did — but renumbering now would break
every reference to this file from a commit message, which is where the
reasoning for each of these lives.
