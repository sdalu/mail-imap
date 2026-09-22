# Closing a round on mail-imap

What has to be true before a unit of work here is done.

## Gates

Run in order. A red gate is answered before anything else, and the
answer is sometimes that the checker is wrong rather than the file.

- [ ] `make check` — clippy with warnings denied, and the release number
      still written only in `Cargo.toml`
- [ ] `make tests` — the suite, against the mock backend
- [ ] `target/debug/mail-imap --mock <the commands this round touched>` —
      the suite drives the backends, not the CLI surface: a broken
      argument shape passes `make tests` and fails here

## The real server

The suite never talks to one, so a round that changed `src/imap/real.rs`
or anything it sends has not been tested by anything above.

- [ ] Did this round change what goes on the wire? If so, run it against
      a real account (`-c incal.conf`, `-d` to see the exchange) and say
      in the commit which commands were run.
- [ ] Did it change a mutating path (`flag`, `tag` — the only two)? Then
      it was run against a real account on a message that can be spared.

## Documents

Re-read each against what this round changed: a document that was true
this morning is a claim, not a fact.

- [ ] `README.md` — does it name only flags, paths and commands that exist
      today, and does every example still run?
- [ ] `DESIGN.md` — did this round change what it says about the
      IMAP commands sent, the fallbacks, or the resolution of selections?
- [ ] `CLAUDE.md` — does it still point at files that exist, and is its
      gate command still the one that runs?

## Release

- [ ] Does the number move? A change that alters no behaviour usually
      does not. It lives in one file: `Cargo.toml`. There is no `make
      tag`: the tags belong to the AiTools repository this tree sits in.

Then capture: whatever this round learned goes to the artifact that owns
that kind of fact, and anything this list failed to ask becomes a line
here.
