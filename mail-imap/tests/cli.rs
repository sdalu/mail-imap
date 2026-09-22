//! The CLI surface, driven as a user drives it.
//!
//! The Rust suite calls the `cli::` functions directly, so an argument
//! shape broken in `src/main.rs` passes it (CLAUDE.md says so), and
//! `scripts/check-examples.sh` covers only the shapes the documents
//! print. This covers what neither does: what the *binary* does when
//! run, for the handful of behaviours that are about the binary rather
//! than about IMAP.
//!
//! Needs no server and no network.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// Run the binary this test was built alongside, with the environment
/// scrubbed of anything that would let a real config leak in: a
/// developer's own `~/.config/mail-imap.conf` must not decide whether
/// these pass.
fn run(args: &[&str]) -> Output {
    // A counter rather than anything derived from the arguments: these
    // run in parallel, and two tests with the same argument count would
    // otherwise share a directory and delete it under each other.
    static N: AtomicU32 = AtomicU32::new(0);
    let home: PathBuf = std::env::temp_dir().join(format!(
        "mail-imap-cli-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&home).expect("temp home");
    let out = Command::new(env!("CARGO_BIN_EXE_mail-imap"))
        .args(args)
        .env_remove("MAIL_IMAP_CONFIG")
        .env_remove("XDG_CONFIG_HOME")
        .env("HOME", &home)
        .output()
        .expect("run the binary");
    std::fs::remove_dir_all(&home).ok();
    out
}

#[test]
fn tag_known_needs_neither_a_server_nor_a_config() {
    // It reads a table compiled into the binary. Requiring a config for
    // it meant the one command that needs nothing was the one refusing
    // to run for want of a file it never reads -- which stayed hidden
    // while `--mock` existed, because that was the documented way to
    // run it and it happened to make a missing config non-fatal.
    let out = run(&["tag", "known"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "`tag known` failed with no config: {}",
        stderr
    );
    assert!(
        stdout.contains("$Important") && stdout.contains("$label1"),
        "both tables should be listed, got: {}",
        stdout
    );
}

#[test]
fn tag_known_says_the_same_thing_as_json() {
    // Checked by shape rather than by parsing: `serde_json` is a
    // dependency of the crate, not a dev-dependency, so an integration
    // test cannot reach it -- and adding one for four assertions is a
    // worse trade than this.
    let out = run(&["-j", "tag", "known"]);
    assert!(out.status.success(), "`-j tag known` failed");
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    assert_eq!(line.lines().count(), 1, "JSON output is one line");
    assert!(line.starts_with('{') && line.ends_with('}'), "got: {}", line);
    for key in ["\"count\"", "\"registered\"", "\"well_known\"", "\"$Important\""] {
        assert!(line.contains(key), "{} missing from: {}", key, line);
    }
}

#[test]
fn a_command_that_needs_an_account_still_asks_for_a_config() {
    // The exemption above is for `tag known` alone: everything that
    // reaches a server must still say it has nowhere to go, rather
    // than inventing a default account.
    let out = run(&["folder", "list"]);
    assert!(!out.status.success(), "a missing config must be fatal here");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Configuration error") && stderr.contains("no config file found"),
        "the error should name the problem, got: {}",
        stderr
    );
}

#[test]
fn a_config_error_is_reported_before_anything_connects() {
    // QUICKSTART promises a typo shows up as a configuration error and
    // not as a mysterious network failure.
    let dir = std::env::temp_dir().join(format!("mail-imap-badconf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let conf = dir.join("bad.conf");
    std::fs::write(&conf, "server = \"h\"\nusername = \"u\"\nnosuchkey = 1\n").expect("write");
    let out = run(&["-c", conf.to_str().expect("path"), "folder", "list"]);
    std::fs::remove_dir_all(&dir).ok();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Configuration error") && stderr.contains("nosuchkey"),
        "an unknown key should be named, got: {}",
        stderr
    );
}

/// The mock is a development build's, and this is that build.
#[cfg(feature = "mock")]
#[test]
fn a_development_build_has_the_mock() {
    let out = run(&["--mock", "-j", "folder", "list"]);
    assert!(
        out.status.success(),
        "--mock should work in a build with the feature: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// ... and a release build does not. There is no way to assert the
/// absence from inside a build that has it, so this records where the
/// claim is actually checked rather than pretending to check it here.
#[cfg(not(feature = "mock"))]
#[test]
fn a_release_build_has_no_mock() {
    let out = run(&["--mock", "folder", "list"]);
    assert!(!out.status.success(), "--mock must not be accepted");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument"),
        "clap should reject it as unknown, got: {}",
        stderr
    );
}
