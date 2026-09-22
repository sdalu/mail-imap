# mail-imap

A command-line IMAP client for programmatic / AI use. What it may
change on the server is capped by `access-level` in the config
(`readonly` / `organize` / `full`, default `organize`), enforced in
`ImapClient` rather than in the handlers.

## Where things are

- `README.md` — what it does and every command, flag and config field.
- `DESIGN.md` — why it is shaped this way: the backends, the IMAP
  commands each operation sends, the degradation ladders, threading,
  sorting, and how a command line becomes per-folder UID groups.
- `CHECKLIST.md` — what has to be true before a round here is done.
- `Makefile` — the interface. `make` alone prints it.

Gate: `make check && make tests`

## Traps

- **The `imap` and `imap-proto` dependencies are local forks**
  (`../forks/rust-imap`, `../forks/tokio-imap/imap-proto`), carrying
  RFC 5256 THREAD/SORT support that upstream has not released. A
  checkout without `../forks` does not build.
- **The suite proves the mock backend, not the wire.** `make tests`
  never opens a socket, so nothing in it can fail because
  `src/imap/real.rs` sends the wrong thing. Changes there are tested by
  running the binary against a real account — see CHECKLIST.md.
- **The suite does not drive the CLI surface either.** It calls the
  `cli::` functions; an argument shape broken in `src/main.rs` still
  passes. Run `--mock` invocations of what you changed.
- **The tree is not rustfmt-clean.** Do not run `cargo fmt` across it as
  part of another change: the reformatting of untouched code buries the
  diff. Format the lines you write.
- **`*.conf` is gitignored** because real configs hold credentials.
  `--mock` needs none: it ignores a missing config entirely.
- **This tree is a subdirectory of the AiTools repository**, which owns
  the git history and the tags. There is no `make tag` here.
