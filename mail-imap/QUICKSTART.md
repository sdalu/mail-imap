# mail-imap quickstart

mail-imap is a command-line IMAP client for programmatic and AI use.
Reads change nothing on the server -- they use `BODY.PEEK[]` -- and
everything else is capped by an `access-level` in the config
(`readonly` / `organize` / `restructure` / `full`).

## Try it with no server

An in-memory mock backend ships with the binary: no server, no
config, nothing to set up. Build once with `make build RELEASE=no`
(or `make build` for a release binary; see
[Build & Install](README.md#build--install)), then paste these:

```bash
$ target/debug/mail-imap --mock folder
Folders (5):
  - INBOX
  - Sent Items
  - Drafts
  - Trash
  - Spam

$ target/debug/mail-imap --mock search invoice
Found 1 email(s) in 'INBOX' matching: invoice
  UID 5 | 2026-09-20 12:00:00 +0000 | Your invoice | billing@example.com  [120 bytes]  [2 part(s)]

$ target/debug/mail-imap --mock read 5
Subject: Your invoice
From: billing@example.com
Date: 2026-09-20 12:00:00 +0000

Invoice #42 is due.
```

That's a full first query -- list, search, read -- against canned
data, no account needed. `-j` gives the same thing as one line of
JSON, for scripting:

```bash
$ target/debug/mail-imap --mock -j info
{"tool":{"name":"mail-imap","version":"0.1.0","backend":"mock"},
 "config":{"path":null,"server":"localhost", ...}, "access":{...},
 "folders":{...}, "defaults":{...}, "server":{...}}
```

(trimmed and wrapped for this page; the real output is one line --
see [`info`](README.md#info) for every field.)

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

`readonly` costs nothing while you get the shape right; `organize` is
the config's own default if the field is left out. See [Access
level](README.md#access-level) for what each of the four levels
permits.

The same `--mock` commands above work unchanged once you swap
`--mock` for `-c myaccount.conf`: `folder`, `search invoice`, `read
5` (or whatever `search` finds) now reach the real server instead of
the canned mock data. To confirm the file itself parses before
risking a connection, run it with `--mock` still attached -- the
backend stays mock, but the config really is read:

```bash
$ target/debug/mail-imap -c myaccount.conf --mock info
mail-imap 0.1.0 (mock backend)
  config      myaccount.conf
  account     you@example.com@imap.example.com:993 (implicit TLS)
  ...
```

(trimmed -- `info` also reports the access level, the folder
hierarchy delimiter, and which wire path each operation takes; see
[`info`](README.md#info).)

## What to read next

- [README.md](README.md) -- every command, flag and config field.
- [DESIGN.md](DESIGN.md) -- why it is shaped this way.
- `mail-imap.1` -- the man page (`man mail-imap` after
  `make install`).
