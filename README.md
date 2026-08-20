# undefined-player

A deliberately machine-specific Wayland video-player MVP. It uses the local
FFmpeg 9 checkout at `/home/eriks/Development/ffmpeg`, decodes supported codecs
directly with NVIDIA Vulkan Video, renders with libplacebo to a Vulkan Wayland
swapchain, and plays audio through PipeWire.

## Build and run

```sh
cargo build --release
target/release/undefined-player ~/Videos/example.mp4
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
