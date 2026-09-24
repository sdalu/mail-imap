# mail-imap quickstart

mail-imap is a command-line IMAP client for programmatic and AI use.
Reads change nothing on the server -- they use `BODY.PEEK[]` -- and
everything else is capped by an `access-level` in the config
(`survey` / `organize` / `restructure` / `full`).

## Point it at a real account

A real account needs four fields: where to connect, who as, and how
much mail-imap may change. The config is [UCL][ucl] -- comments, bare
keys, no commas, no outer braces. Save it as
`~/.config/mail-imap.conf`, which is where mail-imap looks when no
config is named (or keep it anywhere and point `-c` /
`$MAIL_IMAP_CONFIG` at it):

```nginx
server   = "imap.example.com"
username = "you@example.com"
password = "secret"

# survey / organize / restructure / full
access-level = survey
```

[ucl]: https://github.com/vstakhov/libucl

UCL is a superset of JSON, so the same thing written as a JSON object
works just as well -- a config you already have keeps working.

More than one account goes in the same file, each in a named block,
selected with `-p` — see [Several accounts in one
file](README.md#several-accounts-in-one-file).

`survey` costs nothing while you get the shape right; `organize` is
the config's own default if the field is left out. See [Access
level](README.md#access-level) for what each of the four levels
permits.

Then a first query -- list, search, read:

```bash
$ mail-imap folder list
$ mail-imap search 'TEXT invoice'
$ mail-imap read 5
```

`info` is the one to run first, though: it reports what this run may
change, which file it read the settings from, the folder hierarchy
delimiter, and which wire path each operation will take on this
server.

```bash
$ mail-imap info
```

A config that will not parse is reported before anything connects --
the error says `Configuration error:` and names the file -- so a typo
never shows up as a mysterious network failure. `-j` gives any of
these as one line of JSON, for scripting; see
[`info`](README.md#info) for every field.

The search query is an IMAP `SEARCH` expression, not a bare word:
`TEXT invoice` looks in the whole message, `SUBJECT invoice` only in
the subject, `UNSEEN` and `ALL` take no argument. A bare word is
refused by the server, not by this tool.

Three commands need no account at all, and no config: `tag known`
(the keywords with an agreed meaning, read from a table in the
binary), `completion bash|zsh|fish`, and `--version`.

One more thing worth doing early: the password need not sit in the
file. `password-command` runs a command and reads the password from
its output, so `pass`, `gpg` or a keyring can hold it instead — see
[Security note](README.md#security-note).

## What to read next

- [README.md](README.md) -- every command, flag and config field.
- [DESIGN.md](DESIGN.md) -- why it is shaped this way, including the
  in-memory mock backend that development builds carry for running the
  suite without a server.
- `mail-imap.1` -- the man page (`man mail-imap` after
  `make install`).
