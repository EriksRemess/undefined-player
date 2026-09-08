pub(crate) enum SubtitleContent {
    Clear,
    Text(String),
    Bitmap {
        width: i32,
        height: i32,
        pixels: Vec<u8>,
    },
}

pub(crate) struct SubtitleCue {
    pub(crate) track: usize,
    pub(crate) start: f64,
    pub(crate) end: f64,
    pub(crate) serial: u64,
    pub(crate) content: SubtitleContent,
}

pub(crate) fn subtitle_dialogue_text(raw: &str, ass: bool) -> String {
    let dialogue = if ass {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("Dialogue:") {
            trimmed.splitn(10, ',').nth(9).unwrap_or(trimmed)
        } else {
            trimmed.splitn(9, ',').nth(8).unwrap_or(trimmed)
        }
    } else {
        raw
    };
    let mut text = String::with_capacity(dialogue.len());
    let mut in_override = false;
    let mut characters = dialogue.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '{' => in_override = true,
            '}' if in_override => in_override = false,
            '\\' if !in_override => match characters.next() {
                Some('N' | 'n') => text.push('\n'),
                Some('h') => text.push(' '),
                Some(other) => {
                    text.push('\\');
                    text.push(other);
                }
                None => text.push('\\'),
            },
            '\0' => {}
            '’' | '‘' => text.push('\''),
            '“' | '”' => text.push('"'),
            '–' | '—' => text.push('-'),
            _ if !in_override && character.is_whitespace() => text.push(' '),
            _ if !in_override && !character.is_control() => text.push(character),
            _ => {}
        }
    }
    text.trim().to_owned()
}
