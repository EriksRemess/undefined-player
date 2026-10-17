# Configuration

[README](README.md) · [Playback](USAGE.md)

On Linux and macOS, preferences are read when a video opens from
`$XDG_CONFIG_HOME/undefined-player/config`, or `~/.config/undefined-player/config`
when the variable is unset or relative.

```ini
# Logical pixels, 0–32. Default: 0 (square).
corner_radius = 12
# Default: en_US. Use auto for the media's default track.
preferred_audio_language = en_US
```

Palette changes create or update the file, preserving comments. Manual edits
apply to the next video opened. Invalid syntax, unknown or duplicate keys, and
out-of-range values cause defaults to be used; palette changes never overwrite
an invalid file.

## Rounded corners

Choose **Rounded corners...** in the palette, then **Off**, **15 pixels**, or
**Enter custom radius...**. Alternatively, type `corners 12` or `corners 0`.
Changes apply immediately and are saved.

The radius follows display scaling. Fullscreen and maximized windows stay
square. On Linux, unsupported transparency produces a warning and square corners.

## Preferred audio language

Choose **Preferred audio language...**, or type `language en_US`, `language eng`,
or `language pt-BR`. Two- and three-letter ISO 639 codes are accepted, including
aliases such as `eng`/`en` and `ger`/`deu`/`de`. Script and region tags are supported;
matching ignores case and treats underscores as hyphens.

Selection prefers an exact tag, then the bare language, then another tag for
that language. Without a match, startup keeps the media’s default track.

Palette changes save the preference and switch the current video’s track when
possible. A missing match or failed seek leaves the current track playing.
`language auto` restores default selection. Cycling tracks with **A** does not
change the preference.
