# Development

[Installation](README.md) · [Playback](USAGE.md) · [Configuration](CONFIGURATION.md)

## Build requirements

Rust 1.93.1+ and Cargo are required. Native libraries are discovered through
`pkg-config`.

### macOS

Install Xcode or command-line developer tools with the macOS 26 SDK or newer:

```sh
brew install pkgconf sdl3 ffmpeg pango
cargo build --release --locked
target/release/undefined-player ~/Movies/example.mp4
```

Deployment requirements also depend on the linked libraries. Apple ML upscaling
is runtime-gated to macOS 26+ and supported hardware.

### Linux

Requires a C compiler, `ar`, `pkg-config`, `wayland-scanner`, Wayland protocols,
SDL3, libplacebo 7+, Vulkan, Pango/Cairo, GIO, and FFmpeg 8+ development libraries.

Ubuntu 26.04:

```sh
sudo apt install build-essential binutils pkg-config \
  wayland-protocols libwayland-dev libvulkan-dev libsdl3-dev libplacebo-dev \
  libpango1.0-dev libcairo2-dev libglib2.0-dev libavformat-dev \
  libavcodec-dev libswresample-dev libswscale-dev libavutil-dev \
  ffmpeg desktop-file-utils fonts-dejavu-core
```

Arch Linux:

```sh
sudo pacman -S --needed base-devel rust pkgconf wayland wayland-protocols \
  vulkan-headers sdl3 libplacebo pango cairo glib2 ffmpeg \
  desktop-file-utils ttf-dejavu
```

Install the Vulkan driver appropriate for the GPU, then build and run:

```sh
cargo build --release --locked
target/release/undefined-player ~/Videos/example.mp4
```

### Custom dependencies

`PKG_CONFIG_PATH` selects alternate library installations. `FFMPEG_DIR` selects
an in-place shared FFmpeg build and adds its library directories to runtime
search paths. On Linux, `WAYLAND_PROTOCOLS_DIR` overrides protocol discovery:

```sh
FFMPEG_DIR=/path/to/ffmpeg \
WAYLAND_PROTOCOLS_DIR=/path/to/wayland-protocols \
cargo build --release --locked
```

`UP_VULKAN_DEVICE` passes a device selector to FFmpeg on Linux.

## Checks and diagnostics

```sh
make check
cargo build --release --locked
git diff --check
```

`make check` runs formatting checks, native tests, Rust tests, strict Clippy
checks, and Linux desktop-file validation. Playback tests generate fixtures with `ffmpeg`
(including libx264) and use dummy SDL drivers. Linux text tests require DejaVu
fonts. No desktop or GPU is needed for these checks.

On a Mac with a desktop and supported GPU:

```sh
make check-metal
make check-upscale
```

These exercise HDR luminance/tone mapping, scaled bob filtering, and live Apple
ML processing, including the 696×480 decoder-padding regression. Also smoke-test
Finder opening, Command+O, pause, seek, fullscreen, resizing, and subtitles.

`--perf` reports shown/dropped frames, decode/display times, and audio underruns.
`--debug` enables FFmpeg and platform diagnostics. Normal macOS startup sets
`CFLOG_FORCE_DISABLE_STDERR` unless explicitly configured, suppressing repeated
Apple ML compilation messages while preserving player errors.

## macOS packaging and signing

Requires Python 3.11+ at build time; the app has no Python dependency.

```sh
make app
make dmg
make dmg CODESIGN_IDENTITY='Developer ID Application: APPS SIA (9LZX4PF57K)'
```

- App: `target/dist/Undefined Player.app`
- DMG: `target/dist/undefined-player-VERSION-macos-ARCH.dmg`
- Bundle identifier: `lv.apps.undefined-player`

`CODESIGN_IDENTITY` also applies to `make app`; its default is `-` (ad-hoc).
Developer ID builds sign libraries first, then the app with hardened runtime
and secure timestamps, then the DMG. Signatures and disk-image checksums are
verified before replacing outputs. These targets do not install or notarize.

`packaging/macos/bundle.py` copies non-system dylibs and license notices, rewrites
load paths, and creates the icon and plist. The minimum OS follows the bundled
Mach-O libraries; architecture follows the build. Fontconfig uses bundled
configuration and macOS font directories. The DMG contains an Applications
shortcut. Document declarations register an alternate viewer, not a default.

## Linux packaging

`make install` installs the binary and desktop entry under `~/.local`;
`make uninstall` removes them. Override `PREFIX` and `DESTDIR` as needed.
On macOS, these targets handle only the CLI binary.

Build a Debian package against distribution-provided dependencies:

```sh
sudo apt build-dep .
make deb
sudo apt install ../undefined-player_*_amd64.deb
```

CI uses Ubuntu 26.04; other distributions must meet `debian/control` requirements.
Use direct Cargo builds for custom source-installed dependencies that Debian’s
package manager cannot resolve.

`make tarball` creates a Linux archive under `target/dist` with a user-local
installer. It requires compatible shared libraries on the destination system.
Do not redistribute builds using FFmpeg’s `--enable-nonfree` configuration.

## Rust conventions

- Keep application policy in Rust and native adapters focused on platform/library calls.
- Make FFI wrappers safe when they can establish every precondition themselves.
  Document caller obligations with `# Safety` on unsafe APIs and explain ownership,
  lifetimes, and synchronization at raw-pointer boundaries.
- Use named fields for cross-module data whose tuple positions are ambiguous.
- Use typed errors when callers need different recovery paths (such as audio-track
  selection). Display-only failures can use the application’s string-based `Result<T>` alias.
- Prefer pattern matching for expected absence. Use `expect` with the invariant
  when a failure indicates a programming error.
- Reject poisoned state in normal operations. Cleanup may recover only when needed
  to release resources safely, with the reason documented at the recovery site.

## Architecture

| Area | Modules |
| --- | --- |
| Startup and Finder requests | `main.rs`, `macos_app.rs`, `native/app_macos.m` |
| Playback and timing | `playback.rs`, `clock.rs`, `presentation.rs` |
| Command search and argument steps | `palette.rs` |
| Demux, decode, buffering | `media.rs`, `decoder.rs`, `worker.rs`, `read_ahead.rs` |
| Audio and subtitles | `audio.rs`, `subtitles.rs` |
| Window, controls, overlays | `window.rs`, `geometry.rs`, `overlay.rs`, `pixel_font.rs` |
| Video analysis and geometry | `autocrop.rs`, `deinterlace.rs`, `luma.rs`, `zoom.rs` |
| Settings and resume | `config.rs`, `resume.rs` |
| Streams and metadata | `source.rs`, `hls.rs`, `youtube.rs`, `metadata.rs`, `chapters.rs`, `artwork.rs` |
| Desktop media controls | `mpris.rs` (Linux), `mpris_macos.rs` (macOS), shared `mpris_types.rs` |

### Rendering

Linux uses `native/video_renderer.c` with Vulkan/libplacebo. macOS uses
`native/video_renderer_macos.c` with SDL/Metal and optional VideoToolbox decoding.
Unsupported hardware decode falls back to software.

The macOS color shader samples NV12/P010 surfaces directly, decoding PQ/HLG and
mapping BT.2020/P3 into linear BT.709. HDR reference white is 203 nits; MaxCLL
precedes mastering metadata, with a 1000-nit fallback. Metal fences retain
decoder surfaces through GPU completion. Software HDR and unusual surfaces use
P010 upload and compute conversion; software SDR uses RGBA8.

Linux deinterlacing uses YADIF with valid adjacent frames and bob otherwise.
macOS uses spatial bob. Seeks and format/timestamp discontinuities invalidate
temporal references. Progressive hardware frames bypass interlace analysis.

Apple ML sessions initialize asynchronously. Enhancement preserves source SAR,
crop, rotation, and color metadata. Incompatible decoder padding is copied into
Apple’s required pixel-buffer pool. Paused results are cached.

Shared hit regions keep controls and resizing aligned. macOS content dragging
uses ordinary SDL events because Cocoa draggable hit regions consume clicks
and motion. Rounded masks follow size/scale changes and are disabled fullscreen.
SDL stays alive across Finder requests while each playback session releases
its decoder, audio, and renderer resources.

### Buffering and synchronization

The decode worker yields after an 8 ms work budget. Linux Vulkan decoding allows
24 frames across decoder and handoff queues to retain decode-ahead at 4K/8K.
Software decoding and VideoToolbox use a 256 MiB estimate, bounded to 4–24 frames.
These limits include the current and previous presentation frames; codec
reference pictures are additional. Seek completion counts both queues to avoid
deadlocking when audio starts later than video.

Demuxing targets a 250 ms audio lead. Compressed video read-ahead is bounded to
256 packets or 32 MiB, plus one packet. When video is over 250 ms late, catch-up
skips non-keyframes until a current keyframe; long GOPs can therefore freeze
the picture when decoding cannot keep pace.

Local-file buffering retains 4 MiB behind and reads 4 MiB ahead. Seeks invalidate
in-flight reads; shutdown joins workers before freeing their state. Network
reads use timeouts and the decoder’s interrupt token. HLS segment prefetching
is bounded, with native handling for encrypted and byte-range resources.

Autocrop runs on owned frame references in a background worker. Seek/toggle
changes discard stale results. VideoToolbox luma sampling locks decoder planes
read-only rather than downloading a full frame.

## CI and releases

Pushes to `main` update the `tip` prerelease; `v*` tags create versioned releases.
Pull requests do not trigger the workflow. CI checks macOS and Arch Linux,
builds/lints Debian packages on Ubuntu 26.04, and publishes Linux Debian packages
and tarballs. macOS app/DMG artifacts are not currently published by CI.
