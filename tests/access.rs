//! Every access level against every operation it gates.
//!
//! `access-level` is this tool's central promise: what a run may change
//! on the server is capped by one config field, enforced in
//! `ImapClient` rather than in the handlers, so a new command cannot
//! quietly escape it. The promise is only as good as its weakest gate,
//! and mutation testing found two that nothing tested at all --
//! `may_strip_part` could be forced to `true`, letting `readonly`
//! rewrite a message, and the suite passed.
//!
//! So the ladder is checked as a *matrix* rather than one case at a
//! time: every level against every gated operation, in both
//! directions. Refused where it must be refused is half of it; the
//! other half is permitted where it must be permitted, because a gate
//! stuck shut is also a defect and a refusal-only test cannot see one.
//!
//! Adding an operation to `ImapBackend` means adding a row here, and
//! the row is the statement of which rung it belongs on -- CHECKLIST
//! asks for exactly that, and this is where the answer is executable.
//!
//! Offline: the mock backend, no server, no network.

use anyhow::Result;
use mail_imap::config::{AccessLevel, Config};
use mail_imap::imap::{ImapBackend, ImapClient};

/// The ladder, least to most permissive.
const LEVELS: [AccessLevel; 4] = [
    AccessLevel::ReadOnly,
    AccessLevel::Organize,
    AccessLevel::Restructure,
    AccessLevel::Full,
];

/// A client at one level, against the mock. A fresh one per case: the
/// permitted operations really do change the mock, and a shared client
/// would let one row's success set up or spoil the next row's.
fn client(level: AccessLevel) -> ImapClient {
    ImapClient::connect(
        &Config { mock: true, access: level, ..Config::default() },
        false,
    )
    .expect("the mock backend needs no server")
}

/// One gated operation: what it is called in the refusal, the lowest
/// level allowed to run it, and how to run it.
struct Op {
    what: &'static str,
    needs: AccessLevel,
    run: fn(&mut ImapClient) -> Result<()>,
    /// What has to be true before `run` can succeed, for the rows where
    /// the operation is not self-contained. Only the permitted
    /// direction runs it: the refusals must reach their own gate
    /// rather than tripping over the setup's, which would test the
    /// wrong gate and look identical from outside.
    prepare: fn(&mut ImapClient) -> Result<()>,
}

/// Most operations need nothing set up first.
fn nothing(_: &mut ImapClient) -> Result<()> {
    Ok(())
}

/// Every operation `ImapClient` gates, and the rung it sits on.
///
/// The rungs are not arbitrary and the comments in `config.rs` give
/// the reasoning: `organize` keeps every message and may file them,
/// `restructure` may shape the tree but still loses nothing, and
/// `full` is for the operations that destroy something -- a mailbox, a
/// message, or a part inside one.
fn operations() -> Vec<Op> {
    vec![
        Op {
            what: "set a flag",
            needs: AccessLevel::Organize,
            run: |c| c.store_flags("INBOX", &[1], &["\\Flagged".to_string()], &[]),
            prepare: nothing,
        },
        Op {
            what: "clear a flag",
            needs: AccessLevel::Organize,
            run: |c| c.store_flags("INBOX", &[1], &[], &["\\Flagged".to_string()]),
            prepare: nothing,
        },
        Op {
            what: "move mail",
            needs: AccessLevel::Organize,
            run: |c| c.move_messages("INBOX", &[1], "Trash"),
            prepare: nothing,
        },
        Op {
            what: "copy mail",
            needs: AccessLevel::Organize,
            run: |c| c.copy_messages("INBOX", &[1], "Trash"),
            prepare: nothing,
        },
        Op {
            what: "create a folder",
            needs: AccessLevel::Restructure,
            run: |c| c.create_folder("Archive", None),
            prepare: nothing,
        },
        Op {
            what: "rename a folder",
            needs: AccessLevel::Restructure,
            run: |c| c.rename_folder("Spam", "Junk"),
            prepare: nothing,
        },
        Op {
            what: "subscribe to a folder",
            needs: AccessLevel::Restructure,
            run: |c| c.set_subscribed("Spam", true),
            prepare: nothing,
        },
        Op {
            what: "set \\Deleted",
            needs: AccessLevel::Full,
            run: |c| c.store_flags("INBOX", &[1], &["\\Deleted".to_string()], &[]),
            prepare: nothing,
        },
        Op {
            what: "delete a folder",
            needs: AccessLevel::Full,
            run: |c| c.delete_folder("Spam", true),
            prepare: nothing,
        },
        Op {
            what: "strip a part",
            needs: AccessLevel::Full,
            run: |c| c.strip_part("INBOX", 3, &[2]).map(|_| ()),
            prepare: nothing,
        },
        Op {
            what: "expunge",
            needs: AccessLevel::Full,
            run: |c| c.expunge_messages("INBOX", &[1]).map(|_| ()),
            // `expunge` removes only what is already marked, and never
            // marks anything itself -- so a run that permits it still
            // has nothing to remove until something is marked. Only
            // the permitted direction does this: below `full` the bare
            // call must be stopped by `check_expunge`, and marking
            // first would have it stopped by the flag gate instead.
            prepare: |c| c.store_flags("INBOX", &[1], &["\\Deleted".to_string()], &[]),
        },
        Op {
            what: "append a message",
            needs: AccessLevel::Full,
            run: |c| c.append_message("INBOX", b"To: a@b\r\n\r\nbody\r\n", &[], None).map(|_| ()),
            prepare: nothing,
        },
    ]
}

/// Nothing below an operation's own rung may perform it.
#[test]
fn every_level_refuses_what_it_is_not_allowed_to_do() {
    for op in operations() {
        for level in LEVELS.into_iter().filter(|l| *l < op.needs) {
            let err = (op.run)(&mut client(level))
                .expect_err(&format!("{:?} must not {}", level, op.what));
            let text = err.to_string();
            // The refusal names the level in force, so a reader knows
            // which setting produced it rather than only that
            // something was refused.
            assert!(
                text.contains(&format!("access level '{}'", level.as_str())),
                "{:?} refusing to {} did not name the level in force: {}",
                level,
                op.what,
                text
            );
        }
    }
}

/// And every level at or above it may.
///
/// The half a refusal-only suite cannot see: a gate stuck shut refuses
/// everything and passes every "is it refused?" assertion there is.
#[test]
fn every_level_permits_what_it_is_allowed_to_do() {
    for op in operations() {
        for level in LEVELS.into_iter().filter(|l| *l >= op.needs) {
            let mut c = client(level);
            (op.prepare)(&mut c)
                .unwrap_or_else(|e| panic!("{:?} could not set up {}: {:#}", level, op.what, e));
            (op.run)(&mut c)
                .unwrap_or_else(|e| panic!("{:?} must be able to {}: {:#}", level, op.what, e));
        }
    }
}

/// Reading is never gated, including at `readonly` -- which is the
/// level's whole point, and the reason reads use `BODY.PEEK` so that
/// even fetching a message sets no `\Seen`.
#[test]
fn readonly_can_still_read_everything() {
    let mut c = client(AccessLevel::ReadOnly);
    c.list_folders().expect("listing mailboxes changes nothing");
    c.folder_uids("INBOX").expect("listing UIDs changes nothing");
    c.search_folders(&["INBOX".to_string()], "ALL", 0, None)
        .expect("searching changes nothing");
    c.read_message("INBOX", 1, false).expect("reading changes nothing");
    c.message_flags("INBOX", 1).expect("reading flags changes nothing");
}

/// The first gate that stops you is the one that speaks.
///
/// Setting `\Deleted` needs `full`, but at `readonly` it is refused
/// before that ever comes up -- by the gate that allows no changes at
/// all -- and the message says `organize`, the next rung, not `full`.
/// That is right: it names the smallest step that would get the reader
/// moving, and a matrix test asserting "the refusal names the level
/// the operation needs" would have quietly demanded the opposite.
#[test]
fn a_refusal_names_the_rung_that_would_lift_it_not_the_final_one() {
    let deleted = ["\\Deleted".to_string()];

    let at_readonly = client(AccessLevel::ReadOnly)
        .store_flags("INBOX", &[1], &deleted, &[])
        .expect_err("readonly changes nothing")
        .to_string();
    assert!(
        at_readonly.contains("'organize'"),
        "readonly should point at the next rung, not the last: {}",
        at_readonly
    );

    let at_organize = client(AccessLevel::Organize)
        .store_flags("INBOX", &[1], &deleted, &[])
        .expect_err("\\Deleted needs full")
        .to_string();
    assert!(
        at_organize.contains("'full'"),
        "organize is past the first gate, so the next answer is the real one: {}",
        at_organize
    );
    // Clearing it, though, is allowed wherever changing flags is:
    // taking a flag off a message never loses the message.
    client(AccessLevel::Organize)
        .store_flags("INBOX", &[1], &[], &deleted)
        .expect("clearing \\Deleted is not a destructive act");
}

/// The ordering the whole ladder rests on.
///
/// Every gate is a `>=` against one of these, and `--access-level`
/// narrowing is a `<=` in `main.rs`, so the order of the four levels
/// is load-bearing in a way no single gate makes visible. The CLI half
/// of that -- that a wider `--access-level` is refused rather than
/// quietly obeyed -- is driven through the binary in `tests/cli.rs`,
/// because it is a property of the command line and not of the client.
#[test]
fn the_ladder_is_ordered_least_to_most_permissive() {
    for pair in LEVELS.windows(2) {
        assert!(pair[0] < pair[1], "{:?} must be narrower than {:?}", pair[0], pair[1]);
    }
    assert_eq!(LEVELS.iter().copied().min(), Some(AccessLevel::ReadOnly));
    assert_eq!(LEVELS.iter().copied().max(), Some(AccessLevel::Full));
    assert_eq!(AccessLevel::default(), AccessLevel::Organize, "the default is neither extreme");
}
