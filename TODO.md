# What is not here yet

Candidates, not commitments: gaps found by reading the command surface
against what IMAP offers and what this tool says it is for. Each entry
says what is missing, where the code would change, and what it costs.
An entry that turns out to be a deliberate boundary belongs in
DESIGN.md with its reason, not here — moving it there is a way of
closing it.

One is left.

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

---

Everything else that stood here is closed; `git log` has the list, and
what each one decided is written where it is enforced rather than
repeated here — mostly DESIGN.md, which is the file to search. §1, §3
through §7 are all gone that way. The numbers do not close up as
entries leave: commit messages cite them, and renumbering would break
every reference to a file whose whole value is being citable.
