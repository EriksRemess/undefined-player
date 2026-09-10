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
- `C` — toggle automatic cropping of encoded black borders
- `Z` — toggle zoom to fill the window, using autocrop first
- `D` — cycle deinterlacing: Auto (default), On, Off
- `I` — toggle video/color details, deinterlacing status, cropped resolution,
  FPS / shown / dropped frames, and playback
  position
- `A` — switch to the next audio track
- `Left` / `Right` — seek backward or forward 10 seconds
- `,` / `.` — jump to the previous or next chapter
- `Space` — pause or resume
- `S` — toggle subtitles
- `J` — switch to the next embedded subtitle track
- `Q` — quit

For files with chapters, dots on the timeline mark each chapter's start.
Chapter navigation keeps the current pause state and stops at the first or last
chapter; files without chapters leave these keys inactive.
After a chapter jump, its number and title appear in the top bar for three
seconds. Hovering a chapter dot shows its title there too. The `I` overlay lists
the current chapter at the top right, alongside embedded title and artist tags
when available. Untitled chapters show only their number.

The current file is also exported through MPRIS. GNOME and other desktop media
controls show its embedded title, artist, and cover image when available, falling
back to the filename for untitled media. They support play, pause, stop, and seek;
previous/next buttons navigate chapters and disable at their respective ends.

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
together once buffered data is ready. Audio tracks that begin later stay silent
until their starting timestamp.

Autocrop starts disabled. Press `C` to detect stable black borders in the video;
press it again to restore the complete frame. Detection samples frames in a
background worker and preserves the video's pixel aspect ratio. It ignores
black frames and very dark scenes, so allow a few visible frames for detection.
When paused, it scans the current frame. Borders required by the window's own
aspect ratio can remain after encoded bars are removed.

Zoom fills the current window (or the screen in fullscreen) without stretching.
It first enables autocrop, then trims the center of the remaining picture to the
window's aspect ratio. Wider pictures lose content at the sides; narrower pictures
lose content at the top and bottom. Integer scaling is bypassed while filling.
Press `Z` again to restore normal fitting and your previous autocrop setting.
`C` can override cropping during zoom; leaving zoom still restores the earlier
setting. The `I` panel shows `ZOOM: FILL`, `FIT`, or `FILL (DETECTING)` while
waiting for a reliable border sample. Dark scenes retain the last detected crop;
if border sampling is unsupported, zoom uses the full frame. Both controls work
while paused.

Deinterlacing starts in Auto mode and follows each decoded frame's interlacing
flag and field order. Press `D` to force it On for incorrectly flagged files,
again to turn it Off, and again to return to Auto. On assumes top-field-first
when the frame has no interlacing flag. Changes also apply while paused.
Filtering runs on the GPU at the source frame rate (not doubled field rate).
Auto uses decoder metadata; it does not scan the picture for combing.

## Scope

The player accepts one local media path on the command line. Hardware video
decode covers the codecs and profiles exposed by both the selected FFmpeg build
and the installed Vulkan driver. If Vulkan decoder initialization or initial
decoding fails, playback restarts with FFmpeg's software decoder and uploads
frames for Vulkan presentation.
HDR metadata is retained from FFmpeg through libplacebo, which supplies the
matching colorspace hint to the Vulkan swapchain for an HDR-enabled compositor.
Small square-pixel videos (up to 640×480, or 480×640 in portrait) use the largest
integer scale that fits, with nearest-neighbour filtering and centered black
borders. If the video needs shrinking, normal aspect-preserving scaling applies.
Other sources at 720p and below use libplacebo's GPU EWA Lanczos-sharp upscaler;
larger sources use the normal Lanczos path.

The current platform scope is:

- Wayland only (`SDL_VIDEODRIVER=wayland`)
- PipeWire only (`SDL_AUDIODRIVER=pipewire`)
- a Vulkan-capable GPU
- FFmpeg 8 or newer shared libraries
- system SDL3 and libplacebo
- system Pango/Cairo and Wayland client libraries

The default embedded subtitle track is preselected, but subtitles start hidden;
`S` shows them and `J` cycles through all decodable embedded tracks while
briefly showing the selected number.
DVD/PGS bitmap subtitles retain their authored placement. Text and ASS dialogue
use a compact bold monospace Pango font with Unicode shaping and automatic font
fallback; advanced ASS styling is ignored. There is no playlist or audio-stream
selection menu yet; audio tracks are cycled with `A`.

## License

undefined-player is free software licensed under the GNU General Public License
version 3 or later. See [`LICENSE`](LICENSE).
