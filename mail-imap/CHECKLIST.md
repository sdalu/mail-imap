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
- [ ] `example.conf` — does it still parse, and does it still show a
      profile block? It is the only place the multi-account shape is
      written out for someone to copy.
- [ ] `example.conf` — a config field that changed is written in four
      places: the struct, the README table, the man page, and this
      template. `scripts/check-examples.sh` does not read configs, so nothing
      catches a stale one but this line.

## Release

- [ ] Does the number move? A change that alters no behaviour usually
      does not. It lives in one file: `Cargo.toml`. There is no `make
      tag`: the tags belong to the AiTools repository this tree sits in.

Then capture: whatever this round learned goes to the artifact that owns
that kind of fact, and anything this list failed to ask becomes a line
here.
