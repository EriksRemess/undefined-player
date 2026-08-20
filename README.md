# undefined-player

A deliberately machine-specific Wayland video-player MVP. It uses a local
FFmpeg 9 checkout, decodes supported codecs directly with NVIDIA Vulkan Video,
renders with libplacebo to a Vulkan Wayland swapchain, and plays audio through
PipeWire.

## Requirements

Building requires:

- Rust 1.85 or newer and Cargo (the crate uses Rust 2024 edition)
- a C compiler, `ar`, `pkg-config`, `bindgen`, and libclang
- `wayland-scanner` and the stable Wayland protocol definitions
- development files for SDL3, libplacebo, Vulkan, Wayland, Pango, and Cairo
- an FFmpeg 9 source checkout built with shared `libavformat`, `libavcodec`,
  `libswresample`, and `libavutil` libraries; Vulkan Video support is needed for
  hardware decoding

On Ubuntu 26.04, the packaged build dependencies can be installed with:

```sh
sudo apt install build-essential binutils clang libclang-dev pkg-config \
  wayland-protocols libwayland-dev libvulkan-dev libsdl3-dev libplacebo-dev \
  libpango1.0-dev libcairo2-dev
cargo install bindgen-cli
```

FFmpeg itself and any libraries enabled in that FFmpeg build must be provided
separately.

Running requires a Wayland session, the Vulkan loader and a working Vulkan
driver, SDL3 with its Wayland and PipeWire backends, libplacebo, Wayland client,
Pango/Cairo, and the shared libraries from the selected FFmpeg checkout. The
linker records the selected FFmpeg and non-system library directories as
runtime search paths. NVIDIA Vulkan Video support enables hardware decoding;
unsupported codecs fall back to software decoding but still use Vulkan for
presentation.

When the player is built against Ubuntu 26.04's packaged libraries, the base
runtime packages can be installed with:

```sh
sudo apt install libvulkan1 libsdl3-0 libplacebo360 libwayland-client0 \
  libpangocairo-1.0-0 libcairo2 pipewire-audio
```

The NVIDIA driver and the selected FFmpeg build's shared-library dependencies
are additional runtime requirements.

## Build and run

```sh
cargo build --release
target/release/undefined-player ~/Videos/example.mp4
```

The FFmpeg checkout defaults to `../ffmpeg`, relative to this repository. Set
`FFMPEG_DIR` to use a different checkout. Other headers and libraries are
located through `pkg-config`, so `PKG_CONFIG_PATH` can select non-system
installations. `WAYLAND_PROTOCOLS_DIR` can override the Wayland protocol data
directory when needed:

```sh
FFMPEG_DIR=/path/to/ffmpeg \
WAYLAND_PROTOCOLS_DIR=/path/to/wayland-protocols \
cargo build --release
```

Add `--perf` to print shown/dropped frame rates and average decoder-fill and
display times every two seconds:

```sh
target/release/undefined-player --perf ~/Videos/example.mp4
```

To build and install the player and its video-file associations into
`~/.local`:

```sh
make install
```

Use `make uninstall` to remove those installed files.

The player starts playing immediately. The focused video window accepts:

- `F` — toggle fullscreen
- `I` — toggle video/color details, FPS / shown / dropped frames, and playback
  position
- `Left` / `Right` — seek backward or forward 10 seconds
- `Space` — pause or resume
- `S` — toggle subtitles
- `J` — switch to the next embedded subtitle track
- `Q` — quit

The borderless Wayland window can be dragged from anywhere with the left mouse
button. Drag an edge or corner to resize it; the client-area aspect ratio is
free, while the video itself retains its display aspect ratio with black bars
where needed. Double-click anywhere to toggle fullscreen. Its custom title bar
fades after 1.5 seconds without mouse movement and whenever the window loses
focus. Its close button is at the top-right. The minimum window size is 320x180.
The bottom timeline appears with the controls; click it or drag its handle to
seek. Seeking uses the closest indexed keyframe for a quick response and briefly
shows the resulting playback position above the timeline.

## Scope

The MVP accepts one local media path on the command line. Hardware video decode
currently covers the codecs exposed by both this FFmpeg build and the installed
NVIDIA Vulkan driver: H.264, HEVC, AV1, and VP9. Other codecs, including MPEG-2,
fall back to FFmpeg's software decoder and are uploaded for Vulkan presentation.
HDR metadata is retained from FFmpeg through libplacebo, which supplies the
matching colorspace hint to the Vulkan swapchain for an HDR-enabled compositor.
Sources at 720p and below automatically use libplacebo's GPU EWA Lanczos-sharp
upscaler; larger sources use the normal Lanczos path.

The build intentionally targets this workstation:

- Wayland only (`SDL_VIDEODRIVER=wayland`)
- PipeWire only (`SDL_AUDIODRIVER=pipewire`)
- NVIDIA RTX A4000
- local FFmpeg 9 shared libraries
- system SDL3 and libplacebo
- system Pango/Cairo development files
- system Wayland client/protocol development files
- the `bindgen`, `wayland-scanner`, C compiler, and `ar` commands on `PATH`

The default embedded subtitle track is selected automatically; `J` cycles
through all decodable embedded tracks and briefly shows the selected number.
DVD/PGS bitmap subtitles retain their authored placement. Text and ASS dialogue
use a compact bold monospace Pango font with Unicode shaping and automatic font
fallback; advanced ASS styling is ignored. There is no playlist or audio-stream
selection support yet.
