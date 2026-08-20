PREFIX ?= $(HOME)/.local
CARGO ?= cargo

BINARY := undefined-player
DESKTOP_FILE := data/undefined-player.desktop
VERSION := $(shell awk -F'"' '/^version = / { print $$2; exit }' Cargo.toml)
ARCH ?= $(shell uname -m)
TARBALL_NAME := $(BINARY)-$(VERSION)-linux-$(ARCH)
TARBALL_BINARY ?= target/release/$(BINARY)

.PHONY: all build check deb tarball install uninstall clean

all: build

build:
	$(CARGO) build --release

check:
	$(CARGO) test
	$(CARGO) clippy --all-targets -- -D warnings
	desktop-file-validate $(DESKTOP_FILE)

deb:
	dpkg-buildpackage --build=binary --no-sign

tarball: build
	rm -rf target/dist/$(TARBALL_NAME)
	install -Dm755 $(TARBALL_BINARY) target/dist/$(TARBALL_NAME)/bin/$(BINARY)
	install -Dm644 $(DESKTOP_FILE) target/dist/$(TARBALL_NAME)/share/applications/$(BINARY).desktop
	install -Dm644 README.md target/dist/$(TARBALL_NAME)/README.md
	install -Dm644 debian/copyright target/dist/$(TARBALL_NAME)/COPYRIGHT
	install -Dm644 packaging/tarball/INSTALL.txt target/dist/$(TARBALL_NAME)/INSTALL.txt
	install -Dm755 packaging/tarball/install.sh target/dist/$(TARBALL_NAME)/install.sh
	tar -C target/dist -czf target/dist/$(TARBALL_NAME).tar.gz $(TARBALL_NAME)
	@echo "Built target/dist/$(TARBALL_NAME).tar.gz"

install: build
	install -Dm755 target/release/$(BINARY) $(DESTDIR)$(PREFIX)/bin/$(BINARY)
	install -Dm644 $(DESKTOP_FILE) $(DESTDIR)$(PREFIX)/share/applications/$(BINARY).desktop
	@if command -v update-desktop-database >/dev/null 2>&1; then \
		update-desktop-database $(DESTDIR)$(PREFIX)/share/applications; \
	fi

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/$(BINARY)
	rm -f $(DESTDIR)$(PREFIX)/share/applications/$(BINARY).desktop
	@if command -v update-desktop-database >/dev/null 2>&1; then \
		update-desktop-database $(DESTDIR)$(PREFIX)/share/applications; \
	fi

clean:
	$(CARGO) clean
