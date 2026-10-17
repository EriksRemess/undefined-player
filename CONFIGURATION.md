# Configuration

[README](README.md) · [Playback guide](USAGE.md)

Preferences are read at startup from `$XDG_CONFIG_HOME/undefined-player/config`,
or `~/.config/undefined-player/config` when XDG_CONFIG_HOME is unset or relative.
The file uses plain `key = value` lines and `#` comments:

```ini
# Window corner radius in logical pixels, from 0 to 32. Default: 0 (square).
corner_radius = 12
# Preferred audio language. Default: en_US. Use auto for the normal default track.
preferred_audio_language = en_US
```

The first palette setting change creates the config file.

The entire config is validated before applying it. Invalid syntax, unknown or
duplicate settings, and out-of-range values produce a warning with the file and
line number and use defaults. Invalid files are never overwritten by palette
changes: fix the file first. Successful saves preserve comments and replace the
file atomically. Manual edits take effect on the next launch.

## Rounded corners

Open the command palette and choose **Rounded corners...**, then **Off**, **15
pixels**, or **Enter custom radius...**. You can also type `corners 12` directly.
Changes apply immediately and are saved automatically. `corners 0` turns rounding
off. The radius scales with display scaling; fullscreen and maximized windows
remain square.
Transparent corners let clicks reach the windows beneath. Controls and their
clickable areas are inset together to stay inside the rounded outline.
If the Vulkan surface cannot composite transparent corners, the player warns
and keeps square corners. On macOS, SDL's Metal window shape applies the same
alpha mask to the rendered window and its mouse input area.

## Preferred audio language

Choose **Preferred audio language...** in the palette, or type `language en_US`,
`language eng`, or `language pt-BR`. The preference applies to local files,
YouTube, and other streams using their audio-track language metadata. Two- and
three-letter ISO 639 codes are recognized, including aliases such as `eng`/`en`
and `ger`/`deu`/`de`. Tags can include a script and region (`zh-Hant-TW`);
underscores and hyphens are interchangeable and matching ignores case.

An exact tag match wins, followed by the bare language, then another tag for
the same language. If no track matches or language metadata is missing, startup
keeps the normal default track. The language code identifies the spoken language;
it does not guarantee that a track is the original recording.
Palette changes save immediately and switch to a matching track in the current
video; without a match, the current track stays selected. If the source rejects
the seek needed to switch tracks, playback continues with the current track and
the preference remains saved for future videos. `language auto` restores
the normal default selection. Cycling tracks with `A` does not change the saved
preference.
