# undefined-player

A focused FFmpeg video player for Linux/Wayland, with an initial macOS port.
On Linux, it decodes supported codecs directly with Vulkan Video when available,
renders with libplacebo to a Vulkan Wayland swapchain, and plays audio through
PipeWire. The macOS backend uses VideoToolbox decoding, Metal HDR presentation,
native audio through SDL3, and software decoding for unsupported codecs.

## Installation

Prebuilt Debian packages and binary tarballs are available from the
[GitHub releases](https://github.com/EriksRemess/undefined-player/releases).

Install a downloaded Debian package with `apt` so its declared runtime
dependencies are resolved automatically:

```sh
sudo apt install ./undefined-player_*.deb
```

The binary tarball is dynamically linked and requires compatible shared
libraries on the system. Extract it and run its user-local installer:

```sh
tar -xzf undefined-player-*-linux-*.tar.gz
cd undefined-player-*-linux-*
./install.sh
```

## Runtime requirements

On Linux, running requires a Wayland session, the Vulkan loader and a working Vulkan
driver, SDL3 with its Wayland and PipeWire backends, libplacebo, the Wayland
client library, Pango/Cairo, GIO, and compatible FFmpeg 8 or newer shared
libraries. Vulkan Video support enables hardware decoding; unsupported codecs
fall back to software decoding but still use Vulkan for presentation.

Wayland and PipeWire are the supported Linux display and audio backends.
The Debian package declares its shared-library dependencies; binary tarball
users must provide compatible libraries and their GPU's Vulkan driver.

## macOS preview

Build from source with Rust 1.93.1 or newer, Apple's command-line developer
tools, and Homebrew dependencies:

```sh
brew install pkgconf sdl3 ffmpeg pango
cargo build --release --locked
target/release/undefined-player ~/Movies/example.mp4
```

The macOS backend supports VideoToolbox hardware decoding (H.264, HEVC Main/Main10,
VP9 and AV1, subject to the Mac's capabilities), with software fallback for
unsupported codecs/profiles. PQ/HDR10 and HLG are converted on Metal into
floating-point, extended-linear RGB. HDR displays use EDR headroom; highlights
are tone-mapped to the available headroom, including SDR displays. HDR metadata
sets the content peak, with a 1000-nit default when absent. Normal NV12/P010
hardware surfaces stay on the GPU through presentation.

Audio, seeking, fullscreen, text/bitmap subtitles, overlays, crop/zoom,
rounded transparent corners, spatial deinterlacing, and native Now Playing/media
keys are supported. On macOS 26+, supported low-resolution SDR videos can use
Apple ML 2× upscaling during playback; `U` toggles it for comparison. 4K and HDR
sources bypass the upscaler. Dolby Vision dynamic metadata, temporal deinterlacing, and
distributable app packaging are pending. The binary uses the installed Homebrew
libraries.
`Command+Shift+P` or `Ctrl+Shift+P` opens the command palette.

## Quick start

```sh
undefined-player ~/Videos/example.mp4
undefined-player 'https://example.com/video.m3u8'
undefined-player yt:dQw4w9WgXcQ
```

Accepts one local file or HTTP(S) URL. YouTube links and `yt:ID` require
`yt-dlp` on `PATH`. There is no playlist queue.

Use **Space** to pause, **F** for fullscreen, **Left/Right** to seek, and **Q**
to quit. **Ctrl+Shift+P** opens the command palette. Saved playback positions
prompt you to resume or start over.

Add `--debug` to show FFmpeg diagnostics; playback failures always print an error.

## Documentation

- [Playback guide](USAGE.md) — all shortcuts, streams, subtitles, chapters,
  resume, cropping, and rendering behavior.
- [Configuration](CONFIGURATION.md) — saved settings, rounded corners, and
  preferred audio language.
- [Development](DEVELOPMENT.md) — building, diagnostics, packaging, and releases.

## License

undefined-player is free software licensed under the GNU General Public License
version 3 or later. See [`LICENSE`](LICENSE).
