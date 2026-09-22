#!/bin/sh
#
# Every command line shown in the documentation is accepted by the CLI.
#
# The Rust suite calls the cli:: functions directly, so an argument shape
# broken in src/main.rs passes it.  This script closes that gap the only
# way it can be closed: by running the binary over every example the
# documents print, against the mock backend so no server is touched.
#
# A clap usage error is a documentation bug -- the document shows a
# command the tool would refuse to parse.  A runtime error (no such UID,
# unknown folder) is not: the mock account is not the account the
# examples were written against.
#
# Usage: check-examples.sh [file ...]      (default: the project's docs)

set -eu

BIN=${MAIL_IMAP:-./target/debug/mail-imap}

if [ ! -x "$BIN" ]; then
	echo "check-examples: no binary at $BIN (run: make build RELEASE=no)" >&2
	exit 1
fi

if [ $# -gt 0 ]; then
	files=$*
else
	files=
	for f in README.md QUICKSTART.md DESIGN.md mail-imap.1; do
		[ -f "$f" ] && files="$files $f"
	done
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

: > "$work/lines"
for f in $files; do
	[ -f "$f" ] || continue
	# Only real command lines, and only inside fenced blocks: a 'bash'
	# block's lines that invoke the tool, and a 'console' block's
	# lines that a '$' prompt marks as input.  Program output can
	# begin with the tool's own name too ("mail-imap 0.1.0 ..."), so
	# the fence and the prompt are what tell input from output.
	case $f in
	*.1 | *.[2-9])
		# A man page fences its examples in .Bd -literal / .Ed
		# rather than in backticks.
		awk -v f="$f" '
		    /^\.Bd/ { inbd = 1; next }
		    /^\.Ed/ { inbd = 0; next }
		    !inbd { next }
		    {
		        line = $0
		        sub(/^\$ /, "", line)
		        if (line !~ /^mail-imap /) { next }
		        sub(/^mail-imap /, "", line)
		        print f "\t" line
		    }
		' "$f" |
		    sed 's/  *#.*$//; s/[ 	]*$//' >> "$work/lines"
		continue
		;;
	esac

	# No apostrophes inside this awk program: it is single-quoted.
	awk -v f="$f" '
	    /^```/ { lang = substr($0, 4); infence = !infence; block++; next }
	    !infence { next }
	    /^\$ / { prompted[block] = 1 }
	    {
	        line = $0
	        # A "$ " prompt marks input wherever it appears. In a block
	        # that uses prompts, an unprompted line is output and is
	        # skipped -- output can begin with the tool name too, as in
	        # "mail-imap 0.1.0 (mock backend)".
	        if (line ~ /^\$ /) { sub(/^\$ /, "", line) }
	        else if (prompted[block]) { next }
	        # The documents spell the binary three ways: bare (installed),
	        # ./target/debug/... and target/debug/... in a working tree.
	        if (line !~ /^(\.\/)?(target\/[a-z]+\/)?mail-imap /) { next }
	        sub(/^(\.\/)?(target\/[a-z]+\/)?mail-imap /, "", line)
	        print f "\t" line
	    }
	' "$f" |
	    sed 's/  *#.*$//; s/[ 	]*$//' >> "$work/lines"
done

checked=0
failed=0

while IFS='	' read -r f args; do
	[ -n "${args:-}" ] || continue
	# The examples name a real account; the mock ignores config
	# entirely, which is what makes them runnable here.
	args=$(printf '%s\n' "$args" |
	    sed 's|--config [^ ]*|--mock|; s|-c [^ ]*|--mock|')
	case $args in
	*--mock*) ;;
	*) args="--mock $args" ;;
	esac

	# Word splitting and quote handling are the point: the line runs
	# exactly as a reader would paste it into a shell.  The input is
	# this repository's own documentation, not anything untrusted.
	# Run it in a child shell rather than with eval: a documented line
	# with unbalanced quotes is itself a finding, and must not take
	# this script down with it.
	out=$(printf '%s %s\n' "$BIN" "$args" | sh 2>&1 || true)
	checked=$((checked + 1))

	case $out in
	*"unexpected argument"* | *"unrecognized subcommand"* | \
	*"invalid value"* | *"required arguments"* | \
	*"unexpected value"* | *"error: the following"*)
		failed=$((failed + 1))
		echo "$f: the CLI refuses a documented command line:" >&2
		echo "    mail-imap $args" >&2
		printf '%s\n' "$out" | sed -n '1,2p' | sed 's/^/    /' >&2
		;;
	esac
done < "$work/lines"

if [ "$checked" -eq 0 ]; then
	echo "check-examples: no example command lines found in:$files" >&2
	echo "check-examples: refusing to pass on an empty check" >&2
	exit 1
fi

if [ "$failed" -ne 0 ]; then
	echo "check-examples: $failed of $checked documented command line(s) rejected" >&2
	exit 1
fi

echo "check-examples: $checked documented command line(s), all accepted"
