# Development

This document covers building, testing, diagnostics, packaging, and custom
dependency setups for undefined-player. User installation and playback are
documented in [README.md](README.md).

## Build requirements

- Rust 1.85 or newer and Cargo (the crate uses Rust 2024 edition)
- a C compiler, `ar`, and `pkg-config`
- `wayland-scanner` and the stable Wayland protocol definitions
- development files for SDL3, libplacebo, Vulkan, Wayland, Pango, Cairo, and
  GIO
- FFmpeg 8 or newer development files for `libavformat`, `libavcodec`,
  `libswresample`, and `libavutil`; Vulkan Video support is needed for hardware
  decoding

On Ubuntu 26.04, install the packaged build dependencies with:

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

`make check` runs the Rust tests, strict Clippy checks, and desktop-file
validation.

## Local installation

Build and install the player and its video-file associations into `~/.local`:

```sh
make install
```

Use `make uninstall` to remove those installed files. Override `PREFIX` and
optionally `DESTDIR` for another installation root.

## Debian package

The package currently targets Ubuntu 26.04 and Debian testing/unstable. Debian
stable ships an older FFmpeg release than the player supports.

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
builds and checks on Arch Linux, builds and lints the Debian package on Ubuntu
26.04, creates the binary tarball, and uploads both package formats.
