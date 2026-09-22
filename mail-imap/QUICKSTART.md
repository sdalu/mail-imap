# mail-imap quickstart

mail-imap is a command-line IMAP client for programmatic and AI use.
Reads change nothing on the server -- they use `BODY.PEEK[]` -- and
everything else is capped by an `access-level` in the config
(`readonly` / `organize` / `restructure` / `full`).

## Point it at a real account

A real account needs four fields: where to connect, who as, and how
much mail-imap may change. The config is [UCL][ucl] -- comments, bare
keys, no commas, no outer braces. Save this as `myaccount.conf` (or
point `-c` / `$MAIL_IMAP_CONFIG` at it from anywhere, or drop it at
`~/.config/mail-imap.conf` and pass nothing):

```nginx
server   = "imap.example.com"
username = "you@example.com"
password = "secret"

# readonly / organize / restructure / full
access-level = readonly
```

[ucl]: https://github.com/vstakhov/libucl

UCL is a superset of JSON, so the same thing written as a JSON object
works just as well -- a config you already have keeps working.

More than one account goes in the same file, each in a named block,
selected with `-p` — see [Several accounts in one
file](README.md#several-accounts-in-one-file).

`readonly` costs nothing while you get the shape right; `organize` is
the config's own default if the field is left out. See [Access
level](README.md#access-level) for what each of the four levels
permits.

Then a first query -- list, search, read:

```bash
$ mail-imap -c myaccount.conf folder list
$ mail-imap -c myaccount.conf search invoice
$ mail-imap -c myaccount.conf read 5
```

`info` is the one to run first, though: it reports what this run may
change, which file it read the settings from, the folder hierarchy
delimiter, and which wire path each operation will take on this
server.

```bash
$ mail-imap -c myaccount.conf info
```

A config that will not parse is reported before anything connects --
the error says `Configuration error:` and names the file -- so a typo
never shows up as a mysterious network failure. `-j` gives any of
these as one line of JSON, for scripting; see
[`info`](README.md#info) for every field.

Two commands need no account at all, and no config: `tag known`
(the keywords with an agreed meaning, read from a table in the
binary) and `--version`.

## What to read next

- [README.md](README.md) -- every command, flag and config field.
- [DESIGN.md](DESIGN.md) -- why it is shaped this way, including the
  in-memory mock backend that development builds carry for running the
  suite without a server.
- `mail-imap.1` -- the man page (`man mail-imap` after
  `make install`).
