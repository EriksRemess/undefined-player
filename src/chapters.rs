use crate::ffi;
use std::ffi::{CStr, CString};

pub(crate) struct Chapters {
    starts: Vec<f64>,
    labels: Vec<CString>,
    seek_floor: Option<f64>,
}

impl Chapters {
    pub(crate) unsafe fn inspect(
        format: *const ffi::UpAvFormat,
        playback_start: f64,
        duration: Option<f64>,
    ) -> Self {
        let entries = (0..unsafe { ffi::up_av_chapter_count(format) }).map(|index| {
            let start = unsafe { ffi::up_av_chapter_start(format, index) };
            let title = unsafe { ffi::up_av_chapter_title(format, index) };
            let title = (!title.is_null()).then(|| {
                unsafe { CStr::from_ptr(title) }
                    .to_string_lossy()
                    .into_owned()
            });
            (start, title)
        });
        Self::from_entries(entries, playback_start, duration)
    }

    #[cfg(test)]
    fn new(starts: impl Iterator<Item = f64>, playback_start: f64, duration: Option<f64>) -> Self {
        Self::from_entries(starts.map(|start| (start, None)), playback_start, duration)
    }

    fn from_entries(
        entries: impl Iterator<Item = (f64, Option<String>)>,
        playback_start: f64,
        duration: Option<f64>,
    ) -> Self {
        let mut entries: Vec<_> = entries
            .filter(|(start, _)| start.is_finite())
            .map(|(start, title)| ((start - playback_start).max(0.0), title))
            .filter(|(start, _)| duration.is_none_or(|duration| *start < duration))
            .collect();
        entries.sort_by(|a, b| a.0.total_cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        let count = entries.len();
        let (starts, labels) = entries
            .into_iter()
            .enumerate()
            .map(|(index, (start, title))| {
                let mut label = format!("CHAPTER {} / {count}", index + 1);
                if let Some(title) = title {
                    let title = crate::metadata::single_line_metadata(&title);
                    if !title.is_empty() {
                        label.push_str(" — ");
                        label.push_str(&title.to_uppercase());
                    }
                }
                (
                    start,
                    CString::new(label).expect("chapter label has no NUL bytes"),
                )
            })
            .unzip();
        Self {
            starts,
            labels,
            seek_floor: None,
        }
    }

    pub(crate) fn label(&self, index: usize) -> Option<&CStr> {
        self.labels.get(index).map(|label| label.as_c_str())
    }

    pub(crate) fn current(&self, position: f64) -> Option<usize> {
        if !position.is_finite() {
            return None;
        }
        self.starts
            .partition_point(|start| *start <= position)
            .checked_sub(1)
    }

    pub(crate) fn seeked(&mut self, position: f64) {
        self.seek_floor = position.is_finite().then_some(position);
    }

    pub(crate) fn playback_position(&self, position: f64) -> f64 {
        // The decoded frame can start before the requested seek position.
        // Retain that position for chapter selection until playback catches up.
        // Explicit seek requests bypass this adjustment and replace the floor.
        self.seek_floor
            .map_or(position, |floor| position.max(floor))
    }

    pub(crate) fn markers(&self, duration: Option<f64>) -> Vec<f32> {
        match duration.filter(|duration| duration.is_finite() && *duration > 0.0) {
            Some(duration) => self
                .starts
                .iter()
                .map(|start| (start / duration) as f32)
                .collect(),
            None => Vec::new(),
        }
    }

    pub(crate) fn target(&self, position: f64, forward: bool) -> Option<f64> {
        if !position.is_finite() {
            return None;
        }
        let current = self.current(position);
        let index = if forward {
            current.map_or(0, |index| index + 1)
        } else {
            current?.checked_sub(1)?
        };
        self.starts.get(index).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapter_labels_follow_sorted_timestamps_and_handle_missing_titles() {
        let chapters = Chapters::from_entries(
            [
                (20.0, Some("  gāčē\n日本語\0  ".into())),
                (0.0, None),
                (10.0, Some(" \t ".into())),
                (f64::NAN, Some("invalid".into())),
            ]
            .into_iter(),
            0.0,
            Some(30.0),
        );
        assert_eq!(
            chapters.label(0).unwrap().to_str().unwrap(),
            "CHAPTER 1 / 3"
        );
        assert_eq!(
            chapters.label(1).unwrap().to_str().unwrap(),
            "CHAPTER 2 / 3"
        );
        assert_eq!(
            chapters.label(2).unwrap().to_str().unwrap(),
            "CHAPTER 3 / 3 — GĀČĒ 日本語"
        );
        assert!(chapters.label(3).is_none());
        assert_eq!(chapters.current(9.97), Some(0));
        assert_eq!(chapters.current(-1.0), None);
    }

    #[test]
    fn seek_selection_survives_early_frames_and_resets_on_backward_seeks() {
        let mut chapters = Chapters::new([0.0, 3.2, 6.4, 9.6].into_iter(), 0.0, Some(30.0));
        assert_eq!(chapters.current(3.19), Some(0));
        for (requested, landed, next) in [
            (3.2, 3.0, Some(6.4)),
            (6.4, 6.2, Some(9.6)),
            (9.6, 9.4, None),
        ] {
            chapters.seeked(requested);
            let position = chapters.playback_position(landed);
            assert_eq!(position, requested);
            assert_eq!(chapters.target(position, true), next);
        }
        // An explicit position must not inherit the previous chapter selection.
        assert_eq!(chapters.target(3.1, true), Some(3.2));
        chapters.seeked(3.1);
        assert_eq!(chapters.current(chapters.playback_position(3.0)), Some(0));
        assert_eq!(chapters.current(chapters.playback_position(3.2)), Some(1));
        chapters.seeked(0.0);
        assert_eq!(
            chapters.target(chapters.playback_position(0.0), false),
            None
        );
    }

    #[test]
    fn timestamps_share_the_playback_origin_and_ignore_invalid_entries() {
        let chapters = Chapters::new(
            [30.0, f64::NAN, 10.0, 30.0, f64::INFINITY, 70.0, 9.0].into_iter(),
            10.0,
            Some(60.0),
        );
        assert_eq!(chapters.starts, [0.0, 20.0]);
        assert_eq!(chapters.markers(Some(60.0)), [0.0, 1.0 / 3.0]);
        assert!(chapters.markers(None).is_empty());
    }

    #[test]
    fn chapter_navigation_handles_boundaries_and_seek_rounding() {
        let chapters = Chapters::new([0.0, 10.0, 20.0].into_iter(), 0.0, Some(30.0));
        for (position, previous, next) in [
            (0.0, None, Some(10.0)),
            (5.0, None, Some(10.0)),
            (9.97, None, Some(10.0)),
            (10.0, Some(0.0), Some(20.0)),
            (17.0, Some(0.0), Some(20.0)),
            (20.0, Some(10.0), None),
            (30.0, Some(10.0), None),
        ] {
            assert_eq!(chapters.target(position, false), previous);
            assert_eq!(chapters.target(position, true), next);
        }
        assert_eq!(chapters.target(f64::NAN, true), None);
        let empty = Chapters::new(std::iter::empty(), 0.0, None);
        assert_eq!(empty.target(0.0, true), None);
        assert_eq!(empty.target(0.0, false), None);
        let delayed = Chapters::new([5.0].into_iter(), 0.0, None);
        assert_eq!(delayed.target(0.0, true), Some(5.0));
        assert_eq!(delayed.target(0.0, false), None);
    }
}
