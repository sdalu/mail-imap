#!/bin/sh
# Bring up a throwaway GreenMail IMAP/SMTP server for exercising the real
# backend (src/imap/real.rs), which `make tests` never touches. Prints a
# ready-to-use config path on stdout.
#
#   sh scripts/greenmail-server.sh start   # boots it, writes greenmail.conf
#   sh scripts/greenmail-server.sh stop
#
# `start` prints the config path on stdout and nothing else, so it can
# be captured (CONF=$(sh scripts/greenmail-server.sh start)). Progress
# goes to stderr, where it stays out of that.
#
# GreenMail jar (10 MB) is fetched once into tests-tmp/ (gitignored;
# removed by `make distclean`, kept by `make clean`).
# Ports: IMAP 3143, SMTP 3025. User tester / secret.
set -eu
# Scratch space in the tree, not a dot-directory nobody can find:
# everything here is re-creatable. `make clean` drops the runtime state,
# `make distclean` drops the fetched jar with it.
# `CDPATH= cd` neutralises a set CDPATH for that one command, which is
# the point -- shellcheck reads the empty assignment as a typo.
# shellcheck disable=SC1007
DIR="${MAIL_IMAP_GM_DIR:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)/tests-tmp}"
JAR="$DIR/greenmail.jar"
VER=2.1.9
URL="https://repo1.maven.org/maven2/com/icegreen/greenmail-standalone/$VER/greenmail-standalone-$VER.jar"
CONF="${MAIL_IMAP_GM_CONF:-$DIR/greenmail.conf}"
# The server is stopped by the pid we started, never by matching the
# command line: `pkill -f greenmail.jar` also kills anything that merely
# MENTIONS that string -- a build script, an editor, the very shell
# running this -- and it did exactly that during development.
PIDFILE="$DIR/greenmail.pid"

# Stop the server this script started, if it is still up. Says so on
# stderr only when there was something to stop, so `make clean` calling
# it every time stays quiet.
stop_server() {
    pid=""
    [ -f "$PIDFILE" ] && pid="$(cat "$PIDFILE" 2>/dev/null || true)"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        echo "==> stopping GreenMail (pid $pid)" >&2
        kill "$pid" 2>/dev/null || true
        # Give it a moment to go, so a start right after this one does
        # not race it for the port.
        n=0
        while kill -0 "$pid" 2>/dev/null; do
            n=$((n + 1))
            [ "$n" -gt 20 ] && { kill -9 "$pid" 2>/dev/null || true; break; }
            sleep 1
        done
    fi
    rm -f "$PIDFILE"
}

case "${1:-start}" in
start)
    mkdir -p "$DIR"
    if [ ! -f "$JAR" ]; then
        echo "==> fetching GreenMail $VER (once; distclean removes it)" >&2
        fetch -o "$JAR" "$URL" 2>/dev/null || curl -fsSL -o "$JAR" "$URL"
    fi
    stop_server
    echo "==> starting GreenMail (IMAP 127.0.0.1:3143, SMTP 127.0.0.1:3025)" >&2
    # -Dgreenmail.verbose logs every protocol line as "C:" / "S:",
    # which is the only way to see what real.rs actually puts on the
    # wire -- and it is what makes the user-creation line below appear.
    ( cd "$DIR" && exec nohup java -Dgreenmail.setup.test.all -Dgreenmail.verbose \
        -Dgreenmail.users=tester:secret@localhost \
        -jar greenmail.jar > "$DIR/greenmail.log" 2>&1 ) &
    # `exec` inside the subshell means java inherits its pid, so this is
    # the JVM's own.
    echo $! > "$PIDFILE"
    # The IMAP port starts listening BEFORE the users are created, so
    # waiting on the socket alone races with a LOGIN that then fails.
    # Wait for the user-creation line instead.
    n=0
    while ! grep -q 'Creating user tester' "$DIR/greenmail.log" 2>/dev/null; do
        n=$((n + 1))
        [ "$n" -gt 60 ] && { echo "greenmail did not come up; see $DIR/greenmail.log" >&2; exit 1; }
        sleep 1
    done
    echo "==> GreenMail is up; config: $CONF" >&2
    cat > "$CONF" <<CONFEOF
server   = "127.0.0.1"
port     = 3143
username = "tester"
password = "secret"
ssl      = false
starttls = false
access-level = full
CONFEOF
    echo "$CONF"
    ;;
stop)
    stop_server
    ;;
*)
    echo "usage: $0 {start|stop}" >&2; exit 2 ;;
esac
