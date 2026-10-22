# Playback

[README](README.md) · [Configuration](CONFIGURATION.md)

## Opening media

On macOS, open the app or use Finder’s **Open With** menu. **Command+O** opens
another video, replacing the current one.

From the command line:

```sh
undefined-player ~/Videos/example.mp4
undefined-player 'https://example.com/video.mp4'
undefined-player 'https://www.youtube.com/watch?v=dQw4w9WgXcQ'
undefined-player yt:dQw4w9WgXcQ
```

The macOS app’s executable is
`/Applications/Undefined Player.app/Contents/MacOS/undefined-player`; installing
the app does not add `undefined-player` to `PATH`.

YouTube requires `yt-dlp` on `PATH`. Reopen the original link if its stream URL
expires. HLS playback prefers the highest available decodable resolution, frame
rate, then bitrate; quality stays fixed during playback. HTTP servers need
byte-range support for reliable seeking.

Add `--debug` for decoder/platform diagnostics or `--perf` for playback statistics.

## Keyboard shortcuts

| Key | Action |
| --- | --- |
| Space | Pause/resume |
| F | Fullscreen |
| Left / Right | Seek −10 / +10 seconds |
| , / . | Previous/next chapter |
| A | Next audio track |
| M | Mute |
| S | Show/hide subtitles |
| J | Next subtitle track |
| C | Automatic crop |
| Z | Zoom to fill |
| D | Deinterlacing: Auto, On, Off |
| U | Apple ML upscaling: Auto/Off (macOS) |
| I | Media details and processing statistics |
| L | Loop |
| Q | Quit |
| Ctrl+Shift+P | Command palette |

On macOS, **Command+Shift+P** also opens the palette. Media keys work while other
apps have focus; previous/next navigate chapters or seek ten seconds when no
chapter is available. Linux desktop media controls use MPRIS.

## Command palette

Type to filter, use Up/Down to select, Enter to run or advance, and Escape to close.
Seek commands accept amounts or timestamps:

```text
seek forward 45
seek backward 2 minutes
jump 2:03
jump 1:02:03
```

The palette also contains [saved preferences](CONFIGURATION.md).

## Window and timeline

Drag the video area to move the window; drag an edge or corner to resize it.
Double-click the video area for fullscreen. Click or drag the bottom timeline
to seek. Hover a chapter marker to see its title.

Controls fade after 1.5 seconds without mouse movement or when the window loses
focus. The cursor hides after 1.5 seconds of inactivity in fullscreen.
On supported Linux desktops, right-click opens the compositor’s window menu.

## Resume

Local videos and non-live YouTube videos with a known duration remember their
position. At the resume prompt, **Y** resumes and **N** starts over. Quitting
before answering preserves the saved position; reaching the end removes it.
Changed local files do not reuse old positions. YouTube positions follow the
video ID across different link formats.

Positions are stored in `$XDG_STATE_HOME/undefined-player/resume`, falling back
to `~/.local/state/undefined-player/resume` when the variable is unset or relative.
These paths apply on both Linux and macOS.

## Crop and zoom

**C** detects stable black borders. Dark scenes delay detection; while paused,
the current frame is scanned. Bars needed to preserve the video’s aspect ratio
can remain.

**Z** fills the window without stretching, enabling autocrop and trimming the
remaining picture. Press it again to restore the previous crop setting.
The **I** panel shows `FIT`, `FILL`, or `FILL (DETECTING)`.

## Deinterlacing

**Auto** combines picture analysis with interlacing flags. **On** assumes
top-field-first for unmarked video; **Off** disables filtering. Changes apply
while paused, and output retains the source frame rate.

Linux uses GPU YADIF when adjacent frames are available, otherwise bob filtering.
macOS uses spatial bob: GPU filtering for hardware/HDR frames and CPU filtering
for software SDR frames.

## Scaling and HDR

Small square-pixel videos up to 640×480 (480×640 in portrait) use integer scaling
when they fit. Zoom-to-fill bypasses this policy.

On Linux, other sources up to 1080p use libplacebo’s Lanczos-sharp upscaler.
HDR output requires an HDR-enabled compositor. On macOS, HDR10 and HLG use the
display’s available HDR brightness, with tone mapping for SDR displays.

On macOS 26+, Apple ML can enlarge supported progressive, 8-bit SDR video by 2×
while preserving its display aspect ratio. **U** toggles it; **I** shows its status.
Initialization runs in the background. HDR, interlaced or unsupported formats,
sources above 1920×1080, integer-scaled video, and shrinking windows bypass it.
Some dimensions within that limit are also unsupported by Apple’s runtime.

## Subtitles

Subtitles start hidden. **S** shows the preselected embedded track; **J** cycles
tracks. DVD/PGS bitmap subtitles retain their placement. Text and ASS dialogue
use a bold monospace font with Unicode support; advanced ASS styling is ignored.
