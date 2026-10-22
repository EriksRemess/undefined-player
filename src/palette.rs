//! Command search, argument steps, and palette text layout.
use crate::window::{Action, action_shortcut};
use std::ffi::CString;

pub(crate) struct Palette {
    pub(crate) open: bool,
    pub(crate) query: String,
    pub(crate) selected: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PaletteSeek {
    Relative(f64),
    Absolute(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PaletteCommand {
    Action(Action),
    Seek(PaletteSeek),
    Input(&'static str),
    CornerRadius(u16),
    AudioLanguage(String),
}

pub(crate) struct PaletteRow {
    command: PaletteCommand,
    label: String,
    shortcut: &'static str,
}

impl PaletteRow {
    fn input(prefix: &'static str, label: &str) -> Self {
        Self {
            command: PaletteCommand::Input(prefix),
            label: label.into(),
            shortcut: "ENTER",
        }
    }

    fn seek(argument: PaletteSeek, label: String) -> Self {
        Self {
            command: PaletteCommand::Seek(argument),
            label,
            shortcut: "ENTER",
        }
    }
}

impl Palette {
    pub(crate) fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected: 0,
        }
    }

    fn seek_argument(&self) -> Option<(Action, f64, String)> {
        let mut parts = self.query.split_whitespace();
        let first = parts.next()?.to_ascii_lowercase();
        let direction = if first == "seek" {
            parts.next()?.to_ascii_lowercase()
        } else {
            first
        };
        let forward = match direction.as_str() {
            "forward" | "forwards" | "ahead" => true,
            "back" | "backward" | "backwards" => false,
            _ => return None,
        };
        let amount = parts.next()?.parse::<f64>().ok()?;
        if !amount.is_finite() || amount <= 0.0 {
            return None;
        }
        let unit = parts.next().unwrap_or("seconds").to_ascii_lowercase();
        if parts.next().is_some() {
            return None;
        }
        let multiplier = match unit.as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            _ => return None,
        };
        let seconds = amount * multiplier;
        if !seconds.is_finite() {
            return None;
        }
        let label = format!(
            "Seek {} {amount} {}",
            if forward { "forward" } else { "backward" },
            if multiplier == 60.0 {
                "minutes"
            } else {
                "seconds"
            }
        );
        Some((
            if forward {
                Action::SeekForward
            } else {
                Action::SeekBackward
            },
            seconds,
            label,
        ))
    }

    fn jump_argument(&self) -> Option<(f64, String)> {
        let mut parts = self.query.split_whitespace();
        if !parts.next()?.eq_ignore_ascii_case("jump") {
            return None;
        }
        let token = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        let fields = token.split(':').collect::<Vec<_>>();
        let seconds = match fields.as_slice() {
            [value] => value.parse::<f64>().ok()?,
            [minutes, seconds] => {
                let minutes = minutes.parse::<u64>().ok()?;
                let seconds = seconds.parse::<f64>().ok()?;
                if !(0.0..60.0).contains(&seconds) {
                    return None;
                }
                minutes as f64 * 60.0 + seconds
            }
            [hours, minutes, seconds] => {
                let hours = hours.parse::<u64>().ok()?;
                let minutes = minutes.parse::<u32>().ok()?;
                let seconds = seconds.parse::<f64>().ok()?;
                if minutes >= 60 || !(0.0..60.0).contains(&seconds) {
                    return None;
                }
                hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds
            }
            _ => return None,
        };
        if !seconds.is_finite() || seconds < 0.0 {
            return None;
        }
        Some((seconds, format!("Jump {token}")))
    }

    fn all_rows(audio: bool, subtitles: bool) -> Vec<PaletteRow> {
        let mut rows = vec![
            (Action::TogglePause, "Play / Pause"),
            (Action::PreviousChapter, "Previous chapter"),
            (Action::NextChapter, "Next chapter"),
            (Action::ToggleFullscreen, "Toggle fullscreen"),
            (Action::ToggleInfo, "Toggle media information"),
            (Action::ToggleZoom, "Toggle fit / fill"),
            (Action::ToggleCrop, "Toggle autocrop"),
            (Action::CycleDeinterlace, "Cycle deinterlacing"),
            (Action::ToggleLoop, "Toggle looping"),
            (Action::ToggleMute, "Toggle mute"),
            #[cfg(target_os = "macos")]
            (Action::ToggleUpscale, "Toggle Apple ML upscaling"),
        ]
        .into_iter()
        .map(|(action, label)| PaletteRow {
            shortcut: action_shortcut(&action),
            command: PaletteCommand::Action(action),
            label: label.into(),
        })
        .collect::<Vec<_>>();
        rows.insert(1, PaletteRow::input("seek ", "Seek..."));
        rows.insert(2, PaletteRow::input("jump ", "Jump to time..."));
        rows.push(PaletteRow::input("corners ", "Rounded corners..."));
        rows.push(PaletteRow::input(
            "language ",
            "Preferred audio language...",
        ));
        if audio {
            rows.push(PaletteRow {
                command: PaletteCommand::Action(Action::CycleAudio),
                label: "Next audio track".into(),
                shortcut: "A",
            });
        }
        if subtitles {
            rows.push(PaletteRow {
                command: PaletteCommand::Action(Action::ToggleSubtitles),
                label: "Toggle subtitles".into(),
                shortcut: "S",
            });
            rows.push(PaletteRow {
                command: PaletteCommand::Action(Action::CycleSubtitles),
                label: "Next subtitle track".into(),
                shortcut: "J",
            });
        }
        rows.push(PaletteRow {
            command: PaletteCommand::Action(Action::Quit),
            label: "Quit player".into(),
            shortcut: "Q",
        });
        rows
    }

    fn query_words(&self) -> String {
        self.query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    fn direction_step(&self) -> Option<bool> {
        let query = self.query_words();
        match query.strip_prefix("seek ").unwrap_or(&query) {
            "forward" | "forwards" | "ahead" => Some(true),
            "back" | "backward" | "backwards" => Some(false),
            _ => None,
        }
    }

    pub(crate) fn rows(&self, audio: bool, subtitles: bool) -> Vec<PaletteRow> {
        let query = self.query_words();
        if query == "language" {
            return vec![
                PaletteRow {
                    command: PaletteCommand::AudioLanguage("en-US".into()),
                    label: "English (US)".into(),
                    shortcut: "ENTER",
                },
                PaletteRow {
                    command: PaletteCommand::AudioLanguage("auto".into()),
                    label: "Auto (default track)".into(),
                    shortcut: "ENTER",
                },
                PaletteRow::input("language custom ", "Enter language code..."),
            ];
        }
        if let Some(value) = query.strip_prefix("language ") {
            let value = value.strip_prefix("custom ").unwrap_or(value);
            return crate::language::parse_preference(value)
                .ok()
                .into_iter()
                .map(|language| PaletteRow {
                    label: format!("Prefer audio: {language}"),
                    command: PaletteCommand::AudioLanguage(language),
                    shortcut: "ENTER",
                })
                .collect();
        }
        if query == "corners" || query.starts_with("corners ") {
            let values = if query == "corners" {
                vec![0, 15]
            } else {
                let value = query
                    .strip_prefix("corners ")
                    .expect("corner argument branch has a prefix");
                let value = value.strip_prefix("custom ").unwrap_or(value);
                crate::config::parse_radius(value)
                    .ok()
                    .into_iter()
                    .collect()
            };
            let mut rows: Vec<_> = values
                .into_iter()
                .map(|radius| PaletteRow {
                    command: PaletteCommand::CornerRadius(radius),
                    label: if radius == 0 {
                        "Off (square corners)".into()
                    } else {
                        format!("{radius} pixels")
                    },
                    shortcut: "ENTER",
                })
                .collect();
            if query == "corners" {
                rows.push(PaletteRow::input(
                    "corners custom ",
                    "Enter custom radius...",
                ));
            }
            return rows;
        }
        // Parsed commands are matches without applying the label search again.
        if let Some((action, amount, label)) = self.seek_argument() {
            let offset = if action == Action::SeekForward {
                amount
            } else {
                -amount
            };
            let amount_label = label
                .splitn(3, ' ')
                .nth(2)
                .expect("seek label contains command, direction, and amount")
                .to_owned();
            return vec![PaletteRow::seek(
                PaletteSeek::Relative(offset),
                amount_label,
            )];
        }
        if let Some((position, label)) = self.jump_argument() {
            let time = label
                .strip_prefix("Jump ")
                .expect("jump parser supplies the label prefix");
            let label = if time.contains(':') {
                time.into()
            } else {
                format!("{time} seconds")
            };
            return vec![PaletteRow::seek(PaletteSeek::Absolute(position), label)];
        }
        if let Some(forward) = self.direction_step() {
            return [10, 30, 60, 120]
                .into_iter()
                .map(|seconds| {
                    PaletteRow::seek(
                        PaletteSeek::Relative(if forward {
                            seconds as f64
                        } else {
                            -(seconds as f64)
                        }),
                        format!("{seconds} seconds"),
                    )
                })
                .collect();
        }
        let query = self.query_words();
        if query == "jump" {
            return Vec::new();
        }
        if query.starts_with("seek") {
            return [("seek forward ", "Forward"), ("seek backward ", "Backward")]
                .into_iter()
                .filter(|(prefix, _)| prefix.trim().contains(&query))
                .map(|(prefix, label)| PaletteRow::input(prefix, label))
                .collect();
        }
        Self::all_rows(audio, subtitles)
            .into_iter()
            .filter(|row| row.label.to_lowercase().contains(&query))
            .collect()
    }

    fn selected_action(&self, audio: bool, subtitles: bool) -> Option<PaletteCommand> {
        let rows = self.rows(audio, subtitles);
        rows.get(self.selected.min(rows.len().saturating_sub(1)))
            .map(|row| row.command.clone())
    }

    pub(crate) fn activate(&mut self, audio: bool, subtitles: bool) -> Option<PaletteCommand> {
        match self.selected_action(audio, subtitles)? {
            PaletteCommand::Input(prefix) => {
                self.query = prefix.into();
                self.selected = 0;
                None
            }
            command => Some(command),
        }
    }

    pub(crate) fn display(&mut self, audio: bool, subtitles: bool) -> CString {
        let rows = self.rows(audio, subtitles);
        let stable_width = Self::all_rows(audio, subtitles)
            .iter()
            .map(|row| row.label.chars().count() + row.shortcut.len() + 6)
            .max()
            .unwrap_or(0);
        let query = self.query_words();
        let command = query.split_whitespace().next().unwrap_or("");
        let hint = if command == "language" {
            Some("LANGUAGE CODE: en_US, eng, pt-BR, OR auto")
        } else if query == "corners" {
            Some("CHOOSE OFF, 15 PX, OR A CUSTOM RADIUS")
        } else if command == "corners" {
            Some("ENTER A WHOLE NUMBER FROM 0 TO 32")
        } else if command == "jump" && self.jump_argument().is_none() {
            Some("ENTER TIME: jump 123 OR jump 2:03")
        } else if self.direction_step().is_some() {
            Some("CHOOSE SECONDS OR TYPE AN AMOUNT")
        } else if command == "seek" && self.seek_argument().is_none() {
            Some("CHOOSE FORWARD OR BACKWARD")
        } else {
            None
        };
        let mut text = format!("COMMAND PALETTE  > {}\n", self.query);
        if let Some(hint) = hint {
            text.push_str(hint);
            text.push('\n');
        }
        if rows.is_empty() {
            if hint.is_none() {
                text.push_str("NO MATCHES");
            }
            return CString::new(text).expect("palette text contains no NUL bytes");
        }
        self.selected = self.selected.min(rows.len() - 1);
        let start = self
            .selected
            .saturating_sub(2)
            .min(rows.len().saturating_sub(5));
        for (index, row) in rows.iter().enumerate().skip(start).take(5) {
            text.push_str(if index == self.selected { "> " } else { "  " });
            text.push_str(&row.label);
            text.push_str("  [");
            text.push_str(row.shortcut);
            text.push(']');
            let row_width = row.label.chars().count() + row.shortcut.len() + 6;
            text.push_str(&" ".repeat(stable_width.saturating_sub(row_width)));
            text.push('\n');
        }
        CString::new(text).expect("palette text contains no NUL bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_commands_keep_their_arguments_and_ignore_display_normalization() {
        for (query, expected) in [
            ("seek forward 1", PaletteSeek::Relative(1.0)),
            ("seek forward 30 secs", PaletteSeek::Relative(30.0)),
            ("seek backward 2 mins", PaletteSeek::Relative(-120.0)),
            ("seek backwards 2 mins", PaletteSeek::Relative(-120.0)),
            ("seek ahead 30", PaletteSeek::Relative(30.0)),
            ("back 5", PaletteSeek::Relative(-5.0)),
            ("seek forward 10.0", PaletteSeek::Relative(10.0)),
            ("jump  123", PaletteSeek::Absolute(123.0)),
            ("  JUMP 2:30  ", PaletteSeek::Absolute(150.0)),
            ("jump 1:02:03.5", PaletteSeek::Absolute(3723.5)),
        ] {
            let mut palette = Palette::new();
            palette.query = query.into();
            assert_eq!(palette.rows(true, true).len(), 1, "{query}");
            assert_eq!(
                palette.selected_action(true, true),
                Some(PaletteCommand::Seek(expected)),
                "{query}"
            );
            assert!(
                !palette
                    .display(true, true)
                    .to_string_lossy()
                    .contains("NO MATCHES"),
                "{query}"
            );
        }
    }

    #[test]
    fn malformed_times_and_amounts_cannot_execute() {
        for query in [
            "jump",
            "jump 1:-30",
            "jump 1:60",
            "jump 1:60:00",
            "jump -1",
            "jump NaN",
            "jump inf",
            "jump 1:NaN",
            "seek forward 0",
            "seek back -10",
            "seek forward NaN",
            "seek forward 10 bananas",
        ] {
            let mut palette = Palette::new();
            palette.query = query.into();
            assert!(palette.selected_action(true, true).is_none(), "{query}");
        }
    }

    #[test]
    fn argument_commands_advance_through_steps_before_execution() {
        let mut palette = Palette::new();
        palette.query = "seek".into();
        assert_eq!(
            palette
                .rows(true, true)
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["Forward", "Backward"]
        );
        palette.selected = 1;
        assert_eq!(palette.activate(true, true), None);
        assert_eq!(palette.query, "seek backward ");
        assert_eq!(
            palette
                .rows(true, true)
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["10 seconds", "30 seconds", "60 seconds", "120 seconds"]
        );
        palette.selected = 1;
        assert_eq!(
            palette.activate(true, true),
            Some(PaletteCommand::Seek(PaletteSeek::Relative(-30.0)))
        );
        palette.query = "seek forward 2.5".into();
        palette.selected = 0;
        assert_eq!(
            palette.activate(true, true),
            Some(PaletteCommand::Seek(PaletteSeek::Relative(2.5)))
        );

        palette.query = "jum".into();
        assert_eq!(palette.activate(true, true), None);
        assert_eq!(palette.query, "jump ");
        assert_eq!(palette.activate(true, true), None);
        palette.query.push_str("2:30");
        assert_eq!(
            palette.activate(true, true),
            Some(PaletteCommand::Seek(PaletteSeek::Absolute(150.0)))
        );
    }

    #[test]
    fn navigating_keeps_width_and_selection_consistent() {
        let mut palette = Palette::new();
        let rows = palette.rows(true, true);
        let mut widths = Vec::new();
        for selected in 0..rows.len() {
            palette.selected = selected;
            widths.push(
                palette
                    .display(true, true)
                    .to_string_lossy()
                    .lines()
                    .map(|line| line.chars().count())
                    .max()
                    .unwrap(),
            );
        }
        assert!(widths.iter().all(|width| *width == widths[0]));
        palette.selected = rows.len();
        let before = palette.selected_action(true, true);
        palette.display(true, true);
        assert_eq!(before, palette.selected_action(true, true));
    }
}
