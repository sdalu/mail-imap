NAME		= mail-imap

CARGO		?= cargo
RELEASE		?= yes
PREFIX		?= /usr/local
BINDIR		?= $(PREFIX)/bin
MANDIR		?= $(PREFIX)/share/man
DESTDIR		?=
INSTALL		?= install
MANDOC		?= mandoc

R_yes		= --release
R_no		=
D_yes		= release
D_no		= debug

PROFILE		= $(D_$(RELEASE))
BIN		= target/$(PROFILE)/$(NAME)
MAN1		= $(NAME).1

# The release number lives in Cargo.toml and nowhere else; the binary
# reads it through CARGO_PKG_VERSION and `check-version` keeps it that way.
VERSION		!= sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml

.PHONY: help all check check-lint check-version check-man build tests \
	tests-unit tests-examples doc version options clean distclean \
	install uninstall

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
	    MANDIR   '$(MANDIR)  ($(MAN1) goes in man1 under it)' \
	    DESTDIR  '$(DESTDIR)  (staging prefix for packaging)'
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
	MAIL_IMAP=target/debug/$(NAME) ./check-examples.sh

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
	$(CARGO) clean

distclean: clean				## clean, plus the generated documentation
	rm -rf target/doc

install: build					## install the binary and the man page under PREFIX
	$(INSTALL) -d $(DESTDIR)$(BINDIR)
	$(INSTALL) -m 755 $(BIN) $(DESTDIR)$(BINDIR)/$(NAME)
	$(INSTALL) -d $(DESTDIR)$(MANDIR)/man1
	$(INSTALL) -m 644 $(MAN1) $(DESTDIR)$(MANDIR)/man1/$(MAN1)

uninstall:					## remove what install put down
	rm -f $(DESTDIR)$(BINDIR)/$(NAME)
	rm -f $(DESTDIR)$(MANDIR)/man1/$(MAN1)
