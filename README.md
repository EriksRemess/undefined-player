# undefined-player

A focused Wayland video player. It uses FFmpeg 8 or newer, decodes supported
codecs directly with Vulkan Video when available, renders with libplacebo to a
Vulkan Wayland swapchain, and plays audio through PipeWire.

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

Running requires a Wayland session, the Vulkan loader and a working Vulkan
driver, SDL3 with its Wayland and PipeWire backends, libplacebo, the Wayland
client library, Pango/Cairo, GIO, and compatible FFmpeg 8 or newer shared
libraries. Vulkan Video support enables hardware decoding; unsupported codecs
fall back to software decoding but still use Vulkan for presentation.

Wayland and PipeWire are the only supported display and audio backends.
The Debian package declares its shared-library dependencies; binary tarball
users must provide compatible libraries and their GPU's Vulkan driver.

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
