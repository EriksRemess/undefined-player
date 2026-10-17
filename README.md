# undefined-player

A minimal video player for Linux/Wayland and macOS, with hardware decoding,
HDR, embedded subtitles, and keyboard controls.

## Installation

### Linux

Debian packages and binary tarballs are available from
[GitHub releases](https://github.com/EriksRemess/undefined-player/releases).

```sh
sudo apt install ./undefined-player_*.deb
```

For the binary tarball:

```sh
tar -xzf undefined-player-*-linux-*.tar.gz
cd undefined-player-*-linux-*
./install.sh
```

Requires Wayland, Vulkan, and PipeWire. Debian packages resolve library
dependencies through `apt`; tarball users need compatible FFmpeg 8+, SDL3,
libplacebo, Wayland, Pango/Cairo, and GIO libraries.

### macOS

Open the DMG and drag **Undefined Player** to **Applications**. Choose a package
compatible with your processor and macOS version. FFmpeg and the other required
libraries are bundled.

Launch the app to choose a video, or use Finder’s **Open With → Undefined Player**.
**Command+O** opens another video. The app does not change your default player.

Hardware decoding supports H.264, HEVC, VP9, and AV1 where available. HDR10 and
HLG are supported; Dolby Vision dynamic metadata is not. On macOS 26+, supported
low-resolution SDR videos can use Apple ML 2× upscaling. Press **U** to compare.

## Playback

**Space** pauses, **F** toggles fullscreen, **Left/Right** seeks, **S** toggles
subtitles, and **Q** quits. **Ctrl+Shift+P** opens the command palette;
**Command+Shift+P** also works on macOS.

The command-line player accepts a local file or HTTP(S) URL:

```sh
undefined-player ~/Videos/example.mp4
undefined-player 'https://example.com/video.m3u8'
undefined-player yt:dQw4w9WgXcQ
```

YouTube playback requires `yt-dlp` on `PATH`. There is no playlist queue.

## Documentation

- [Playback](USAGE.md) — shortcuts, streams, resume, cropping, and subtitles.
- [Configuration](CONFIGURATION.md) — rounded corners and audio language.
- [Development](DEVELOPMENT.md) — building, testing, packaging, and signing.

## License

[GNU General Public License, version 3 or later](LICENSE).
