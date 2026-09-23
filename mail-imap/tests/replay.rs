//! The bottom of `fetch_chunk`'s degradation ladder, driven by a
//! server that answers badly on purpose.
//!
//! `tests/wire.rs` proves what `real.rs` puts on a socket, but against
//! GreenMail, which answers *well*. The ladder exists for servers
//! GreenMail is not: `fetch_chunk` narrows its FETCH items twice and
//! then walks the batch one message at a time, and the last rung --
//! `fetch_to_result_from_headers`, rebuilding the metadata from a
//! literal `BODY.PEEK[HEADER.FIELDS ...]` -- was reached by nothing at
//! all. Mutation testing said so: inverting either of its filters
//! survived the whole suite, GreenMail included.
//!
//! So this is not a server but a script, speaking just enough IMAP to
//! walk `RealClient` down to the rung under test.
//!
//! There are two doors into `Attempt::Unparseable`, and the script can
//! open either. A well-formed response that is not the FETCH that was
//! asked for gives `Error::Unexpected`, and the ladder narrows on the
//! spot. A FETCH whose ENVELOPE the parser refuses poisons the stream
//! instead: `attempt_fetch` reconnects, retries, gets the same refusal
//! on a fresh connection, and only then narrows -- so that route walks
//! the same rungs with a reconnect at each one. Both are scripted
//! below, because only the second exercises the reconnect.
//!
//! These are NOT `#[ignore]`d: the script needs no server, so `make
//! tests` runs them offline like the rest of the suite.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use mail_imap::config::Config;
use mail_imap::imap::{ImapBackend, ImapClient};

/// The message the script serves, in the headers the last rung reads.
const SUBJECT: &str = "Rescued from the headers";
const FROM: &str = "Alice Example <alice@example.com>";
/// Deliberately not the same instant as INTERNALDATE, and in the other
/// format: this is when it was written, that is when it landed.
const SENT: &str = "Fri, 18 Sep 2026 17:30:00 +0200";
const INTERNALDATE: &str = "20-Sep-2026 09:05:00 +0000";

/// How the script answers a `UID FETCH` that asked for an ENVELOPE.
#[derive(Clone, Copy)]
enum Answer {
    /// A well-formed response that is not a FETCH. The client reads it,
    /// finds it is not what it asked for, and reports
    /// `Error::Unexpected` -- one of the two errors `attempt_fetch`
    /// turns into `Attempt::Unparseable`, so the ladder narrows.
    NotAFetch,
    /// A FETCH whose ENVELOPE is malformed. The parser refuses it,
    /// which counts as a poisoned stream, so each rung costs a
    /// reconnect before the client accepts that narrowing is needed.
    Malformed,
}

/// What the script was asked for, in order, so a test can say which
/// rungs were taken rather than only where it landed.
type Log = Arc<Mutex<Vec<String>>>;

/// Start the script on an ephemeral port. Returns the port and the log.
fn start(answer: Answer) -> (u16, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let server_log = Arc::clone(&log);
    // Each connection gets its own thread, and that is not tidiness:
    // a stalled read makes the client reconnect, and a script that was
    // still inside the first connection would never accept the second.
    // The client would then block reading a greeting that never comes
    // -- and *that* read is before `timeout` is applied (the connect
    // phase is unbounded, which TODO.md names), so the hang would be
    // permanent and would look like the code under test.
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(s) = stream else { return };
            let log = Arc::clone(&server_log);
            thread::spawn(move || serve(s, &log, answer));
        }
    });
    (port, log)
}

/// One connection's worth of dialogue.
fn serve(stream: TcpStream, log: &Log, answer: Answer) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut out = stream;
    log.lock().expect("log").push("<connected>".to_string());
    let _ = out.write_all(b"* OK [CAPABILITY IMAP4rev1] replay script ready\r\n");

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            continue;
        }
        log.lock().expect("log").push(line.clone());

        let Some((tag, rest)) = line.split_once(' ') else {
            continue;
        };
        let upper = rest.to_ascii_uppercase();
        let body = if upper.starts_with("LOGIN") {
            format!("{} OK LOGIN completed\r\n", tag)
        } else if upper.starts_with("CAPABILITY") {
            format!("* CAPABILITY IMAP4rev1\r\n{} OK CAPABILITY completed\r\n", tag)
        } else if upper.starts_with("SELECT") || upper.starts_with("EXAMINE") {
            format!(
                "* 1 EXISTS\r\n* 0 RECENT\r\n* FLAGS (\\Seen \\Answered \\Flagged)\r\n\
                 * OK [PERMANENTFLAGS (\\Seen \\Answered \\Flagged \\*)] limited\r\n\
                 * OK [UIDVALIDITY 1] UIDs valid\r\n* OK [UIDNEXT 2] predicted\r\n\
                 {} OK [READ-WRITE] SELECT completed\r\n",
                tag
            )
        } else if upper.starts_with("UID SEARCH") {
            format!("* SEARCH 1\r\n{} OK UID SEARCH completed\r\n", tag)
        } else if upper.starts_with("UID FETCH") {
            fetch_reply(tag, &upper, answer)
        } else if upper.starts_with("LOGOUT") {
            let _ = out.write_all(format!("* BYE\r\n{} OK LOGOUT completed\r\n", tag).as_bytes());
            return;
        } else {
            format!("{} OK done\r\n", tag)
        };
        if out.write_all(body.as_bytes()).is_err() {
            return;
        }
    }
}

/// The scripted answer to one `UID FETCH`, by what it asked for.
///
/// Every request carrying an ENVELOPE is answered badly, whether it is
/// the batch or the per-message retry, which is what walks the client
/// all the way down. Only the HEADER.FIELDS request -- the last rung --
/// is answered properly.
fn fetch_reply(tag: &str, items: &str, answer: Answer) -> String {
    if items.contains("HEADER.FIELDS") {
        let headers = format!("Subject: {}\r\nFrom: {}\r\nDate: {}\r\n\r\n", SUBJECT, FROM, SENT);
        // `\Recent` is in here because the tool must drop it: it is the
        // server's own bookkeeping, and every path that reports flags
        // omits it.
        return format!(
            "* 1 FETCH (UID 1 FLAGS (\\Seen \\Recent) INTERNALDATE \"{}\" \
             RFC822.SIZE 321 BODY[HEADER.FIELDS (SUBJECT FROM DATE)] {{{}}}\r\n{})\r\n\
             {} OK UID FETCH completed\r\n",
            INTERNALDATE,
            headers.len(),
            headers,
            tag
        );
    }
    match answer {
        // A LIST is a perfectly good response, and an answer to a
        // question nobody asked here.
        Answer::NotAFetch => format!(
            "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n{} OK UID FETCH completed\r\n",
            tag
        ),
        // RFC 3501 gives ENVELOPE ten fields; this has eight, so the
        // parser is still waiting for the ninth when the tagged line
        // arrives -- and reads that as part of the unfinished response.
        Answer::Malformed => format!(
            "* 1 FETCH (UID 1 ENVELOPE (\"{}\" \"Subj\" NIL NIL NIL NIL NIL NIL) \
             FLAGS (\\Seen) INTERNALDATE \"{}\" RFC822.SIZE 321)\r\n\
             {} OK UID FETCH completed\r\n",
            SENT, INTERNALDATE, tag
        ),
    }
}

/// A client pointed at the script. `timeout` is a parameter because one
/// test here is *about* the timeout firing.
fn client(port: u16, timeout: u64) -> ImapClient {
    let json = format!(
        r#"{{"server":"127.0.0.1","port":{},"username":"tester","password":"secret",
            "ssl":false,"starttls":false,"access-level":"readonly","timeout":{}}}"#,
        port, timeout
    );
    let config: Config = serde_json::from_str(&json).expect("config");
    ImapClient::connect(&config, false).expect("the script is listening")
}

/// How many connections the script served.
fn connections(log: &Log) -> usize {
    log.lock().expect("log").iter().filter(|l| *l == "<connected>").count()
}

/// Every `UID FETCH` the script was asked for, in order.
fn fetches(log: &Log) -> Vec<String> {
    log.lock()
        .expect("log")
        .iter()
        .filter(|l| l.to_ascii_uppercase().contains("UID FETCH"))
        .cloned()
        .collect()
}

// ----------------------------------------------------------- the tests

/// The whole ladder, ending on the rung nothing else reaches.
#[test]
fn an_unusable_fetch_reply_walks_down_to_the_header_fetch() {
    let (port, log) = start(Answer::NotAFetch);
    let mut c = client(port, 5);
    let hits = c
        .search_folders(&["INBOX".to_string()], "ALL", 0, None)
        .expect("a search that ends on the header rung still returns the message");

    assert_eq!(hits.len(), 1, "the message survived the ladder");
    let hit = &hits[0];

    // Rebuilt from the headers: had any earlier rung been believed,
    // there would be no subject at all to find.
    assert_eq!(hit.subject, SUBJECT);
    assert!(hit.from.contains("alice@example.com"), "from: {:?}", hit.from);

    // The two dates stay two dates on this path as well. This rung
    // fetches both INTERNALDATE and the `Date:` header, and the code
    // used to let the header stand in for a missing internaldate; they
    // are deliberately different instants here, so one standing in for
    // the other cannot pass unnoticed.
    assert_eq!(hit.date.as_deref(), Some("2026-09-20 09:05:00 +0000"));
    assert_eq!(hit.sent.as_deref(), Some(SENT));

    // `\Recent` is the server's own and is never reported.
    assert!(
        hit.flags.iter().any(|f| f.eq_ignore_ascii_case("\\Seen")),
        "flags: {:?}",
        hit.flags
    );
    assert!(
        !hit.flags.iter().any(|f| f.eq_ignore_ascii_case("\\Recent")),
        "\\Recent must not be reported: {:?}",
        hit.flags
    );

    // And the ladder was walked rather than jumped.
    let asked = fetches(&log);
    assert!(
        asked.len() >= 3,
        "expected the full/envelope/header narrowing, got {:?}",
        asked
    );
    assert!(
        asked[0].contains("BODYSTRUCTURE"),
        "the first attempt asks for everything: {:?}",
        asked[0]
    );
    assert!(
        asked.last().expect("at least one").contains("HEADER.FIELDS"),
        "the last attempt is the header fetch: {:?}",
        asked
    );
}

/// The rung is reached by narrowing, not by asking for headers first.
///
/// Worth pinning separately: a change that made `ITEMS_HEADERS` the
/// opening request would pass the test above -- same metadata, same
/// flags -- while quietly costing every ordinary fetch a round trip and
/// losing BODYSTRUCTURE, which is where the part count comes from.
#[test]
fn the_header_fetch_is_a_last_resort_and_not_the_first_ask() {
    let (port, log) = start(Answer::NotAFetch);
    let mut c = client(port, 5);
    let _ = c
        .search_folders(&["INBOX".to_string()], "ALL", 0, None)
        .expect("search");

    let asked = fetches(&log);
    let first = asked.first().expect("something was fetched");
    assert!(
        !first.contains("HEADER.FIELDS"),
        "the first fetch must not be the fallback: {:?}",
        first
    );
    assert!(
        first.contains("ENVELOPE") && first.contains("BODYSTRUCTURE"),
        "the first fetch asks for everything at once: {:?}",
        first
    );
}

/// The same ladder by the other door, which costs a reconnect a rung.
///
/// A refused ENVELOPE is not a tidy "that was not a FETCH": it leaves
/// the stream poisoned, so `attempt_fetch` reconnects and tries the
/// same items again before concluding that the items are the problem.
/// That is the one path in this tree that exercises reconnect-and-still-
/// broken, and nothing else reaches it -- GreenMail cannot be asked to
/// send a response its own parser refuses.
#[test]
fn a_refused_envelope_reconnects_at_each_rung_and_still_arrives() {
    let (port, log) = start(Answer::Malformed);
    let mut c = client(port, 5);
    let hits = c
        .search_folders(&["INBOX".to_string()], "ALL", 0, None)
        .expect("the ladder still ends with the message in hand");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].subject, SUBJECT, "rebuilt from the headers");
    assert_eq!(hits[0].sent.as_deref(), Some(SENT));

    // Each rung is asked for twice -- once, then again on a fresh
    // connection -- and the narrowing only happens after the second
    // refusal. That doubling IS the reconnect.
    let asked = fetches(&log);
    let full = asked.iter().filter(|f| f.contains("BODYSTRUCTURE")).count();
    let headers = asked.iter().filter(|f| f.contains("HEADER.FIELDS")).count();
    assert_eq!(full, 2, "the widest rung is retried once on a new connection: {:?}", asked);
    assert_eq!(headers, 2, "so is the last one: {:?}", asked);
    assert!(
        asked.last().expect("fetched").contains("HEADER.FIELDS"),
        "the walk ends on the header rung: {:?}",
        asked
    );
    assert!(
        connections(&log) > 1,
        "a refused ENVELOPE poisons the stream, so the client reconnects"
    );
}

/// The other door needs no reconnect, which is the difference worth
/// keeping visible: an unexpected response leaves the stream usable.
#[test]
fn an_unexpected_response_narrows_without_reconnecting() {
    let (port, log) = start(Answer::NotAFetch);
    let mut c = client(port, 5);
    let _ = c
        .search_folders(&["INBOX".to_string()], "ALL", 0, None)
        .expect("search");
    assert_eq!(
        connections(&log),
        1,
        "nothing about this route poisons the stream, so one connection does it"
    );
}
