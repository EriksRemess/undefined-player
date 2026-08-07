# undefined-player

A deliberately machine-specific Wayland video-player MVP. It uses the local
FFmpeg 9 checkout at `/home/eriks/Development/ffmpeg`, decodes directly with
NVIDIA Vulkan Video, renders with libplacebo to a Vulkan Wayland swapchain, and
plays audio through PipeWire.

## Build and run

```sh
cargo build --release
target/release/undefined-player ~/Videos/example.mp4
```

Add `--perf` to print shown and dropped frame rates every two seconds:

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
- `I` — toggle the FPS / shown / dropped frame / playback-position overlay
- `Left` / `Right` — seek backward or forward 10 seconds
- `Space` — pause or resume
- `Q` — quit

The borderless Wayland window can be dragged from anywhere with the left mouse
button. Drag an edge or corner to resize it; the client-area aspect ratio is
free, while the video itself retains its display aspect ratio with black bars
where needed. Double-click anywhere to toggle fullscreen. Its custom title bar
fades after 1.5 seconds without mouse movement and whenever the window loses
focus. Its close button is at the top-right. The minimum window size is 320x180.
Seeking briefly shows the new playback position in the information overlay.

## Scope

The MVP accepts one local media path on the command line. Hardware video decode
currently covers the codecs exposed by both this FFmpeg build and the installed
NVIDIA Vulkan driver: H.264, HEVC, AV1, and VP9. Unsupported codecs or profiles
fail with an explicit error instead of downloading frames for software decode.
HDR metadata is retained from FFmpeg through libplacebo, which supplies the
matching colorspace hint to the Vulkan swapchain for an HDR-enabled compositor.

The build intentionally targets this workstation:

- Wayland only (`SDL_VIDEODRIVER=wayland`)
- PipeWire only (`SDL_AUDIODRIVER=pipewire`)
- NVIDIA RTX A4000
- local FFmpeg 9 shared libraries
- system SDL3 and libplacebo
- system Wayland client/protocol development files
- the `bindgen`, `wayland-scanner`, C compiler, and `ar` commands on `PATH`

There is no seeking, playlist, subtitle, stream-selection, or software-decoding
fallback yet.
