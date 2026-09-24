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
- `TODO.md` — what is not here yet: the gaps between what IMAP offers
  and what this tool does, each with where the code would change and
  what it costs. Candidates, not commitments; one that turns out to be
  a deliberate boundary moves to DESIGN.md with its reason.
- `man/mail-imap.1` — the man page (mdoc). The same surface as the
  README's reference sections, in the form `man` expects; `make check`
  lints it, `make install` puts it under `MANDIR`.
- `Makefile` — the interface. `make` alone prints it.

Gate: `make check && make tests`

## Traps

- **The `imap` and `imap-proto` dependencies are local forks**, carried
  as submodules under `forks/` (`forks/rust-imap`,
  `forks/tokio-imap/imap-proto`), carrying RFC 5256 THREAD support that
  upstream has not released (`SORT` it already has), and
  `ClientBuilder::timeout` — without which nothing can bound the
  connect phase, because it all happens before a `Client` exists.
  **Clone with `--recurse-submodules`**, or `cargo build` has nothing
  to point at; an existing clone catches up with `git submodule update
  --init`. The commit pins the fork, not a version number, so a release
  here says which fork commit it was built against and nothing else
  does.
- **`tests/replay.rs` is a scripted socket, not a server.** It speaks
  just enough IMAP to walk the real backend down `fetch_chunk`'s
  degradation ladder -- answering badly on purpose, which no real
  server can be asked to do -- so the last rung
  (`fetch_to_result_from_headers`) is covered offline. It needs no
  server and is not `#[ignore]`d. One trap if you extend it: serve each
  connection on its own thread. A refused ENVELOPE poisons the stream
  and the client reconnects, and a single-threaded script never accepts
  that second connection -- the client then blocks reading a greeting
  that never comes. `timeout` bounds that now (the fork's
  `ClientBuilder::timeout`, added because of exactly this), so it fails
  in a second instead of hanging for good -- but with `timeout = 0`, or
  a script that stalls some other way, it can still look exactly like a
  defect in the code under test.
- **`make tests` proves the mock backend, not the wire.** It never
  opens a socket, so nothing in it can fail because
  `src/imap/real.rs` sends the wrong thing. `make tests-wire` is the
  other half: it starts a throwaway GreenMail, runs `tests/wire.rs`
  against it, and stops it. Those tests are `#[ignore]`d so the
  ordinary suite stays server-free and shows them as skipped rather
  than hiding them, and they fail loudly rather than pass quietly when
  no server is there. They cover what the mock cannot — that reads use
  `BODY.PEEK` and leave `\Seen` alone, UID order, the result cap,
  server-side `UID SORT`, `UID STORE`, `UID MOVE`, the folder tree,
  MIME over the wire, threading. What they still do not cover is a
  *real* account's quirks (the degradation ladders in `fetch_chunk`
  exist for servers GreenMail is not) — see CHECKLIST.md.
- **The Rust suite barely drives the CLI surface.** It calls the
  `cli::` functions, so an argument shape broken in `src/main.rs`
  passes it — except for the few `src/main.rs`'s own test module pins
  (the selection/name split, the folder spec, that a global option is
  taken on either side of the command). `make tests` therefore also
  runs `scripts/check-examples.sh`, which replays every command line
  the documents print against `--mock` and fails on a clap usage
  error. That covers the *documented* shapes only: a shape nobody
  wrote down is still unchecked, so run `--mock` invocations of what
  you changed.
- **The mock is behind a default-on `mock` cargo feature, and the
  release build turns it off** (`F_yes = --no-default-features` in the
  Makefile). So `make build` produces a binary where `--mock` is an
  unknown argument, while `make build RELEASE=no` keeps it -- and the
  suite, `scripts/check-examples.sh` and QUICKSTART all need the
  development build. `make check` lints both configurations, because
  `cfg`-gated code that only compiles one way is the failure this
  invites.
- **The mock is held to the real server's behaviour, not guessed at.**
  `tests/wire.rs::the_mock_answers_like_a_real_server` runs the same
  probes against both and requires them to agree on which calls are
  refused. Before it existed the mock was *stricter* than a real
  server -- it refused a `UID STORE` to a UID that does not exist,
  where the server answers OK -- and that is what hid the `tag junk`
  defect from the offline suite. A fake being stricter than the thing
  it stands in for turns a live bug into an offline pass.
- **The config is UCL, and the UCL parser is built from source.** The
  `libucl` crate pulls `libucl-bind`, whose `build.rs` runs **cmake**
  over a *vendored libucl 0.5.0* and links it statically — it does not
  use this host's `libucl` package (0.9.4), and it drags **clap 2.34**
  into a tree that already has clap 4. So: cmake and a C compiler are
  build requirements, the binary has no runtime libucl dependency, and
  the UCL dialect understood is 0.5.0's, not the system's.
- **GreenMail advertises `AUTH=XOAUTH2` and cannot complete it.** It
  answers the `AUTHENTICATE` with *"Missing argument. Command should be
  `<tag> AUTHENTICATE <auth_type> *(CRLF base64)`"* — it wants the
  initial response inline (RFC 4959 SASL-IR), which the `imap` crate
  does not send. So the throwaway server can neither complete an
  XOAUTH2 exchange nor exercise the missing-capability refusal, and
  `auth = "xoauth2"` is **the one path in this tree with no end-to-end
  test behind it**. The SASL payload is unit-tested byte for byte;
  the handshake against a real provider is not tested at all. Anyone
  touching `establish_session` should know that the suite will not
  catch them there.
- **`--access-level` can only narrow, never widen.** So a `full`-level
  path (`expunge`, `append`, `part strip`, `folder delete`) cannot be
  exercised by adding a flag to a default run: it needs a config file
  saying `access-level = "full"`. With `mock = true` in it, that costs
  nothing and touches no account — which is how the `full` commands
  get run by hand.
- **`scripts/check-examples.sh` reads four documents only** — README.md,
  QUICKSTART.md, DESIGN.md and `man/mail-imap.1`. A command line
  written anywhere else, TODO.md included, is replayed by nothing.
- **`cargo mutants` copies the tree to a scratch directory**, which is
  where a run here fails rather than in the code under test: the
  baseline build dies in the copy while the real tree is fine.
  `--in-place` runs it where the files are instead, and whether this
  host needs that is in CLAUDE.local.md rather than here — it is a fact
  about the machine's scratch space, not about the repository.
  `.gitignore` carries `mutants.out/`, so this has been run here
  before. **A run that finishes cleanly can still leave a mutant in the
  tree** -- one here exited 0 having left `required_capability`
  returning `Some("")` -- so `git status` and `git diff` after every
  run, before anything else.
  An unnoticed leftover is a deliberately broken line committed as if
  it were yours. A run over
  `src/imap/real.rs` also needs the wire runner, or every mutant in it
  "survives" because the ignored wire tests never ran — CHECKLIST.md
  carries that command line, and the survivors already accounted for.
- **The tree is not rustfmt-clean.** Do not run `cargo fmt` across it as
  part of another change: the reformatting of untouched code buries the
  diff. Format the lines you write.
- **`*.conf` is gitignored** because real configs hold credentials.
  `--mock` needs none, and swallows more than a missing one: any
  `load_config` error — unparseable UCL, a bad `access-level` — falls
  back to `Config::default()` (`src/main.rs`), so a config typo under
  `--mock` shows up as defaults rather than as an error. `info` names
  the file it actually read, or says none was.
- **A release is tagged with `make tag`**, which reads the number out
  of `Cargo.toml` so it is never typed twice, refuses an unclean
  worktree or an existing tag, runs `make check` and the suite on the
  way, and pushes nothing. `make check` gates the other direction:
  `scripts/checktag.sh` refuses a tree whose number and nearest tag
  disagree.
