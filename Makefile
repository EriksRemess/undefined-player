PREFIX ?= $(HOME)/.local
CARGO ?= cargo

BINARY := undefined-player
DESKTOP_FILE := data/undefined-player.desktop
VERSION := $(shell awk -F'"' '/^version = / { print $$2; exit }' Cargo.toml)
ARCH ?= $(shell uname -m)
TARBALL_NAME := $(BINARY)-$(VERSION)-linux-$(ARCH)
TARBALL_BINARY ?= target/release/$(BINARY)
HOST_OS := $(shell uname -s)

.PHONY: all build check check-native check-metal check-upscale deb tarball install uninstall clean

all: build

build:
	$(CARGO) build --release

check: check-native
	$(CARGO) test
	$(CARGO) clippy --all-targets -- -D warnings
ifneq ($(HOST_OS),Darwin)
	desktop-file-validate $(DESKTOP_FILE)
endif

ifeq ($(HOST_OS),Darwin)
target/tests/video_color_macos_shader.h: native/video_color_macos.metal
	mkdir -p target/tests
	python3 -c 'import json; print("static const char video_color_shader[] = " + json.dumps(open("native/video_color_macos.metal").read()) + ";")' > $@

target/tests/video-color.o: native/video_color_macos.m native/video_color_macos.h target/tests/video_color_macos_shader.h
	$(CC) -std=c11 -O2 -Wall -Wextra -Werror -fobjc-arc -Itarget/tests \
		$(shell pkg-config --cflags sdl3 libavutil libswscale) -c $< -o $@

target/tests/video-upscaler.o: native/video_upscaler_macos.m native/video_upscaler_macos.h
	mkdir -p target/tests
	$(CC) -std=c11 -O2 -Wall -Wextra -Werror -fobjc-arc \
		$(shell pkg-config --cflags sdl3 libavutil libswscale) -c $< -o $@

target/tests/video-renderer: native/video_renderer_macos_tests.c native/video_renderer_macos.c native/video_renderer.h native/window_shape.h native/ffmpeg_luma.h native/ffmpeg_compat.h target/tests/video-color.o target/tests/video-upscaler.o
	$(CC) -std=c11 -O2 -Wall -Wextra -Werror \
		$(shell pkg-config --cflags sdl3 pangocairo libavutil libswscale) \
		native/video_renderer_macos_tests.c target/tests/video-color.o target/tests/video-upscaler.o -o $@ \
		$(shell pkg-config --libs sdl3 pangocairo libavutil libswscale) \
		-framework VideoToolbox -framework CoreVideo -framework CoreMedia -framework Metal \
		-framework QuartzCore -framework Foundation -Wl,-dead_strip

target/tests/media-controls: native/media_controls_macos_tests.m native/media_controls_macos.m native/media_controls_macos.h
	mkdir -p target/tests
	$(CC) -std=c11 -O2 -Wall -Wextra -Werror -fobjc-arc $< -o $@ \
		-framework AppKit -framework MediaPlayer -framework Foundation

target/tests/video-upscaler: native/video_upscaler_macos_tests.m native/video_upscaler_macos.m native/video_upscaler_macos.h native/macos_logging.h
	mkdir -p target/tests
	$(CC) -std=c11 -O2 -Wall -Wextra -Werror -fobjc-arc \
		$(shell pkg-config --cflags sdl3 libavutil libswscale) $< -o $@ \
		$(shell pkg-config --libs sdl3 libavutil libswscale) \
		-framework Foundation -framework VideoToolbox -framework CoreVideo -framework CoreMedia

check-native: target/tests/video-renderer target/tests/media-controls target/tests/video-upscaler
	target/tests/video-renderer
	target/tests/media-controls
	target/tests/video-upscaler

# Requires a desktop/Metal GPU. Kept separate from CPU-only CI checks.
check-metal: target/tests/video-renderer
	target/tests/video-renderer --metal

check-upscale: target/tests/video-upscaler
	target/tests/video-upscaler --apple-ml
else
check-native:
	mkdir -p target/tests
	$(CC) -std=c11 -O2 -Wall -Wextra -Werror -ffunction-sections -fdata-sections \
		$(shell pkg-config --cflags libplacebo sdl3 pangocairo) \
		$(if $(FFMPEG_DIR),-I"$(FFMPEG_DIR)",$(shell pkg-config --cflags libavutil)) \
		native/video_renderer_tests.c -o target/tests/video-renderer \
		$(shell pkg-config --libs libplacebo sdl3 pangocairo) -lm -Wl,--gc-sections
	target/tests/video-renderer
endif

deb:
	dpkg-buildpackage --build=binary --no-sign

tarball: build
	rm -rf target/dist/$(TARBALL_NAME)
	install -Dm755 $(TARBALL_BINARY) target/dist/$(TARBALL_NAME)/bin/$(BINARY)
	install -Dm644 $(DESKTOP_FILE) target/dist/$(TARBALL_NAME)/share/applications/$(BINARY).desktop
	install -Dm644 README.md target/dist/$(TARBALL_NAME)/README.md
	install -m644 USAGE.md CONFIGURATION.md DEVELOPMENT.md target/dist/$(TARBALL_NAME)/
	install -Dm644 LICENSE target/dist/$(TARBALL_NAME)/LICENSE
	install -Dm644 debian/copyright target/dist/$(TARBALL_NAME)/COPYRIGHT
	install -Dm644 packaging/tarball/INSTALL.txt target/dist/$(TARBALL_NAME)/INSTALL.txt
	install -Dm755 packaging/tarball/install.sh target/dist/$(TARBALL_NAME)/install.sh
	tar -C target/dist -czf target/dist/$(TARBALL_NAME).tar.gz $(TARBALL_NAME)
	@echo "Built target/dist/$(TARBALL_NAME).tar.gz"

install: build
ifeq ($(HOST_OS),Darwin)
	install -d $(DESTDIR)$(PREFIX)/bin
	install -m755 target/release/$(BINARY) $(DESTDIR)$(PREFIX)/bin/$(BINARY)
else
	install -Dm755 target/release/$(BINARY) $(DESTDIR)$(PREFIX)/bin/$(BINARY)
	install -Dm644 $(DESKTOP_FILE) $(DESTDIR)$(PREFIX)/share/applications/$(BINARY).desktop
	@if command -v update-desktop-database >/dev/null 2>&1; then \
		update-desktop-database $(DESTDIR)$(PREFIX)/share/applications; \
	fi
endif

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/$(BINARY)
ifneq ($(HOST_OS),Darwin)
	rm -f $(DESTDIR)$(PREFIX)/share/applications/$(BINARY).desktop
	@if command -v update-desktop-database >/dev/null 2>&1; then \
		update-desktop-database $(DESTDIR)$(PREFIX)/share/applications; \
	fi
endif

clean:
	$(CARGO) clean
