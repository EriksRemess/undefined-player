# Development

This document covers building, testing, diagnostics, packaging, and custom
dependency setups for undefined-player. User installation and playback are
documented in [README.md](README.md).

## Build requirements

- Rust 1.93.1 or newer and Cargo (the crate uses Rust 2024 edition)
- a C compiler, `ar`, and `pkg-config`
- `wayland-scanner` and the stable Wayland protocol definitions
- development files for SDL3, libplacebo, Vulkan, Wayland, Pango, Cairo, and
  GIO
- FFmpeg 8 or newer development files for `libavformat`, `libavcodec`,
  `libswresample`, `libswscale`, and `libavutil`; Vulkan Video support is needed for hardware
  decoding

On Ubuntu 26.04, install the packaged build dependencies with:

```sh
sudo apt install build-essential binutils pkg-config \
  wayland-protocols libwayland-dev libvulkan-dev libsdl3-dev libplacebo-dev \
  libpango1.0-dev libcairo2-dev libglib2.0-dev libavformat-dev \
  libavcodec-dev libswresample-dev libswscale-dev libavutil-dev
```

On Arch Linux, install the build dependencies and AMD Vulkan driver with:

```sh
sudo pacman -S --needed base-devel rust pkgconf wayland wayland-protocols \
  vulkan-headers sdl3 libplacebo pango cairo glib2 ffmpeg vulkan-radeon
```

## Build and run

```sh
cargo build --release --locked
target/release/undefined-player ~/Videos/example.mp4
```

FFmpeg and the other libraries are located through `pkg-config` by default, so
`PKG_CONFIG_PATH` can select non-system installations. Set `FFMPEG_DIR` to use
an in-place custom FFmpeg build instead; its shared-library directories are
then recorded as runtime search paths. `WAYLAND_PROTOCOLS_DIR` can override the
Wayland protocol data directory when needed:

```sh
FFMPEG_DIR=/path/to/ffmpeg \
WAYLAND_PROTOCOLS_DIR=/path/to/wayland-protocols \
cargo build --release --locked
```

Vulkan device selection is automatic. On a multi-GPU system,
`UP_VULKAN_DEVICE` can pass an explicit device selector to FFmpeg.

## Rust and native code

`src/main.rs` handles startup and command-line results. The runtime is split by
responsibility:

| Modules | Responsibility |
| --- | --- |
| `playback.rs`, `clock.rs` | Event loop, command palette, seeking, and playback synchronization |
| `resume.rs` | Local-file and YouTube resume storage and startup choice |
| `chapters.rs` | Chapter timestamps, timeline markers, and previous/next navigation |
| `artwork.rs` | Embedded cover extraction and temporary file lifetime for MPRIS |
| `media.rs`, `decoder.rs`, `worker.rs` | Demuxing, owned FFmpeg frames, and decoder worker |
| `read_ahead.rs` | Bounded local-file buffering, background reads, and byte seeking |
| `config.rs` | Validated XDG preferences and atomic saves from the command palette |
| `source.rs` | Local-path and HTTP(S) input classification |
| `audio.rs` | Conversion, bounded audio queues, and timestamp scheduling |
| `window.rs`, `geometry.rs` | Window lifetime, actions, and shared control hit regions |
| `renderer.rs`, `overlay.rs`, `pixel_font.rs` | Renderer ownership, overlay pixels, and placement |
| `autocrop.rs` | Background luma sampling and stable black-border detection |
| `deinterlace.rs`, `luma.rs`, `zoom.rs` | Picture analysis, luma sampling, and fit/fill behavior |
| `metadata.rs`, `subtitles.rs`, `presentation.rs` | Media labels, subtitle content, and UI state |
| `mpris.rs`, `mpris.xml` | MPRIS metadata, playback state, commands, and interface definition |

SDL resize handling and Wayland dragging call the same Rust geometry function
as playback. Close buttons take precedence over resize edges, then the scrubber,
then window dragging. Rendering shares the physical control dimensions.

`src/overlay.rs` owns the title and control pixel buffers, text caches, Unicode
mask scaling, truncation, and drawing rectangles. `src/pixel_font.rs` contains
the complete printable ASCII font. Canonically decomposable Latin letters use
the same pixel bodies with supported accents above or below, preserving their
size and cell spacing. Other characters use shaped masks. Prepared frames borrow
the Rust buffers for a synchronous renderer call; image revisions avoid repeated
GPU uploads. Text masks have a one-pixel dark shadow that fades with the
foreground; timeline geometry is not shadowed.
Detail lines use a fixed spacing that accommodates accents. Oversized detail
panels scale uniformly to fit above the playback controls when the window shrinks.
The renderer borrows its window and takes references to owned video frames.

`src/resume.rs` stores positions in a versioned text file under the XDG state
directory. Each record contains a SHA-256 hash of the canonical path, seconds,
file size, and modification time (seconds and nanoseconds). Non-live YouTube
videos instead hash `youtube:VIDEO_ID`, using the ID reported by yt-dlp, with
zeroes in the file metadata fields. Expiring stream URLs are never stored.
FFmpeg's existing libavutil dependency supplies the hash. Updates lock
`resume.lock`, reload the
records, and atomically replace the data file. The lock uses a separate file
because replacing the data file changes its inode. The startup prompt can render
on a black frame when audio buffering delays the first decoded picture.

`src/autocrop.rs` samples owned frame references on a bounded background worker
when enabled. It checks luma edges, stabilizes crop bounds, and rejects stale
results after toggles, every explicit seek, or timeline discontinuities. Its
area and brightness checks allow narrow pictures while rejecting small highlights.
The FFmpeg adapter exposes 8–16-bit luma planes and downloads hardware frames
when sampling requires it; the worker finishes before the renderer destroys its
Vulkan device.

`src/read_ahead.rs` owns the input file and its read worker. Its buffer retains
up to 4 MiB of history and prefetches 4 MiB ahead. A mutex protects buffer state,
but positional file reads run outside the lock so network stalls do not prevent
consumption of buffered data. Seeks outside the buffer invalidate in-flight
reads, including their errors. The C FFmpeg adapter only supplies AVIO callbacks
and translates EOF; it does not use FFmpeg's `async` protocol. Closing the AVIO
joins the Rust worker before freeing the callback state.

HTTP(S) sources bypass the local-file AVIO adapter and use FFmpeg's network
protocol implementation. Servers should support byte ranges for reliable seeks
and containers whose metadata is not stored at the beginning. Network reads have
a 15-second timeout and bounded retries for disconnects and transient 503/504
responses. Their FFmpeg interrupt callback shares the decode worker's lifetime,
so closing playback cancels a blocked protocol read before joining the worker.
`src/youtube.rs` resolves YouTube page URLs through an optional `yt-dlp` on
`PATH`, before creating the player window. It runs without a shell or yt-dlp
config, requests one video in simulation mode, and validates its output before
passing the stream to FFmpeg. It prefers an HLS master, with a combined
HTTP audio/video format as fallback. JSON string output preserves title/channel
metadata without adding a JSON dependency. The original page URL remains the
MPRIS URI; stream URLs are not persisted. Extraction failures and rejected
streams return normal player errors. HLS HTTP rejection errors are preserved
when FFmpeg would otherwise skip segments and return EOF.

For network HLS masters, `src/hls.rs` selects video by advertised resolution,
frame rate, then bandwidth before FFmpeg opens the media playlists. The filtered
master retains the selected variant's audio/subtitle groups and global tags.
Unknown quality metadata retains full discovery; failure to open or initialize
the selected rendition retries full discovery and the existing decoder fallback.
Full discovery retains the same network IO, error handling, and caption support.
Local HLS files keep FFmpeg's full discovery and stream ranking.

`native/hls_input.h` preserves the master URL after redirects and starts the next
ordinary HTTP media segment connection in a background thread. FFmpeg's `async`
protocol then buffers up to 4 MiB ahead, plus 4 MiB of read-back history, per open
segment. This buffers compressed bytes without growing the decoded frame queues.
Manifest, byte-range, encrypted, and unknown resource types use native protocol
handling; builds without `async` also retain native reads. Closing or seeking
cancels and joins pending connections before freeing their callback state.
Only the selected HLS audio stream is read during playback; switching tracks
updates FFmpeg's discard flags before seeking. HTTP failures during discovery
trigger a replay from the start with that selection, so an unused audio rendition
cannot leave a stale error behind. HTTP error capture also covers requests that
do not use prefetching. Network HLS permits extensionless segments while retaining
the protocol restrictions, and opened subtitle decoders enable their HLS streams
so WebVTT tracks produce cues.
Subtitle playlists and segments are identified from their manifest declarations,
including redirected and extensionless URLs. Each caption resource is buffered
with an 8 MiB limit; unavailable captions become empty subtitle data without
changing video/audio error handling. Live subtitle URL tracking retains only the
current playlist window. URL resolution uses the existing GLib dependency.
`--perf` reports input-open and stream-discovery times as well as playback stats.

`src/zoom.rs` owns the fill mode, saved autocrop preference, and centered source
crop geometry. Fill runs after autocrop and accounts for sample aspect ratio and
rotation. The native adapter keeps the target at the viewport bounds, preserving
subtitle and control placement. Zoom waits for a usable crop, with full-frame
fallback when sampling is unavailable or manually disabled, and bypasses integer
scaling until normal fitting is restored.
Bitmap subtitles fit their complete authored canvas inside the final visible
source crop, after both C and Z. This preserves captions in removed bars while
retaining their proportions and following the video's SAR and rotation.

`src/deinterlace.rs` owns Auto/On/Off selection, picture analysis, and temporal
adjacency checks. The decode path samples bounded luma regions from three frames,
retaining adjacent scan lines and their parity. Consistent temporal evidence
overrides interlacing flags; inconclusive samples retain the last decision, or
fall back to flags before a decision is available. Seeks, timestamp gaps, and
dimension changes reset the detector. Decisions travel with each `VideoFrame`.
`src/luma.rs` shares depth-independent sampling with autocrop. Hardware frames
flagged as interlaced are read back on the decode worker; progressive hardware
frames bypass analysis to preserve the normal playback path.
Playback retains one preceding frame and borrows the next queued frame for GPU
YADIF filtering. Every seek clears the previous frame; timestamp gaps and changes
in dimensions or pixel format exclude references. Missing references use bob
filtering. Mapped references must also match the actual GPU plane dimensions,
formats, component layout, and sample encoding; incompatible references use the
same fallback. The renderer maps references into separate texture sets, unmaps them
after each render, and reports deinterlacing failures. Presentation keeps the
source frame cadence, rendering its first field. Progressive frames bypass the
filter in Auto mode; On defaults unmarked frames to top-field-first.

`native/text_raster.c` adapts GLib normalization/decomposition and Pango/Cairo
shaping into natural-size masks. Rust owns their lifetime through a wrapper and fits the
masks into the overlay cells. `native/video_renderer.c` handles FFmpeg/Vulkan
and libplacebo integration, uploads the prepared overlays, and renders subtitles.
The SDL and Wayland adapters retain native API translation. The GIO/MPRIS
adapter handles registration, serialization, and signals; Rust owns its callback
state. Its private GIO context is dispatched on the owning thread, and callbacks
are unregistered before their Rust state is freed.

## Diagnostics and checks

Add `--perf` to print shown/dropped frame rates and average decoder-fill and
display times every two seconds:

```sh
target/release/undefined-player --perf ~/Videos/example.mp4
```

Run the repository checks with:

```sh
make check
cargo build --release --locked
git diff --check
```

`make check` runs the native renderer logic tests, Rust tests, strict Clippy checks,
and desktop-file validation. The Rust overlay tests cover printable symbols,
Unicode accents, mask bounds, title truncation, caching, and control placement.
Geometry and MPRIS state tests run without a desktop or D-Bus session.
The text rendering tests run on the CPU and need the
DejaVu fonts (`fonts-dejavu-core` on Ubuntu, `ttf-dejavu` on Arch Linux).
The playback regression tests use the `ffmpeg` command (including
its libx264 encoder) to generate small fixtures and SDL's dummy drivers, so
they do not require a Wayland session, audio server, or GPU. Install the
`ffmpeg` package to run them.

## Local installation

Build and install the player and its video-file associations into `~/.local`:

```sh
make install
```

Use `make uninstall` to remove those installed files. Override `PREFIX` and
optionally `DESTDIR` for another installation root.

## Debian package

The package is built on Ubuntu 26.04 in CI. Other Debian-based distributions
must satisfy the minimum versions declared in `debian/control`.

Install the build dependencies declared in `debian/control`, then build an
unsigned binary package:

```sh
sudo apt build-dep .
make deb
sudo apt install ../undefined-player_*_amd64.deb
```

Installing through `apt` resolves the shared-library runtime dependencies
recorded in the package and normally installs the recommended PipeWire audio
setup. A suitable Vulkan driver remains hardware-specific and must be installed
for the user's GPU.

Debian's package manager cannot treat libraries installed directly from source
as satisfying package dependencies. Debian packages should therefore be built
against distribution-provided development packages. A custom FFmpeg or other
source-installed library should instead be selected with `FFMPEG_DIR` or
`PKG_CONFIG_PATH` when building directly with Cargo.

## Binary tarball

Build a versioned binary tarball with:

```sh
make tarball
```

The archive is written under `target/dist/` and includes a user-local
`install.sh`. Unlike the Debian package, the tarball cannot install or validate
runtime dependencies. It is intended for compatible Linux systems where the
required shared libraries, including custom source installations, are already
managed by the user.

## Distribution licensing

Release packages are built against redistributable distribution-provided
libraries. Do not redistribute builds linked against an FFmpeg configuration
created with `--enable-nonfree`.

## Automated releases

Pushes to `main` update the rolling `tip` prerelease. Tags matching `v*` create
versioned releases. Pull requests do not run the release workflow. The workflow
uses the latest stable Rust toolchain for both build jobs. It builds and
checks on Arch Linux, builds and lints the Debian package on Ubuntu 26.04,
creates the binary tarball, and uploads both package formats. User builds
require Rust 1.93.1 or newer, as declared in `Cargo.toml`.
