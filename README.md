# undefined-player

A Wayland video-player MVP. It uses FFmpeg 8 or newer, decodes supported codecs directly
with Vulkan Video when available, renders with libplacebo to a Vulkan Wayland
swapchain, and plays audio through PipeWire.

## Requirements

Building requires:

- Rust 1.85 or newer and Cargo (the crate uses Rust 2024 edition)
- a C compiler, `ar`, and `pkg-config`
- `wayland-scanner` and the stable Wayland protocol definitions
- development files for SDL3, libplacebo, Vulkan, Wayland, Pango, Cairo, and
  GIO
- FFmpeg 8 or newer development files for `libavformat`, `libavcodec`, `libswresample`,
  and `libavutil`; Vulkan Video support is needed for hardware decoding

On Ubuntu 26.04, the packaged build dependencies can be installed with:

```sh
sudo apt install build-essential binutils pkg-config \
  wayland-protocols libwayland-dev libvulkan-dev libsdl3-dev libplacebo-dev \
  libpango1.0-dev libcairo2-dev libglib2.0-dev libavformat-dev \
  libavcodec-dev libswresample-dev libavutil-dev
```

On Arch Linux, install the build dependencies and AMD Vulkan driver with:

```sh
sudo pacman -S --needed base-devel rust pkgconf wayland wayland-protocols \
  vulkan-headers sdl3 libplacebo pango cairo glib2 ffmpeg vulkan-radeon
```

Running requires a Wayland session, the Vulkan loader and a working Vulkan
driver, SDL3 with its Wayland and PipeWire backends, libplacebo, Wayland client,
Pango/Cairo, GIO, and the corresponding FFmpeg shared libraries. Vulkan Video
support enables hardware decoding; unsupported codecs fall back to software
decoding but still use Vulkan for presentation.

When the player is built against Ubuntu 26.04's packaged libraries, the base
runtime packages can be installed with:

```sh
sudo apt install libvulkan1 libsdl3-0 libplacebo360 libwayland-client0 \
  libpangocairo-1.0-0 libcairo2 pipewire-audio
```

The GPU vendor's Vulkan driver is an additional runtime requirement.
On Arch Linux with an AMD Radeon GPU, that driver is `vulkan-radeon`.

## Build and run

```sh
cargo build --release
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
cargo build --release
```

Vulkan device selection is automatic. On a multi-GPU system,
`UP_VULKAN_DEVICE` can pass an explicit device selector to FFmpeg.

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

## Debian package

The package currently targets Ubuntu 26.04 and Debian testing/unstable. Debian
stable ships an older FFmpeg release than the player supports.

Install the build dependencies listed in `debian/control`, then build an
unsigned binary package with:

```sh
sudo apt build-dep .
make deb
sudo apt install ../undefined-player_0.1.2-1_amd64.deb
```

Installing through `apt` automatically resolves the shared-library runtime
dependencies recorded in the package, and normally installs the recommended
PipeWire audio setup. A suitable Vulkan driver remains hardware-specific and
must be installed for the user's GPU.

Debian's package manager cannot treat libraries installed directly from source
as satisfying package dependencies. Debian packages should therefore be built
against distribution-provided development packages. Users intentionally using
custom FFmpeg or other libraries should build from source with `cargo build`
and use `FFMPEG_DIR` or `PKG_CONFIG_PATH` as described above.

## Binary tarball

Build a versioned binary tarball with:

```sh
make tarball
```

The archive is written under `target/dist/` and includes a user-local
`install.sh`. Unlike the Debian package, the tarball cannot install or validate
runtime dependencies. It is intended for compatible Linux systems where the
required shared libraries—including custom source installations—are already
managed by the user.

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
- system Pango/Cairo development files
- system Wayland client/protocol development files
- the `wayland-scanner`, C compiler, and `ar` commands on `PATH`

The default embedded subtitle track is selected automatically; `J` cycles
through all decodable embedded tracks and briefly shows the selected number.
DVD/PGS bitmap subtitles retain their authored placement. Text and ASS dialogue
use a compact bold monospace Pango font with Unicode shaping and automatic font
fallback; advanced ASS styling is ignored. There is no playlist or audio-stream
selection menu yet; audio tracks are cycled with `A`.

## License

undefined-player is free software licensed under the GNU General Public License
version 3 or later. See [`LICENSE`](LICENSE).

Release packages are built against redistributable distribution-provided
libraries. Do not redistribute builds linked against an FFmpeg configuration
created with `--enable-nonfree`.
