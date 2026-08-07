PREFIX ?= $(HOME)/.local
CARGO ?= cargo

BINARY := undefined-player
DESKTOP_FILE := data/undefined-player.desktop

.PHONY: all build check install uninstall clean

all: build

build:
	$(CARGO) build --release

check:
	$(CARGO) test
	$(CARGO) clippy --all-targets -- -D warnings
	desktop-file-validate $(DESKTOP_FILE)

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
