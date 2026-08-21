# undefined-player

A Wayland video-player MVP. It uses FFmpeg 8 or newer, decodes supported codecs directly
with Vulkan Video when available, renders with libplacebo to a Vulkan Wayland
swapchain, and plays audio through PipeWire.

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

Source-building, custom FFmpeg, packaging, diagnostics, and release checks are
documented in [DEVELOPMENT.md](DEVELOPMENT.md).

## Runtime requirements

Running requires a Wayland session, the Vulkan loader and a working Vulkan
driver, SDL3 with its Wayland and PipeWire backends, libplacebo, the Wayland
client library, Pango/Cairo, GIO, and compatible FFmpeg 8 or newer shared
libraries. Vulkan Video support enables hardware decoding; unsupported codecs
fall back to software decoding but still use Vulkan for presentation.

When the player is built against Ubuntu 26.04's packaged libraries, the base
runtime packages can be installed with:

```sh
sudo apt install libvulkan1 libsdl3-0 libplacebo360 libwayland-client0 \
  libpangocairo-1.0-0 libcairo2 pipewire-audio
```

The GPU vendor's Vulkan driver is an additional runtime requirement. On Arch
Linux with an AMD Radeon GPU, that driver is `vulkan-radeon`.

## Usage

Open a video from the command line:

```sh
undefined-player ~/Videos/example.mp4
```

The player starts playing immediately. The focused video window accepts:

- `F` — toggle fullscreen
- `I` — toggle video/color details, FPS / shown / dropped frames, and playback
  position
- `A` — switch to the next audio track
- `Left` / `Right` — seek backward or forward 10 seconds
- `Space` — pause or resume
- `S` — toggle subtitles
- `J` — switch to the next embedded subtitle track
- `Q` — quit

The current file is also exported through MPRIS. GNOME and other desktop media
controls can show its title and send play, pause, stop, and seek commands.

The borderless Wayland window can be dragged from anywhere with the left mouse
button. Drag an edge or corner to resize it; the client-area aspect ratio is
free, while the video itself retains its display aspect ratio with black bars
where needed. Double-click anywhere to toggle fullscreen. Its custom title bar
fades after 1.5 seconds without mouse movement and whenever the window loses
focus. Its close button is at the top-right. The minimum window size is 320x180.
The bottom timeline appears with the controls; click it or drag its handle to
seek. Timeline, keyboard, and MPRIS seeking target the requested position;
FFmpeg resolves its nearest usable preceding keyframe. Decoding continues in
the background so the window remains responsive, and audio and video resume
together once both are ready.

## Scope

The MVP accepts one local media path on the command line. Hardware video decode
currently covers the codecs exposed by both the selected FFmpeg build and the
installed Vulkan driver. Unsupported codecs and profiles fall back to FFmpeg's
software decoder and are uploaded for Vulkan presentation.
HDR metadata is retained from FFmpeg through libplacebo, which supplies the
matching colorspace hint to the Vulkan swapchain for an HDR-enabled compositor.
Sources at 720p and below automatically use libplacebo's GPU EWA Lanczos-sharp
upscaler; larger sources use the normal Lanczos path.

The current platform scope is:

- Wayland only (`SDL_VIDEODRIVER=wayland`)
- PipeWire only (`SDL_AUDIODRIVER=pipewire`)
- a Vulkan-capable GPU
- FFmpeg 8 or newer shared libraries
- system SDL3 and libplacebo
- system Pango/Cairo and Wayland client libraries

The default embedded subtitle track is selected automatically; `J` cycles
through all decodable embedded tracks and briefly shows the selected number.
DVD/PGS bitmap subtitles retain their authored placement. Text and ASS dialogue
use a compact bold monospace Pango font with Unicode shaping and automatic font
fallback; advanced ASS styling is ignored. There is no playlist or audio-stream
selection menu yet; audio tracks are cycled with `A`.

## License

undefined-player is free software licensed under the GNU General Public License
version 3 or later. See [`LICENSE`](LICENSE).
