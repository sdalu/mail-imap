NAME		= mail-imap

CARGO		?= cargo
RELEASE		?= yes
PREFIX		?= /usr/local
BINDIR		?= $(PREFIX)/bin
MANDIR		?= $(PREFIX)/share/man
DESTDIR		?=
INSTALL		?= install
MANDOC		?= mandoc

# Where the shell helpers live.
SCRIPTS		?= scripts

# Scratch space for the test tooling: the fetched GreenMail jar and the
# log and config it runs with. Nothing in here is authored or tracked --
# it is all re-creatable, which is what the name says. `clean` empties
# it; `distclean` removes it, jar and all.
TESTS_TMP	?= tests-tmp

R_yes		= --release
R_no		=
D_yes		= release
D_no		= debug

PROFILE		= $(D_$(RELEASE))
BIN		= target/$(PROFILE)/$(NAME)
# The man page source lives in man/; MAN1PAGE is its bare name, which
# is what it must be installed as (MAN1 carries the directory).
MANSRC		?= man
MAN1PAGE	= $(NAME).1
MAN1		= $(MANSRC)/$(MAN1PAGE)

# The release number lives in Cargo.toml and nowhere else; the binary
# reads it through CARGO_PKG_VERSION and `check-version` keeps it that way.
VERSION		!= sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml

.PHONY: help all check check-lint check-version check-man build tests \
	tests-unit tests-examples tests-server tests-server-stop doc version \
	options clean distclean install uninstall

help:						## show this help (the default)
	@echo '$(NAME) -- query and read email over IMAP, for programmatic / AI use'
	@echo ''
	@echo 'Targets:'
	@awk -F':.*## ' '/^[a-z][a-z0-9-]*:.*## /{ \
	    pre = sprintf("  %-16s ", $$1); n = split($$2, w, / /); line = ""; \
	    for (i = 1; i <= n; i++) { \
	        cand = (line == "" ? w[i] : line " " w[i]); \
	        if (length(pre) + length(cand) > 80 && line != "") { \
	            print pre line; pre = "                   "; line = w[i]; \
	        } else line = cand; \
	    } \
	    if (line != "") print pre line; \
	}' Makefile
	@echo ''
	@echo 'Variables (current value):'
	@printf '  %-16s %s\n' \
	    CARGO    '$(CARGO)' \
	    RELEASE  '$(RELEASE)  (no builds and installs the debug profile)' \
	    PREFIX   '$(PREFIX)  (install prefix)' \
	    BINDIR   '$(BINDIR)' \
	    MANDIR   '$(MANDIR)  ($(MAN1PAGE) goes in man1 under it)' \
	    DESTDIR  '$(DESTDIR)  (staging prefix for packaging)' \
	    SCRIPTS  '$(SCRIPTS)  (shell helpers)' \
	    MANSRC   '$(MANSRC)  (man page source)' \
	    TESTS_TMP '$(TESTS_TMP)  (scratch for the test server; distclean removes it)'
	@echo ''
	@echo 'There is no tag target: this tree is a subdirectory of the AiTools'
	@echo 'repository, which carries the tags. There is no deploy target'
	@echo 'either -- mail-imap runs where it is installed.'

all: check build					## check, then build

check: check-lint check-version check-man	## preflight: this tree is fit to build

check-lint:					## clippy is clean, warnings included
	$(CARGO) clippy --all-targets -- -D warnings

check-version:					## the release number is written once, in Cargo.toml
	@if grep -RnE '"[0-9]+\.[0-9]+\.[0-9]+"' src | grep -v CARGO_PKG_VERSION; then \
	    echo 'make: a version literal reappeared in src; read it from' >&2; \
	    echo '      CARGO_PKG_VERSION instead (Cargo.toml holds the number)' >&2; \
	    exit 1; fi
	@echo 'version $(VERSION) is written once (Cargo.toml)'

check-man:					## the man page is well-formed mdoc
	@if [ ! -f $(MAN1) ]; then \
	    echo 'make: $(MAN1) is missing' >&2; exit 1; fi
	$(MANDOC) -Tlint -Wall $(MAN1)

build:						## build the binary (RELEASE=no for a debug build)
	$(CARGO) build $(R_$(RELEASE))

tests: tests-unit tests-examples		## run the whole suite (mock backend, no server)

tests-unit:					## the Rust suite
	$(CARGO) test

tests-examples:					## every documented command line is still accepted by the CLI
	$(CARGO) build
	MAIL_IMAP=target/debug/$(NAME) $(SCRIPTS)/check-examples.sh

# `make tests` deliberately does not depend on these: the suite must run
# with no network and no server. The wire is the half the mock cannot
# prove (see CLAUDE.md), and this is how you get a server to prove it
# against -- it prints the config path to point -c at.
tests-server:					## start a throwaway IMAP server (GreenMail) for exercising the real backend
	@sh $(SCRIPTS)/greenmail-server.sh start

tests-server-stop:				## stop the throwaway IMAP server
	@sh $(SCRIPTS)/greenmail-server.sh stop

doc:						## generate the API documentation
	$(CARGO) doc --no-deps

version:					## print the release, bare
	@echo '$(VERSION)'

options:					## print the configuration knobs and their defaults
	@printf '  %-16s %-14s %s\n' \
	    RELEASE  'yes'           'build and install the release profile' \
	    PREFIX   '/usr/local'   'install prefix' \
	    BINDIR   '$$(PREFIX)/bin' 'where install puts the binary' \
	    MANDIR   '$$(PREFIX)/share/man' 'where install puts the man page' \
	    DESTDIR  ''             'staging prefix, prepended to every install path'
	@echo ''
	@echo 'Runtime configuration is the JSON config file, not a build knob:'
	@echo 'see the Configuration table in README.md.'

clean:						## remove what a build here made
	@sh $(SCRIPTS)/greenmail-server.sh stop
	$(CARGO) clean
	rm -rf mutants.out mutants.out.old
	rm -f $(TESTS_TMP)/greenmail.log $(TESTS_TMP)/greenmail.conf

# The GreenMail jar is a 10 MB download, so `clean` keeps it and only
# `distclean` throws it away: re-fetching it on every build would be a
# tax on the common case.
distclean: clean				## clean, plus the generated documentation and the fetched test tools
	rm -rf target/doc
	rm -rf $(TESTS_TMP)

install: build					## install the binary and the man page under PREFIX
	$(INSTALL) -d $(DESTDIR)$(BINDIR)
	$(INSTALL) -m 755 $(BIN) $(DESTDIR)$(BINDIR)/$(NAME)
	$(INSTALL) -d $(DESTDIR)$(MANDIR)/man1
	$(INSTALL) -m 644 $(MAN1) $(DESTDIR)$(MANDIR)/man1/$(MAN1PAGE)

uninstall:					## remove what install put down
	rm -f $(DESTDIR)$(BINDIR)/$(NAME)
	rm -f $(DESTDIR)$(MANDIR)/man1/$(MAN1PAGE)
