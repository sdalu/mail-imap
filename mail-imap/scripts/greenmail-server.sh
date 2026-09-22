#!/bin/sh
# Bring up a throwaway GreenMail IMAP/SMTP server for exercising the real
# backend (src/imap/real.rs), which `make tests` never touches. Prints a
# ready-to-use config path on stdout.
#
#   sh scripts/greenmail-server.sh start   # boots it, writes greenmail.conf
#   sh scripts/greenmail-server.sh stop
#
# GreenMail jar (10 MB) is fetched once into tests-tmp/ (gitignored;
# removed by `make distclean`, kept by `make clean`).
# Ports: IMAP 3143, SMTP 3025. User tester / secret.
set -eu
# Scratch space in the tree, not a dot-directory nobody can find:
# everything here is re-creatable. `make clean` drops the runtime state,
# `make distclean` drops the fetched jar with it.
DIR="${MAIL_IMAP_GM_DIR:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)/tests-tmp}"
JAR="$DIR/greenmail.jar"
VER=2.1.9
URL="https://repo1.maven.org/maven2/com/icegreen/greenmail-standalone/$VER/greenmail-standalone-$VER.jar"
CONF="${MAIL_IMAP_GM_CONF:-$DIR/greenmail.conf}"

case "${1:-start}" in
start)
    mkdir -p "$DIR"
    [ -f "$JAR" ] || fetch -o "$JAR" "$URL" 2>/dev/null || curl -fsSL -o "$JAR" "$URL"
    pkill -f greenmail.jar 2>/dev/null || true
    sleep 1
    # -Dgreenmail.verbose logs every protocol line as "C:" / "S:",
    # which is the only way to see what real.rs actually puts on the
    # wire -- and it is what makes the user-creation line below appear.
    ( cd "$DIR" && exec nohup java -Dgreenmail.setup.test.all -Dgreenmail.verbose \
        -Dgreenmail.users=tester:secret@localhost \
        -jar greenmail.jar > "$DIR/greenmail.log" 2>&1 ) &
    # The IMAP port starts listening BEFORE the users are created, so
    # waiting on the socket alone races with a LOGIN that then fails.
    # Wait for the user-creation line instead.
    n=0
    while ! grep -q 'Creating user tester' "$DIR/greenmail.log" 2>/dev/null; do
        n=$((n + 1))
        [ "$n" -gt 60 ] && { echo "greenmail did not come up; see $DIR/greenmail.log" >&2; exit 1; }
        sleep 1
    done
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
    pkill -f greenmail.jar 2>/dev/null || true
    ;;
*)
    echo "usage: $0 {start|stop}" >&2; exit 2 ;;
esac
