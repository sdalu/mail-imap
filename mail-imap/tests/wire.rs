//! What `src/imap/real.rs` actually puts on, and gets back from, a
//! socket.
//!
//! `make tests` proves the mock and never opens one, so nothing in it
//! can fail because the real backend sends the wrong thing (CLAUDE.md
//! says so, and it is why these exist). These run against a throwaway
//! GreenMail:
//!
//! ```text
//! make tests-wire          # starts the server, runs these, stops it
//! ```
//!
//! Every test here is `#[ignore]`d, so the ordinary suite stays
//! server-free and the skipped ones are *visible* in its output rather
//! than silently absent. Run under `--ignored` with no server, they
//! fail loudly with what to do about it -- a wire test that passes
//! because it quietly found nothing to talk to is worse than no test.
//!
//! Isolation: every test stamps its own messages with a unique subject
//! token and finds them again by searching for it, so tests do not see
//! each other's mail and need no fixed UIDs. Folders are stamped the
//! same way. The server is disposable, so nothing is torn down.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::DateTime;
use mail_imap::config::{AccessLevel, Config};
use mail_imap::imap::{ImapBackend, ImapClient};

use imap::{ClientBuilder, ConnectionMode};

/// Config naming the server to talk to. `make tests-wire` sets it.
const CONFIG_ENV: &str = "MAIL_IMAP_WIRE_CONFIG";
const DEFAULT_CONFIG: &str = "tests-tmp/greenmail.conf";
/// GreenMail's SMTP port, the way messages get delivered.
const SMTP_ADDR: &str = "127.0.0.1:3025";
const RECIPIENT: &str = "tester@localhost";

// ---------------------------------------------------------------- setup

fn config() -> Config {
    let path = std::env::var(CONFIG_ENV).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    match mail_imap::config::load_config(Some(&path), None) {
        Ok(loaded) => loaded.config,
        Err(e) => panic!(
            "no wire config at '{}' ({:#}).\n\
             These tests need a server. Run `make tests-wire`, which starts one, \
             or `make tests-server-start` and then set {}={}.",
            path, e, CONFIG_ENV, DEFAULT_CONFIG
        ),
    }
}

fn client() -> ImapClient {
    connect_with(config())
}

fn connect_with(config: Config) -> ImapClient {
    ImapClient::connect(&config, false).unwrap_or_else(|e| {
        panic!(
            "could not reach the IMAP server named by the wire config ({:#}).\n\
             Run `make tests-wire`, which starts one and stops it afterwards.",
            e
        )
    })
}

/// A token no other test will match on, for subjects and folder names.
fn unique(what: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "{}-{}-{}-{}",
        what,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

// -------------------------------------------------------------- delivery

/// Minimal SMTP, so the suite can put real mail in the mailbox without
/// taking a dependency for it. Enough of RFC 5321 for a local handoff
/// and nothing more.
fn deliver_raw(message: &str) {
    let stream = TcpStream::connect(SMTP_ADDR)
        .unwrap_or_else(|e| panic!("no SMTP at {}: {} (is the server running?)", SMTP_ADDR, e));
    stream.set_nodelay(true).ok();
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;

    let expect = |reader: &mut BufReader<TcpStream>, lead: u8, what: &str| {
        // A reply may be multi-line: "250-..." continues, "250 ..." ends.
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).expect("SMTP read");
            assert!(n > 0, "SMTP closed while waiting for {}", what);
            let code = line.as_bytes();
            assert!(
                code.first() == Some(&lead),
                "SMTP said {:?} to {}",
                line.trim_end(),
                what
            );
            if code.get(3) != Some(&b'-') {
                return;
            }
        }
    };

    expect(&mut reader, b'2', "the greeting");
    for (cmd, lead) in [
        ("HELO mail-imap-tests\r\n".to_string(), b'2'),
        ("MAIL FROM:<alice@example.com>\r\n".to_string(), b'2'),
        (format!("RCPT TO:<{}>\r\n", RECIPIENT), b'2'),
        ("DATA\r\n".to_string(), b'3'),
    ] {
        writer.write_all(cmd.as_bytes()).expect("SMTP write");
        expect(&mut reader, lead, cmd.trim_end());
    }
    // Dot-stuffing: a line that is just "." would end the message early.
    let body: String = message
        .replace("\r\n", "\n")
        .lines()
        .map(|l| if l == "." { "..".to_string() } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\r\n");
    writer
        .write_all(format!("{}\r\n.\r\n", body).as_bytes())
        .expect("SMTP write body");
    expect(&mut reader, b'2', "the message");
    writer.write_all(b"QUIT\r\n").ok();
}

/// A plain text/plain message carrying `token` in its subject.
fn deliver(token: &str, body: &str) {
    deliver_raw(&format!(
        "From: alice@example.com\r\n\
         To: {}\r\n\
         Subject: {}\r\n\
         Content-Type: text/plain\r\n\
         \r\n\
         {}\r\n",
        RECIPIENT, token, body
    ));
}

/// Puts `message` into INBOX with IMAP `APPEND` instead of SMTP: an
/// IMAP literal carries exactly the byte count handed to it, so raw
/// 8-bit content (a non-ASCII `Subject:`, say) reaches the server
/// intact -- no 7-bit transport, no charset to declare, nothing for a
/// line reader to mangle. `deliver`/`deliver_raw` stay untouched for
/// every other test; this is only for the one test that needs bytes
/// SMTP cannot be trusted to carry.
fn append_raw(message: &str) {
    let cfg = config();
    let client = ClientBuilder::new(cfg.server.as_str(), cfg.port)
        .mode(ConnectionMode::Plaintext)
        .connect()
        .unwrap_or_else(|e| panic!("could not connect for APPEND ({:#}).", e));
    let mut session = client
        .login(cfg.username.as_str(), cfg.password.as_str())
        .map_err(|(e, _)| e)
        .unwrap_or_else(|e| panic!("login as '{}' failed for APPEND ({:#}).", cfg.username, e));
    session
        .append("INBOX", message.as_bytes())
        .finish()
        .unwrap_or_else(|e| panic!("APPEND failed ({:#}).", e));
}

/// The exact bytes `BODY[]` returns for one UID in INBOX, over a raw
/// connection of its own -- not through `ImapClient::get_email`, which
/// rebuilds its own summary text and would hide the very thing a
/// line-ending check needs to see.
fn fetch_raw_body(uid: u32) -> Vec<u8> {
    let cfg = config();
    let client = ClientBuilder::new(cfg.server.as_str(), cfg.port)
        .mode(ConnectionMode::Plaintext)
        .connect()
        .unwrap_or_else(|e| panic!("could not connect for raw FETCH ({:#}).", e));
    let mut session = client
        .login(cfg.username.as_str(), cfg.password.as_str())
        .map_err(|(e, _)| e)
        .unwrap_or_else(|e| panic!("login as '{}' failed for raw FETCH ({:#}).", cfg.username, e));
    session.select("INBOX").expect("SELECT INBOX for raw FETCH");
    let fetches = session
        .uid_fetch(uid.to_string(), "BODY[]")
        .unwrap_or_else(|e| panic!("UID FETCH {} BODY[] failed ({:#}).", uid, e));
    let f = fetches
        .iter()
        .next()
        .unwrap_or_else(|| panic!("no FETCH response for UID {}", uid));
    f.body()
        .unwrap_or_else(|| panic!("no BODY[] in the FETCH response for UID {}", uid))
        .to_vec()
}

/// The UIDs of the messages carrying `token`, ascending.
fn uids_for(client: &mut ImapClient, token: &str) -> Vec<u32> {
    let hits = client
        .search_folders(
            &["INBOX".to_string()],
            &format!("HEADER SUBJECT \"{}\"", token),
            0,
            None,
        )
        .expect("UID SEARCH");
    let mut uids: Vec<u32> = hits.iter().map(|h| h.uid).collect();
    uids.sort_unstable();
    uids
}

/// Deliver `count` messages stamped with a fresh token and hand back
/// the token and their UIDs.
fn fixture(client: &mut ImapClient, what: &str, count: usize) -> (String, Vec<u32>) {
    let token = unique(what);
    for i in 0..count {
        deliver(&token, &format!("body number {}", i));
    }
    let uids = uids_for(client, &token);
    assert_eq!(
        uids.len(),
        count,
        "delivered {} message(s) stamped {} but found {}",
        count,
        token,
        uids.len()
    );
    (token, uids)
}

// ----------------------------------------------------------- the tests

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn it_connects_logs_in_and_reports_what_the_server_advertises() {
    let mut c = client();
    let caps = c.capabilities().expect("CAPABILITY");
    assert!(
        caps.iter().any(|x| x == "IMAP4REV1"),
        "a server that does not say IMAP4rev1 is not one we can trust: {:?}",
        caps
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn it_lists_folders_with_their_hierarchy_delimiter() {
    let mut c = client();
    let folders = c.list_folders().expect("LIST");
    let inbox = folders
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case("INBOX"))
        .expect("every account has an INBOX");
    assert!(
        inbox.delimiter.as_deref().is_some_and(|d| !d.is_empty()),
        "LIST must report a delimiter to build paths with: {:?}",
        inbox
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn a_search_returns_the_metadata_it_was_asked_for() {
    let mut c = client();
    let (token, uids) = fixture(&mut c, "search", 3);
    let hits = c
        .search_folders(
            &["INBOX".to_string()],
            &format!("HEADER SUBJECT \"{}\"", token),
            0,
            None,
        )
        .expect("search");
    assert_eq!(hits.len(), 3);
    for h in &hits {
        assert_eq!(h.folder, "INBOX");
        assert_eq!(h.subject, token, "ENVELOPE subject came back wrong");
        assert!(h.from.contains("alice@example.com"), "from: {}", h.from);
        assert!(h.size.unwrap_or(0) > 0, "RFC822.SIZE missing");
        assert!(h.date.is_some(), "INTERNALDATE missing");
    }
    // Default order is most-recent first, which for UIDs is descending.
    let got: Vec<u32> = hits.iter().map(|h| h.uid).collect();
    let mut newest_first = uids.clone();
    newest_first.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(got, newest_first, "default order is newest first");
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn a_non_ascii_search_is_accepted_as_charset_utf8_though_greenmail_does_not_match_it() {
    let mut c = client();
    // The accented "é" is what routes this search through
    // `CHARSET UTF-8` (src/imap/real.rs, RealClient::uid_search_charset);
    // a plain-ASCII subject never exercises that path. Delivered via
    // `append_raw`, not `deliver`: an IMAP literal carries the exact
    // byte count handed to it, so this is unaffected by anything SMTP
    // (7-bit headers -- RFC 5321/5322; 8BITMIME covers the body, not
    // the Subject) might do to an 8-bit header.
    let token = unique("charset");
    let subject = format!("R\u{e9}union {}", token);
    append_raw(&format!(
        "From: alice@example.com\r\n\
         To: {}\r\n\
         Subject: {}\r\n\
         Content-Type: text/plain\r\n\
         \r\n\
         body\r\n",
        RECIPIENT, subject
    ));

    // Prove the message is really on the server first, matched by the
    // plain-ASCII half of its own subject -- this does not exercise
    // CHARSET at all, so it isolates "did the message arrive intact"
    // from "does this server's SEARCH match a non-ASCII byte".
    let ascii_hits = c
        .search_folders(
            &["INBOX".to_string()],
            &format!("HEADER SUBJECT \"{}\"", token),
            0,
            None,
        )
        .expect("ASCII search");
    assert_eq!(
        ascii_hits.len(),
        1,
        "the message never reached the server intact: {:?}",
        ascii_hits
    );

    // The interesting case: search on the accented subject itself.
    // GreenMail (v2.1.9, as vendored by scripts/greenmail-server.sh)
    // accepts `CHARSET UTF-8` without complaint -- no BAD/NO, so
    // `uid_search_charset`'s fallback never fires here -- but it does
    // not actually match a non-ASCII byte in a header: `hits` comes
    // back empty even though the message above is proven to carry
    // exactly this subject. That is a limit of this particular server,
    // not something this test can claim about real ones, so only the
    // wire form is proven here (declared, accepted, no fallback); the
    // matching half is left unproven on GreenMail.
    let hits = c
        .search_folders(
            &["INBOX".to_string()],
            &format!("HEADER SUBJECT \"{}\"", subject),
            0,
            None,
        )
        .expect("UID SEARCH CHARSET UTF-8 must be accepted, not refused with BAD/NO");
    assert!(
        hits.is_empty() || hits.iter().any(|h| h.subject == subject),
        "a hit that does not carry the searched-for subject would be a real bug: {:?}",
        hits
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn reading_a_message_does_not_mark_it_seen() {
    // The tool's headline promise, and the one the mock can never keep
    // honest: reads go through BODY.PEEK[], so fetching a message
    // leaves \Seen exactly as it was.
    let mut c = client();
    let (_token, uids) = fixture(&mut c, "peek", 1);
    let uid = uids[0];

    let before = c.message_flags("INBOX", uid).expect("FETCH FLAGS");
    assert!(
        !before.iter().any(|f| f.eq_ignore_ascii_case("\\Seen")),
        "a freshly delivered message should not be \\Seen: {:?}",
        before
    );

    let body = c.get_email("INBOX", uid).expect("read");
    assert!(body.contains("body number 0"), "body not returned");

    let after = c.message_flags("INBOX", uid).expect("FETCH FLAGS");
    assert!(
        !after.iter().any(|f| f.eq_ignore_ascii_case("\\Seen")),
        "reading set \\Seen -- BODY.PEEK is not being used: {:?}",
        after
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn max_results_caps_what_a_search_fetches() {
    let mut c = client();
    let (token, _) = fixture(&mut c, "cap", 3);
    let query = format!("HEADER SUBJECT \"{}\"", token);
    let all = c
        .search_folders(&["INBOX".to_string()], &query, 0, None)
        .expect("uncapped");
    assert_eq!(all.len(), 3, "0 means no cap");
    let capped = c
        .search_folders(&["INBOX".to_string()], &query, 2, None)
        .expect("capped");
    assert_eq!(capped.len(), 2);
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn sorting_goes_through_the_server_when_it_advertises_sort() {
    let mut c = client();
    let caps = c.capabilities().expect("CAPABILITY");
    assert!(
        caps.iter().any(|x| x == "SORT"),
        "this test is about the server-side path; the server did not offer it: {:?}",
        caps
    );
    let token = unique("sort");
    // Subject order, To: order and arrival order all differ, so no two
    // of them can be mistaken for each other.
    for (suffix, to) in [("zulu", "alpha"), ("mike", "zulu"), ("alpha", "mike")] {
        deliver_raw(&format!(
            "From: alice@example.com\r\nTo: {}@example.com\r\n\
             Subject: {} {}\r\nContent-Type: text/plain\r\n\r\nbody\r\n",
            to, token, suffix
        ));
    }
    let query = format!("HEADER SUBJECT \"{}\"", token);

    // By subject. Either path can do this one, so it says only that the
    // ordering is right -- not which side did it.
    let spec = mail_imap::imap::parse_sort("subject").expect("parse");
    let hits = c
        .search_folders(&["INBOX".to_string()], &query, 0, Some(&spec))
        .expect("sorted search");
    let subjects: Vec<&str> = hits.iter().map(|h| h.subject.as_str()).collect();
    assert_eq!(subjects.len(), 3);
    let mut ascending = subjects.clone();
    ascending.sort_by_key(|s| s.to_lowercase());
    assert_eq!(subjects, ascending, "not ordered by subject");

    // By To:, which only the server can do. `SearchResult` does not
    // carry the To header, so `sort_results` leaves such a spec in
    // arrival order and `search_in_folder` refuses it outright when the
    // server has no SORT. Getting the right order back is therefore
    // proof that `UID SORT` was the path taken.
    let by_to = mail_imap::imap::parse_sort("to").expect("parse");
    let hits = c
        .search_folders(&["INBOX".to_string()], &query, 0, Some(&by_to))
        .expect("the server advertises SORT, so sorting by 'to' must work");
    let order: Vec<&str> = hits
        .iter()
        .map(|h| h.subject.rsplit(' ').next().unwrap_or(""))
        .collect();
    assert_eq!(
        order,
        vec!["zulu", "alpha", "mike"],
        "UID SORT TO did not order by the To header (subjects, in the order returned)"
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn flags_can_be_set_and_cleared() {
    let mut c = client();
    let (_token, uids) = fixture(&mut c, "flags", 1);
    let uid = uids[0];
    let flagged = vec!["\\Flagged".to_string()];

    c.store_flags("INBOX", &[uid], &flagged, &[]).expect("+FLAGS");
    let on = c.message_flags("INBOX", uid).expect("flags");
    assert!(
        on.iter().any(|f| f.eq_ignore_ascii_case("\\Flagged")),
        "UID STORE +FLAGS did not stick: {:?}",
        on
    );

    c.store_flags("INBOX", &[uid], &[], &flagged).expect("-FLAGS");
    let off = c.message_flags("INBOX", uid).expect("flags");
    assert!(
        !off.iter().any(|f| f.eq_ignore_ascii_case("\\Flagged")),
        "UID STORE -FLAGS did not clear it: {:?}",
        off
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn a_keyword_survives_a_round_trip() {
    let mut c = client();
    let (_token, uids) = fixture(&mut c, "keyword", 1);
    let uid = uids[0];
    let permanent = c.permanent_flags("INBOX").expect("SELECT/PERMANENTFLAGS");
    assert!(
        permanent.keeps("invoice"),
        "this mailbox will not keep a new keyword, so the test cannot run: {:?}",
        permanent
    );
    let kw = vec!["invoice".to_string()];
    c.store_flags("INBOX", &[uid], &kw, &[]).expect("+FLAGS");
    let flags = c.message_flags("INBOX", uid).expect("flags");
    assert!(
        flags.iter().any(|f| f == "invoice"),
        "keyword did not come back: {:?}",
        flags
    );
    c.store_flags("INBOX", &[uid], &[], &kw).expect("-FLAGS");
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn uid_store_ignores_a_missing_uid_which_is_why_the_cli_checks_first() {
    // RFC 3501 6.4.8: a non-existent UID is ignored without an error.
    // This pins the server behaviour that `cli::check_uids_exist`
    // exists to compensate for -- if a server ever started erroring
    // here, that guard would be the thing to revisit.
    let mut c = client();
    let missing = 4_000_000_001u32;
    let result = c.store_flags("INBOX", &[missing], &["\\Flagged".to_string()], &[]);
    assert!(
        result.is_ok(),
        "the server errored on a missing UID; the CLI guard's premise has changed: {:?}",
        result.err()
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn a_message_can_be_filed_into_another_folder() {
    let mut c = client();
    let folder = unique("moved");
    c.create_folder(&folder, None).expect("CREATE");
    let (token, uids) = fixture(&mut c, "move", 1);
    let uid = uids[0];

    c.move_messages("INBOX", &[uid], &folder).expect("move");

    assert!(
        !c.folder_uids("INBOX").expect("uids").contains(&uid),
        "the source still lists the message"
    );
    let landed = c
        .search_folders(
            std::slice::from_ref(&folder),
            &format!("HEADER SUBJECT \"{}\"", token),
            0,
            None,
        )
        .expect("search the target");
    assert_eq!(landed.len(), 1, "the message is not in '{}'", folder);
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn a_message_can_be_copied_without_removing_the_original() {
    let mut c = client();
    let folder = unique("copied");
    c.create_folder(&folder, None).expect("CREATE");
    let (token, uids) = fixture(&mut c, "copy", 1);
    let uid = uids[0];

    c.copy_messages("INBOX", &[uid], &folder).expect("copy");

    assert!(
        c.folder_uids("INBOX").expect("uids").contains(&uid),
        "the source must still list the original -- copy leaves it in place"
    );
    let landed = c
        .search_folders(
            std::slice::from_ref(&folder),
            &format!("HEADER SUBJECT \"{}\"", token),
            0,
            None,
        )
        .expect("search the target");
    assert_eq!(landed.len(), 1, "the message is not in '{}'", folder);
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn expunge_removes_only_the_messages_already_marked_deleted() {
    // GreenMail advertises UIDPLUS (confirmed by hand: `info` on a
    // GreenMail config lists it among the capabilities), so this
    // exercises the real UID EXPUNGE path, not only the refusal --
    // unlike the CHARSET UTF-8 search, there is no gap to record here.
    // What is *not* covered by any wire test is a server that does not
    // advertise UIDPLUS: GreenMail always does, so the "the server
    // does not advertise UIDPLUS" refusal in `src/imap/real.rs` has no
    // wire coverage and is exercised only by inspection.
    let mut c = client();
    let (_token, uids) = fixture(&mut c, "expunge", 2);
    let (keep, remove) = (uids[0], uids[1]);

    // Neither is marked yet: refused, and the refusal says why.
    let err = c
        .expunge_messages("INBOX", &[keep, remove])
        .expect_err("nothing here is marked \\Deleted yet");
    assert!(
        err.to_string().to_lowercase().contains("deleted"),
        "wrong reason: {}",
        err
    );
    assert!(
        c.folder_uids("INBOX").expect("uids").contains(&keep)
            && c.folder_uids("INBOX").expect("uids").contains(&remove),
        "a refused expunge must not have removed anything"
    );

    // Mark only `remove`.
    c.store_flags("INBOX", &[remove], &["\\Deleted".to_string()], &[])
        .expect("mark \\Deleted");

    let removed = c
        .expunge_messages("INBOX", &[keep, remove])
        .expect("expunge the one eligible UID");
    assert_eq!(
        removed,
        vec![remove],
        "only the UID that was actually marked should be reported as removed"
    );

    let remaining = c.folder_uids("INBOX").expect("uids");
    assert!(remaining.contains(&keep), "the untouched message must survive");
    assert!(!remaining.contains(&remove), "the marked message must be gone");
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn an_appended_message_can_be_found_by_search_and_read_back() {
    let mut c = client();
    let token = unique("append");
    let msg = format!(
        "From: alice@example.com\r\nTo: {}\r\nSubject: {}\r\nContent-Type: text/plain\r\n\r\n\
         appended body\r\n",
        RECIPIENT, token
    );
    // Wire-form: this calls `ImapClient::append_message` directly, not
    // through the CLI, so it bypasses `cli::parse_flag_names` (which is
    // what turns the bare word `flag add` takes into this).
    let flagged = vec!["\\Flagged".to_string()];

    let uid = c
        .append_message("INBOX", msg.as_bytes(), &flagged, None)
        .expect("append")
        .expect("GreenMail advertises UIDPLUS, so APPENDUID must come back");

    let hits = c
        .search_folders(
            &["INBOX".to_string()],
            &format!("HEADER SUBJECT \"{}\"", token),
            0,
            None,
        )
        .expect("search");
    assert_eq!(hits.len(), 1, "the appended message was not found by search");
    assert_eq!(hits[0].uid, uid, "the reported UID does not match what search found");

    let body = c.get_email("INBOX", uid).expect("read");
    assert!(body.contains("appended body"), "body not returned: {}", body);

    let flags = c.message_flags("INBOX", uid).expect("flags");
    assert!(
        flags.iter().any(|f| f.eq_ignore_ascii_case("\\Flagged")),
        "--flag did not stick at creation: {:?}",
        flags
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn a_lone_lf_is_normalized_to_crlf_before_appending() {
    // An IMAP literal is exact bytes: a message file saved on this host
    // commonly has bare `\n` line endings, and appending it verbatim
    // would put an unterminated line on the wire -- wrong on the
    // server while every local check still shows the message as fine.
    // `ImapClient::append_message` normalizes a lone LF to CRLF before
    // sending; this reads the message back over a raw socket (see
    // `fetch_raw_body`'s own doc comment for why not `get_email`) and
    // checks every line ending it actually got is CRLF, not LF alone.
    let mut c = client();
    let token = unique("crlf");
    let lf_only = format!(
        "From: alice@example.com\nTo: {}\nSubject: {}\nContent-Type: text/plain\n\n\
         line one\nline two\n",
        RECIPIENT, token
    );
    assert!(!lf_only.contains('\r'), "fixture must be LF-only to test anything");

    let uid = c
        .append_message("INBOX", lf_only.as_bytes(), &[], None)
        .expect("append")
        .expect("GreenMail advertises UIDPLUS, so APPENDUID must come back");

    let raw = fetch_raw_body(uid);
    let mut prev = 0u8;
    for &b in raw.iter() {
        if b == b'\n' {
            assert_eq!(
                prev,
                b'\r',
                "a bare LF reached the server: {:?}",
                String::from_utf8_lossy(&raw)
            );
        }
        prev = b;
    }
    assert!(
        raw.windows(2).any(|w| w == b"\r\n"),
        "sanity: no CRLF found at all in {:?}",
        String::from_utf8_lossy(&raw)
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn appending_to_a_missing_folder_names_folder_create() {
    let mut c = client();
    let absent = unique("append-missing");
    let msg = "From: a@b\r\nTo: c@d\r\nSubject: x\r\nContent-Type: text/plain\r\n\r\nbody\r\n";

    let err = c
        .append_message(&absent, msg.as_bytes(), &[], None)
        .expect_err("the folder does not exist");
    assert!(
        err.to_string().contains("folder create"),
        "the server's raw TRYCREATE text leaked through untranslated: {}",
        err
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn appending_with_an_explicit_date_sets_internaldate() {
    // Proves the flag reaches the server, not only that it parses:
    // deliberately far from "now", so a server stamping the current
    // time instead (silently ignoring the override) would be
    // unmistakable in the result rather than accidentally close enough
    // to pass anyway.
    let mut c = client();
    let token = unique("date");
    let when = DateTime::parse_from_rfc3339("2015-03-14T09:26:53+01:00").expect("fixture date");
    let msg = format!(
        "From: alice@example.com\r\nTo: {}\r\nSubject: {}\r\nContent-Type: text/plain\r\n\r\n\
         body\r\n",
        RECIPIENT, token
    );

    let uid = c
        .append_message("INBOX", msg.as_bytes(), &[], Some(when))
        .expect("append")
        .expect("GreenMail advertises UIDPLUS, so APPENDUID must come back");

    // `search_folders` reports INTERNALDATE as `SearchResult.date`
    // (formatted "%Y-%m-%d %H:%M:%S %z" from the FETCH response, see
    // `RealClient::fetch_to_result`), so reading it back this way
    // exercises the same FETCH INTERNALDATE path a caller would see --
    // not a special read built just for this test. If GreenMail turned
    // out not to report INTERNALDATE at all, `hits[0].date` would be
    // `None` and `.expect` below would fail loudly rather than this
    // silently asserting nothing; that has not happened in practice.
    let hits = c
        .search_folders(
            &["INBOX".to_string()],
            &format!("HEADER SUBJECT \"{}\"", token),
            0,
            None,
        )
        .expect("search");
    assert_eq!(hits.len(), 1, "the appended message was not found by search");
    assert_eq!(hits[0].uid, uid);
    let reported = hits[0]
        .date
        .as_deref()
        .expect("GreenMail did not report an INTERNALDATE for this message");
    let got = DateTime::parse_from_str(reported, "%Y-%m-%d %H:%M:%S %z")
        .unwrap_or_else(|e| panic!("could not parse the reported date '{}' ({:#})", reported, e));
    // Compare the instant, not the string: a server is free to report
    // the zone it stored the timestamp in rather than echo the one it
    // was given, and that would not mean the date failed to reach it.
    assert_eq!(
        got.timestamp(),
        when.timestamp(),
        "INTERNALDATE did not reach the server: sent {}, got {} (raw: {:?})",
        when.to_rfc3339(),
        got.to_rfc3339(),
        reported
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn the_folder_tree_can_be_created_renamed_and_subscribed() {
    let mut c = client();
    let first = unique("tree");
    let second = format!("{}-renamed", first);

    c.create_folder(&first, None).expect("CREATE");
    assert!(
        c.list_folders().expect("LIST").iter().any(|f| f.name == first),
        "created folder is not listed"
    );

    c.set_subscribed(&first, true).expect("SUBSCRIBE");
    c.set_subscribed(&first, false).expect("UNSUBSCRIBE");

    c.rename_folder(&first, &second).expect("RENAME");
    let names: Vec<String> = c.list_folders().expect("LIST").into_iter().map(|f| f.name).collect();
    assert!(names.contains(&second), "renamed folder missing: {:?}", second);
    assert!(!names.contains(&first), "old name still listed");
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn folder_list_subscribed_shows_only_what_was_subscribed_to() {
    let mut c = client();
    let subscribed = unique("lsub-yes");
    let not_subscribed = unique("lsub-no");
    c.create_folder(&subscribed, None).expect("CREATE");
    c.create_folder(&not_subscribed, None).expect("CREATE");
    c.set_subscribed(&subscribed, true).expect("SUBSCRIBE");

    let names: Vec<String> = c
        .list_subscribed_folders()
        .expect("LSUB")
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert!(
        names.contains(&subscribed),
        "LSUB missed a mailbox that was subscribed to: {:?}",
        names
    );
    assert!(
        !names.contains(&not_subscribed),
        "LSUB listed a mailbox that was never subscribed to: {:?}",
        names
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn deleting_a_mailbox_removes_it_but_inbox_and_a_non_empty_one_are_refused() {
    let mut c = client();

    // INBOX: refused outright, --force or not -- the server's own rule
    // (`Session::delete`'s doc comment: "It is an error to attempt to
    // delete INBOX"), not something --force is meant to override.
    let err = c.delete_folder("INBOX", true).expect_err("INBOX must never be deleted");
    assert!(err.to_string().to_lowercase().contains("inbox"), "wrong reason: {}", err);

    // Empty: no --force needed.
    let empty = unique("delete-empty");
    c.create_folder(&empty, None).expect("CREATE");
    c.delete_folder(&empty, false).expect("an empty mailbox needs no --force");
    assert!(
        !c.list_folders().expect("LIST").iter().any(|f| f.name == empty),
        "deleted folder is still listed"
    );

    // Holding a message: refused without --force, and the refusal names
    // the count.
    let holding = unique("delete-holding");
    c.create_folder(&holding, None).expect("CREATE");
    let (_token, uids) = fixture(&mut c, "delete-holding", 1);
    c.move_messages("INBOX", &uids, &holding).expect("file a message into it");
    let err = c
        .delete_folder(&holding, false)
        .expect_err("a non-empty mailbox must be refused without --force");
    assert!(
        err.to_string().contains('1'),
        "the refusal should name the message count: {}",
        err
    );
    assert!(
        c.list_folders().expect("LIST").iter().any(|f| f.name == holding),
        "a refused delete must not have removed the mailbox"
    );

    // --force deletes it anyway.
    c.delete_folder(&holding, true).expect("--force deletes it anyway");
    assert!(
        !c.list_folders().expect("LIST").iter().any(|f| f.name == holding),
        "deleted folder is still listed"
    );
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn mime_parts_are_listed_and_saved_from_the_real_message() {
    let mut c = client();
    let token = unique("parts");
    deliver_raw(&format!(
        "From: alice@example.com\r\n\
         To: {}\r\n\
         Subject: {}\r\n\
         Content-Type: multipart/mixed; boundary=WIREBOUND\r\n\
         \r\n\
         --WIREBOUND\r\n\
         Content-Type: text/plain\r\n\
         \r\n\
         the body\r\n\
         --WIREBOUND\r\n\
         Content-Type: application/pdf\r\n\
         Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         SGVsbG8=\r\n\
         --WIREBOUND--\r\n",
        RECIPIENT, token
    ));
    let uids = uids_for(&mut c, &token);
    assert_eq!(uids.len(), 1);
    let uid = uids[0];

    let parts = c.list_parts("INBOX", uid).expect("list parts");
    assert_eq!(
        parts.iter().map(|p| p.content_type.as_str()).collect::<Vec<_>>(),
        vec!["text/plain", "application/pdf"],
        "the MIME tree did not survive the round trip"
    );
    assert_eq!(parts[1].filename.as_deref(), Some("report.pdf"));
    assert_eq!(parts[0].part, 1, "parts are numbered from 1");

    let dir = std::env::temp_dir().join(unique("wire-part"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let dest = dir.join("out.bin");
    let written = c.save_part("INBOX", uid, 2, &dest).expect("save part");
    // "SGVsbG8=" is base64 for "Hello": the transfer encoding is decoded.
    assert_eq!(written, 5);
    assert_eq!(std::fs::read(&dest).expect("read back"), b"Hello");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn a_reply_chain_is_threaded_from_the_headers() {
    let mut c = client();
    let token = unique("thread");
    let root = format!("<root.{}@example.com>", token);
    let reply = format!("<reply.{}@example.com>", token);
    deliver_raw(&format!(
        "From: alice@example.com\r\nTo: {}\r\nSubject: {}\r\nMessage-ID: {}\r\n\r\nroot\r\n",
        RECIPIENT, token, root
    ));
    deliver_raw(&format!(
        "From: bob@example.com\r\nTo: {}\r\nSubject: Re: {}\r\nMessage-ID: {}\r\n\
         In-Reply-To: {}\r\nReferences: {}\r\n\r\nreply\r\n",
        RECIPIENT, token, reply, root, root
    ));
    let uids = uids_for(&mut c, &token);
    assert_eq!(uids.len(), 2, "both messages should carry the token");

    let thread = c.thread_uids("INBOX", uids[0]).expect("thread");
    for uid in &uids {
        assert!(
            thread.contains(uid),
            "UID {} missing from the thread {:?}",
            uid,
            thread
        );
    }
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn a_line_break_in_a_folder_name_never_reaches_the_server() {
    // Regression: this put `C: A1 NOOP` on the wire and got back
    // `S: A1 OK NOOP completed` -- the name ended the SUBSCRIBE and the
    // rest of it ran as a command of its own.
    let mut c = client();
    let evil = "Evil\r\nA1 NOOP";
    for err in [
        c.create_folder(evil, None).err(),
        c.rename_folder(evil, "Fine").err(),
        c.set_subscribed(evil, true).err(),
        c.move_messages("INBOX", &[1], evil).err(),
        c.copy_messages("INBOX", &[1], evil).err(),
        c.delete_folder(evil, true).err(),
        c.append_message(evil, b"To: a@b\r\n\r\nbody\r\n", &[], None).err(),
    ] {
        let err = err.expect("a line break must be refused before the socket");
        assert!(
            err.to_string().contains("line break"),
            "refused for the wrong reason: {}",
            err
        );
    }
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn readonly_changes_nothing_on_a_real_server() {
    // The access level is enforced in ImapClient, above the backend, so
    // it has to hold with a real socket underneath it too.
    let mut c = client();
    let (_token, uids) = fixture(&mut c, "readonly", 1);
    let uid = uids[0];

    let mut ro = connect_with(Config {
        access: AccessLevel::ReadOnly,
        ..config()
    });
    let err = ro
        .store_flags("INBOX", &[uid], &["\\Flagged".to_string()], &[])
        .expect_err("readonly must refuse");
    assert!(err.to_string().contains("readonly"), "{}", err);

    // ... and reading still works, without marking anything seen.
    assert!(ro.get_email("INBOX", uid).is_ok());
    let flags = ro.message_flags("INBOX", uid).expect("flags");
    assert!(!flags.iter().any(|f| f.eq_ignore_ascii_case("\\Flagged")));
    assert!(!flags.iter().any(|f| f.eq_ignore_ascii_case("\\Seen")));
}

// ------------------------------------------------ mock vs. real server

/// One probe: what the backend did, reduced to the only thing the two
/// backends can be expected to agree on.
///
/// Not the message -- a mock saying "no mailbox 'x' (mock)" and a
/// server saying "SELECT failed. No such mailbox" are the same answer
/// in different words -- and not the content, since the two hold
/// different mail. What has to agree is whether the call was refused.
type Probe = (&'static str, bool);

/// A minimal well-formed message for the `append` probes: only whether
/// the call is refused is under test here, not its content.
const PROBE_MESSAGE: &[u8] = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: probe\r\n\r\nbody\r\n";

/// Where to aim the probes: a folder that exists, one that does not, a
/// UID that exists and one that does not.
struct Ground {
    folder: String,
    absent_folder: String,
    uid: u32,
    absent_uid: u32,
    /// A second real folder, to file into.
    move_target: String,
    /// A third real folder, empty, spent by the `delete_folder` probe.
    deletable_folder: String,
}

/// Run every probe against one backend. Kept as a single list so the
/// two runs cannot drift apart: there is one description of what is
/// being asked, and both backends answer it.
fn probe(c: &mut ImapClient, g: &Ground) -> Vec<Probe> {
    let flag = vec!["\\Flagged".to_string()];
    vec![
        // A UID that is not there is ignored by UID STORE, not refused.
        ("store_flags: absent uid", c.store_flags(&g.folder, &[g.absent_uid], &flag, &[]).is_ok()),
        ("store_flags: present uid", c.store_flags(&g.folder, &[g.uid], &flag, &[]).is_ok()),
        ("store_flags: clearing", c.store_flags(&g.folder, &[g.uid], &[], &flag).is_ok()),
        ("store_flags: absent folder", c.store_flags(&g.absent_folder, &[g.uid], &flag, &[]).is_ok()),
        // ... while a read of one is an error, because there is nothing
        // to read.
        ("message_flags: present uid", c.message_flags(&g.folder, g.uid).is_ok()),
        ("message_flags: absent uid", c.message_flags(&g.folder, g.absent_uid).is_ok()),
        ("get_email: present uid", c.get_email(&g.folder, g.uid).is_ok()),
        ("get_email: absent uid", c.get_email(&g.folder, g.absent_uid).is_ok()),
        ("list_parts: present uid", c.list_parts(&g.folder, g.uid).is_ok()),
        ("list_parts: absent uid", c.list_parts(&g.folder, g.absent_uid).is_ok()),
        ("thread_uids: present uid", c.thread_uids(&g.folder, g.uid).is_ok()),
        ("thread_uids: absent uid", c.thread_uids(&g.folder, g.absent_uid).is_ok()),
        // A folder has to exist before anything can be asked of it: a
        // real backend SELECTs it first.
        ("folder_uids: present folder", c.folder_uids(&g.folder).is_ok()),
        ("folder_uids: absent folder", c.folder_uids(&g.absent_folder).is_ok()),
        ("permanent_flags: present folder", c.permanent_flags(&g.folder).is_ok()),
        ("permanent_flags: absent folder", c.permanent_flags(&g.absent_folder).is_ok()),
        ("mailbox_counts: absent folder", c.mailbox_counts(Some(&g.absent_folder)).is_ok()),
        ("search: present folder", c.search_folders(std::slice::from_ref(&g.folder), "ALL", 0, None).is_ok()),
        ("search: absent folder", c.search_folders(std::slice::from_ref(&g.absent_folder), "ALL", 0, None).is_ok()),
        // The folder tree.
        ("create_folder: existing", c.create_folder(&g.folder, None).is_ok()),
        ("rename_folder: absent", c.rename_folder(&g.absent_folder, "Whatever").is_ok()),
        ("subscribe: absent folder", c.set_subscribed(&g.absent_folder, true).is_ok()),
        ("unsubscribe: absent folder", c.set_subscribed(&g.absent_folder, false).is_ok()),
        // Moving.
        ("move: absent target", c.move_messages(&g.folder, &[g.uid], &g.absent_folder).is_ok()),
        ("move: absent uid", c.move_messages(&g.folder, &[g.absent_uid], &g.move_target).is_ok()),
        // Copying. Unlike move, a UID that exists is still there
        // afterwards, so these probes do not have to avoid spending it.
        ("copy: absent target", c.copy_messages(&g.folder, &[g.uid], &g.absent_folder).is_ok()),
        ("copy: absent uid", c.copy_messages(&g.folder, &[g.absent_uid], &g.move_target).is_ok()),
        // Appending. Adds a message rather than touching any named UID,
        // so it cannot disturb what a later probe expects to find.
        ("append: existing folder", c.append_message(&g.folder, PROBE_MESSAGE, &[], None).is_ok()),
        ("append: absent folder", c.append_message(&g.absent_folder, PROBE_MESSAGE, &[], None).is_ok()),
        // Removing. `g.uid` is not yet marked \Deleted, so this refuses
        // -- both of the next two probes must still see it afterwards,
        // which is why the probe that actually marks and removes it
        // comes last of all, once nothing later needs it.
        ("expunge: not marked \\Deleted", c.expunge_messages(&g.folder, &[g.uid]).is_ok()),
        ("expunge: absent uid", c.expunge_messages(&g.folder, &[g.absent_uid]).is_ok()),
        // Listing only the subscribed mailboxes must not error just
        // because nothing (or everything) is subscribed.
        ("list_subscribed_folders: ok", c.list_subscribed_folders().is_ok()),
        // Deleting.
        ("delete_folder: absent", c.delete_folder(&g.absent_folder, false).is_ok()),
        ("delete_folder: INBOX even with force", c.delete_folder("INBOX", true).is_ok()),
        ("delete_folder: empty folder, no force needed", c.delete_folder(&g.deletable_folder, false).is_ok()),
        // A line break in a name never reaches either backend.
        ("subscribe: name with CRLF", c.set_subscribed("Evil\r\nA1 NOOP", true).is_ok()),
        // Last of all: marks `g.uid` \Deleted and removes it, so it
        // must not run before anything above that still needs the
        // message to exist.
        ("expunge: marked \\Deleted succeeds", {
            c.store_flags(&g.folder, &[g.uid], &["\\Deleted".to_string()], &[])
                .ok();
            c.expunge_messages(&g.folder, &[g.uid]).is_ok()
        }),
    ]
}

#[test]
#[ignore = "needs an IMAP server: make tests-wire"]
fn the_mock_answers_like_a_real_server() {
    // The mock is not a second implementation of IMAP -- it holds
    // different mail and advertises nothing -- so this compares the one
    // thing it must get right: which calls are refused. A fake that is
    // *stricter* than the real thing is the dangerous direction, since
    // it turns a live defect into an offline pass. That is not
    // hypothetical: `tag junk` on a missing UID reported success on the
    // wire and could not be reproduced here, because the mock refused
    // where the server shrugged.
    let mut real = client();
    let (_token, uids) = fixture(&mut real, "conformance", 1);
    let real_target = unique("conformance-target");
    real.create_folder(&real_target, None).expect("a folder to file into");
    let real_deletable = unique("conformance-deletable");
    real.create_folder(&real_deletable, None).expect("a folder to delete");
    let real_ground = Ground {
        folder: "INBOX".to_string(),
        absent_folder: unique("conformance-absent"),
        uid: uids[0],
        absent_uid: 4_000_000_001,
        move_target: real_target,
        deletable_folder: real_deletable,
    };

    let mut mock = ImapClient::connect(
        &Config {
            mock: true,
            access: AccessLevel::Full,
            ..Config::default()
        },
        false,
    )
    .expect("the mock needs no server");
    let mock_ground = Ground {
        folder: "INBOX".to_string(),
        absent_folder: "Nowhere".to_string(),
        uid: 1,
        absent_uid: 4_000_000_001,
        move_target: "Trash".to_string(),
        // Never touched elsewhere in `probe`, so deleting it cannot
        // break a later probe the way spending `move_target` would.
        deletable_folder: "Spam".to_string(),
    };

    let from_real = probe(&mut real, &real_ground);
    let from_mock = probe(&mut mock, &mock_ground);

    let mut differ: Vec<String> = Vec::new();
    for ((name, real_ok), (mock_name, mock_ok)) in from_real.iter().zip(&from_mock) {
        assert_eq!(name, mock_name, "the two runs asked different questions");
        if real_ok != mock_ok {
            differ.push(format!(
                "  {:34}  server: {:<7}  mock: {}",
                name,
                if *real_ok { "ok" } else { "refused" },
                if *mock_ok { "ok" } else { "refused" }
            ));
        }
    }
    assert!(
        differ.is_empty(),
        "the mock and a real server disagree, so a test passing against the mock \
         proves nothing about these:\n{}",
        differ.join("\n")
    );
}
