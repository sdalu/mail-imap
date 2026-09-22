# mail-imap

A command-line IMAP client for programmatic / AI use. What it may
change on the server is capped by `access-level` in the config
(`readonly` / `organize` / `restructure` / `full`, default `organize`),
enforced in `ImapClient` rather than in the handlers.

## Where things are

- `QUICKSTART.md` — the on-ramp: `--mock` in the first minute, then a
  four-field config. Nothing in it is unique to it; it is a path
  through what the README holds.
- `README.md` — what it does and every command, flag and config field.
- `DESIGN.md` — why it is shaped this way: the backends, the IMAP
  commands each operation sends, the degradation ladders, threading,
  sorting, and how a command line becomes per-folder UID groups.
- `CHECKLIST.md` — what has to be true before a round here is done.
- `mail-imap.1` — the man page (mdoc). The same surface as the
  README's reference sections, in the form `man` expects; `make check`
  lints it, `make install` puts it under `MANDIR`.
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
- **The Rust suite does not drive the CLI surface.** It calls the
  `cli::` functions, so an argument shape broken in `src/main.rs`
  passes it. `make tests` therefore also runs `check-examples.sh`,
  which replays every command line the documents print against
  `--mock` and fails on a clap usage error. That covers the
  *documented* shapes only: a shape nobody wrote down is still
  unchecked, so run `--mock` invocations of what you changed.
- **The config is UCL, and the UCL parser is built from source.** The
  `libucl` crate pulls `libucl-bind`, whose `build.rs` runs **cmake**
  over a *vendored libucl 0.5.0* and links it statically — it does not
  use this host's `libucl` package (0.9.4), and it drags **clap 2.34**
  into a tree that already has clap 4. So: cmake and a C compiler are
  build requirements, the binary has no runtime libucl dependency, and
  the UCL dialect understood is 0.5.0's, not the system's.
- **The tree is not rustfmt-clean.** Do not run `cargo fmt` across it as
  part of another change: the reformatting of untouched code buries the
  diff. Format the lines you write.
- **`*.conf` is gitignored** because real configs hold credentials.
  `--mock` needs none, and swallows more than a missing one: any
  `load_config` error — unparseable UCL, a bad `access-level` — falls
  back to `Config::default()` (`src/main.rs`), so a config typo under
  `--mock` shows up as defaults rather than as an error. `info` names
  the file it actually read, or says none was.
- **This tree is a subdirectory of the AiTools repository**, which owns
  the git history and the tags. There is no `make tag` here.
