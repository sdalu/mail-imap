# Closing a round on mail-imap

What has to be true before a unit of work here is done.

## Gates

Run in order. A red gate is answered before anything else, and the
answer is sometimes that the checker is wrong rather than the file.

- [ ] `make check` — clippy with warnings denied, the release number
      still written only in `Cargo.toml`, and `man/mail-imap.1` still
      well-formed mdoc
- [ ] `make tests` — the Rust suite against the mock backend, then
      `scripts/check-examples.sh`: every command line the documents print, run
      against `--mock`, failing on a clap usage error
- [ ] `make tests-wire` — the wire checks against a throwaway GreenMail
      (it starts and stops it). This is what fails when
      `src/imap/real.rs` sends the wrong thing; `make tests` cannot.
- [ ] `target/debug/mail-imap --mock <the commands this round touched>` —
      `scripts/check-examples.sh` covers the shapes that are written down. One
      this round added and did not document is covered by nothing, so
      run it. If it is worth running, it is worth an example.

## The real server

`make tests-wire` talks to a throwaway GreenMail, which is not the same
as an account in the wild: it is one server, advertising one set of
capabilities, with none of the quirks the degradation ladders in
`fetch_chunk` were written for. A round that changed what goes on the
wire still owes a real account.

- [ ] Did this round change what goes on the wire? Then `make tests-wire`
      passes, *and* it was run against a real account (`-c incal.conf`,
      `-d` to see the exchange) with the commands named in the commit.
- [ ] Did it change a mutating path (`flag`, `tag` — the only two)? Then
      it was run against a real account on a message that can be spared.
- [ ] Did it add an operation that changes the server? Then its gate is
      in `ImapClient`, beside the flag gate, and `access-level` says
      which level permits it — a handler is not where that rule lives.

## Documents

Re-read each against what this round changed: a document that was true
this morning is a claim, not a fact.

- [ ] `README.md` — does it name only flags, paths and commands that exist
      today, and does every example still run?
- [ ] `DESIGN.md` — did this round change what it says about the
      IMAP commands sent, the fallbacks, or the resolution of selections?
- [ ] `CLAUDE.md` — does it still point at files that exist, and is its
      gate command still the one that runs?
- [ ] `man/mail-imap.1` — a command, flag or config field changes in three
      places or none: the README table, the man page, and `--help`.
      `make check` proves the page parses, not that it is true.
- [ ] `QUICKSTART.md` — do its commands still run? Every one of them is
      meant to be runnable as written, most under `--mock`.
- [ ] **A documented `search` query is checked by nothing.**
      `scripts/check-examples.sh` proves that clap ACCEPTS a command
      line, not that a server accepts what it carries — the example
      configs it names do not exist, so those runs stop before any
      socket. QUICKSTART printed `search invoice` for several releases;
      a bare word is not an IMAP `SEARCH` key and a real server answers
      `BAD`. If a round adds or edits a `search` example, run it once
      against the throwaway server (`make tests-server-start`, then
      `-c tests-tmp/greenmail.conf`) before believing it.
- [ ] `example.conf` — does it still parse, and does it still show a
      profile block? It is the only place the multi-account shape is
      written out for someone to copy.
- [ ] `example.conf` — a config field that changed is written in four
      places: the struct, the README table, the man page, and this
      template. `scripts/check-examples.sh` does not read configs, so nothing
      catches a stale one but this line.
- [ ] **Does anything now ship without a gate behind it?** One thing
      does — the XOAUTH2 exchange (CLAUDE.md says why) — and it is
      named in three places: DESIGN.md for the reasoning, TODO.md as
      work still owed, and the README where a *user* will meet it. A
      second such thing should get the same treatment rather than a
      commit message alone, which nobody reads twice. If a round makes
      an untested path testable, the note comes back out.
- [ ] **The doc comments above what this round touched.** A module
      header and a trait's summary are documents too, and nothing here
      reads them: `ImapBackend` in `src/imap/mod.rs` still said "nothing
      ever moves or deletes mail" several rounds after `move`, `folder
      create` and `folder rename` landed in it. `make check` cannot
      catch that — clippy lints the code, not the claim above it — so
      re-reading the comment over the code you changed is the only
      thing that does.

## Mutation testing

Not every round, but worth a pass when a round adds a lot of new logic
— it answers the question the suite cannot ask itself: *would these
tests notice if the code stopped being right?*

    cargo mutants --file src/imap/mime.rs --in-place -F '<functions>'

`--in-place` is required here (CLAUDE.md says why). Commit first: the
run mutates the working tree and restores it, so `git checkout --
mail-imap/src` is the recovery if it is interrupted -- and check `git
diff` when it finishes too, because a run that exits 0 can still leave
one mutant behind (one here left `required_capability` returning
`Some("")`). Nothing else may
touch the tree while it runs, readers included — the files on disk are
deliberately broken for the duration.

**`src/imap/real.rs` needs the wire runner, or the run means nothing.**
`cargo mutants` runs `cargo test`, which skips the `#[ignore]`d wire
tests — and `real.rs` is the file the offline suite never reaches. So
every mutant in it "survives" by construction, and the report looks
like a catastrophe while saying nothing at all. Start the server and
point the runner at the wire suite:

    make tests-server-start
    MAIL_IMAP_WIRE_CONFIG=tests-tmp/greenmail.conf \
      cargo mutants --file src/imap/real.rs --in-place -F '<functions>' \
        --test-tool=cargo --cargo-test-arg=--test --cargo-test-arg=wire \
        --cargo-test-arg=-- --cargo-test-arg=--ignored \
        --cargo-test-arg=--test-threads=1
    make tests-server-stop

It is slow — the baseline alone is the whole wire suite, so budget
about a minute a mutant — and it is the only way to ask this file the
question. Measured that way, both of `fetch_to_result`'s filters die.
Three survivors are standing, and none of them is a test gap:

- ~~**`fetch_to_result_from_headers`, both filters.**~~ Closed:
  `tests/replay.rs` scripts a socket that answers badly on purpose and
  walks the client down to that rung, so the function is now covered
  offline and both mutants die under the plain `cargo mutants` runner.
- **`unstated` in `permanent_flags`.** Dropping the field leaves
  `false`, which is what `permanent_flags.is_empty()` returns on any
  server that states its PERMANENTFLAGS — and GreenMail always does.
  Equivalent here; distinguishable only on a server that stays silent.

A **TIMEOUT** in the report is a third thing, and it is fine: the
mutant made the code loop forever (`i += 1` into `i *= 1` in
`matches_at`, say, or its `&&` into `||`) and the run was killed rather
than passing. A test suite that hangs has noticed. Leave them.

**Do not chase 100%.** A surviving mutant is one of two things, and
they want opposite answers:

- a **test gap** — the code has behaviour nothing asserts. Write the
  assertion. The shape to look for is a test asserting *presence*
  (`len()`, `contains`) where the thing that matters is a *value*: a
  mutant turning `i + 1` into `i` survived `attachments.len() == 1`
  and would have renumbered every part.
- an **equivalent mutant** — no input distinguishes it, so no test
  can. `decode_charset_bytes`'s `"utf-8"` arm does exactly what its
  fallback does; `rewrite_part`'s depth guard cannot fire because
  `parse_message` rejects the same bytes first. Both are documented in
  the code as unkillable, which is the deliverable — the next person
  should not spend an afternoon rediscovering it.

## Dependencies

- [ ] **Has upstream taken the THREAD support yet?** `imap` and
      `imap-proto` are local forks, and it is worth being exact about
      why, because the answer is narrower than "RFC 5256":

      | | upstream | the fork adds |
      | --- | --- | --- |
      | `SORT` (RFC 5256) | already there (`uid_sort`) | nothing |
      | `THREAD` (RFC 5256) | missing | `uid_thread` + `extensions/thread.rs`, and the parser for untagged `THREAD` responses |

      Two commits carry it, one per repository:

      - `sdalu/rust-imap` — *Add support for the THREAD extension*
      - the `imap-proto` fork — *Add parser support for the THREAD
        extension*, tracked upstream as djc/tokio-imap#212

      Note that the `imap-proto` fork calls itself **0.16.8**, a local
      bump: crates.io has 0.16.7 and no 0.16.8 exists. A `cargo search`
      showing 0.16.8 is the signal that the real thing shipped.

      So, each round:

      ```sh
      cargo search imap-proto --limit 1     # > 0.16.7 with the THREAD parser?
      cargo search imap --limit 1           # a release carrying uid_thread?
      ```

      If both have it: point `Cargo.toml` at versions instead of
      `path = `, drop the submodules, and delete the fork trap from
      CLAUDE.md. `make tests-wire` is what says the swap worked —
      `a_reply_chain_is_threaded_from_the_headers` exercises the
      feature the fork exists for. (It passes either way: the threading
      falls back to client-side reconstruction when the server does not
      advertise `THREAD=REFERENCES`, and the throwaway server does not.
      So also run it against an account whose server does, or check
      `info` reports `threading  server`.)

      The debt is worth re-reading each round rather than settling in:
      the forks are rebased by hand, they are why a checkout without
      `../forks` does not build, and their own bugs are ours to carry —
      the CRLF hole that let a folder name inject an IMAP command was
      in the fork's `quote!`, not in this tree.

## Release

- [ ] Does the number move? A change that alters no behaviour usually
      does not, and a round in the middle of a longer piece of work
      should wait for the end of it rather than move twice — a number
      that moves twice for one body of work tells a reader less than
      one that moves once. It lives in one file: `Cargo.toml`. There is no `make
      tag`: the tags belong to the AiTools repository this tree sits in.
- [ ] **When it moves, the README's sample output moves with it, and
      nothing checks that.** `make check-version` greps `src` only —
      which is right, since the point is that the code reads
      `CARGO_PKG_VERSION` — so it never sees the two places the README
      *prints* a version: the `info` text output and the `-j` object
      beside it. Both said 0.2.0 while `Cargo.toml` said 0.3.0, and
      every gate passed. Grep the documents for the old number before
      believing the bump is done.

Then capture: whatever this round learned goes to the artifact that owns
that kind of fact, and anything this list failed to ask becomes a line
here.
