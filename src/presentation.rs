use crate::metadata::TrackLabel;
use std::ffi::CString;
use std::time::{Duration, Instant};

pub(crate) fn format_position(seconds: f64) -> String {
    let total = seconds.max(0.0).floor() as u64;
    let hours = total / 3600;
    let minutes = total / 60 % 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

pub(crate) fn timeline_text(position: f64, duration: Option<f64>) -> CString {
    let position = format_position(position);
    let text = duration.map_or(position.clone(), |duration| {
        format!("{position} / {}", format_position(duration))
    });
    CString::new(text).expect("position text has no NUL bytes")
}

pub(crate) struct PositionNotice {
    pub(crate) text: Option<CString>,
    shown_at: Instant,
    pub(crate) alpha: f32,
}

impl PositionNotice {
    pub(crate) fn new() -> Self {
        Self {
            text: None,
            shown_at: Instant::now(),
            alpha: 0.0,
        }
    }

    pub(crate) fn show(&mut self, position: f64, duration: Option<f64>) {
        self.show_text(timeline_text(position, duration));
    }

    pub(crate) fn show_text(&mut self, text: CString) {
        self.text = Some(text);
        self.shown_at = Instant::now();
        self.alpha = 1.0;
    }

    pub(crate) fn update(&mut self) -> bool {
        if self.text.is_none() {
            return false;
        }
        let elapsed = self.shown_at.elapsed().as_secs_f32();
        let next = if elapsed < 1.0 {
            1.0
        } else {
            (1.0 - (elapsed - 1.0) / 0.4).max(0.0)
        };
        let changed = (next - self.alpha).abs() > f32::EPSILON;
        self.alpha = next;
        if self.alpha == 0.0 {
            self.text = None;
            return true;
        }
        changed
    }
}

pub(crate) fn next_track(current: usize, count: usize) -> usize {
    if count == 0 { 0 } else { (current + 1) % count }
}

pub(crate) fn track_status_text(
    kind: &str,
    selected: usize,
    count: usize,
    label: &TrackLabel,
) -> CString {
    let mut status = format!("{kind}: {} / {count}", selected + 1);
    if let Some(language) = label.language.as_deref() {
        status.push_str(" - ");
        status.push_str(&language.to_uppercase());
        if let Some(title) = label.title.as_deref() {
            status.push_str(" - ");
            status.push_str(&title.to_uppercase());
        }
        status.push_str(" - ");
        status.push_str(&label.codec);
    }
    CString::new(status).expect("track status has no NUL bytes")
}

pub(crate) fn subtitle_status_text(
    visible: bool,
    selected: usize,
    count: usize,
    label: &TrackLabel,
) -> CString {
    if visible {
        track_status_text("SUBTITLES", selected, count, label)
    } else {
        CString::new("SUBTITLES: OFF").expect("subtitle status has no NUL bytes")
    }
}

pub(crate) struct TopBar {
    focused: bool,
    last_motion: Instant,
    last_update: Instant,
    pub(crate) alpha: f32,
}

#[derive(Default)]
pub(crate) struct ChapterHeading {
    pub(crate) hovered: Option<usize>,
    jump: Option<(usize, Instant)>,
}

impl ChapterHeading {
    pub(crate) fn show(&mut self, index: usize, now: Instant) {
        self.hovered = None;
        self.jump = Some((index, now));
    }

    pub(crate) fn update(&mut self, now: Instant) -> bool {
        if self
            .jump
            .is_some_and(|(_, shown)| now.duration_since(shown) >= Duration::from_secs(3))
        {
            self.jump = None;
            return true;
        }
        false
    }

    pub(crate) fn index(&self) -> Option<usize> {
        self.hovered.or(self.jump.map(|(index, _)| index))
    }
}

impl TopBar {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        Self {
            focused: true,
            last_motion: now,
            last_update: now,
            alpha: 1.0,
        }
    }

    pub(crate) fn mouse_activity(&mut self) {
        self.last_motion = Instant::now();
    }

    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            self.mouse_activity();
        }
    }

    pub(crate) fn update(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now - self.last_update;
        self.last_update = now;
        let target = if self.focused && now - self.last_motion < Duration::from_millis(1500) {
            1.0
        } else {
            0.0
        };
        let previous = self.alpha;
        let step = elapsed.as_secs_f32() / 0.25;
        if self.alpha < target {
            self.alpha = (self.alpha + step).min(target);
        } else if self.alpha > target {
            self.alpha = (self.alpha - step).max(target);
        }
        (self.alpha - previous).abs() > f32::EPSILON
    }
}

pub(crate) struct PresentationStats {
    shown: u64,
    dropped: u64,
}

impl PresentationStats {
    pub(crate) fn counts(&self) -> (u64, u64) {
        (self.shown, self.dropped)
    }

    pub(crate) fn new() -> Self {
        Self {
            shown: 0,
            dropped: 0,
        }
    }

    pub(crate) fn presented(&mut self) {
        self.shown += 1;
    }

    pub(crate) fn drop_frames(&mut self, count: usize) {
        self.dropped += count as u64;
    }

    pub(crate) fn text(&self, frame_rate: Option<f64>) -> CString {
        let frame_rate = frame_rate.map_or_else(|| "UNKNOWN".to_owned(), |fps| format!("{fps:.3}"));
        CString::new(format!(
            "FPS: {frame_rate}  SHOWN: {}  DROPPED: {}",
            self.shown, self.dropped
        ))
        .expect("statistics text has no NUL bytes")
    }
}
