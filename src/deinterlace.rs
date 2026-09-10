use crate::{decoder::VideoFrame, ffi};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Auto,
    On,
    Off,
}

impl Mode {
    pub(crate) fn status(self, active: bool) -> &'static str {
        match self {
            Self::Auto if active => "AUTO (ON)",
            Self::Auto => "AUTO (OFF)",
            Self::On => "ON",
            Self::Off => "OFF",
        }
    }

    pub(crate) fn cycle(&mut self) -> &'static str {
        *self = match *self {
            Self::Auto => Self::On,
            Self::On => Self::Off,
            Self::Off => Self::Auto,
        };
        match self {
            Self::Auto => "DEINTERLACE: AUTO",
            Self::On => "DEINTERLACE: ON",
            Self::Off => "DEINTERLACE: OFF",
        }
    }

    // Shared field values: 0 = progressive, 1 = top first, 2 = bottom first.
    fn field(self, detected: i32) -> i32 {
        match self {
            Self::Auto => detected,
            Self::On => {
                if detected == 0 {
                    1
                } else {
                    detected
                }
            }
            Self::Off => 0,
        }
    }

    pub(crate) fn frame_field(self, frame: &VideoFrame) -> i32 {
        self.field(unsafe { ffi::up_av_frame_field(frame.as_ptr()) })
    }
}

// Exclude gaps and discontinuities from temporal filtering. Missing references
// at startup, EOF, or after a seek use a single-frame filter instead.
pub(crate) fn adjacent(earlier: &VideoFrame, later: &VideoFrame) -> bool {
    adjacent_pts(earlier.pts, later.pts, earlier.duration)
}

fn adjacent_pts(earlier: f64, later: f64, duration: f64) -> bool {
    duration.is_finite()
        && duration > 0.0
        && (later - earlier - duration).abs() <= (duration * 0.1).max(0.002)
        && later > earlier
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_and_manual_field_selection() {
        for (mode, expected) in [
            (Mode::Auto, [0, 1, 2]),
            (Mode::On, [1, 1, 2]),
            (Mode::Off, [0, 0, 0]),
        ] {
            for (detected, field) in expected.into_iter().enumerate() {
                assert_eq!(mode.field(detected as i32), field);
            }
        }
        let mut mode = Mode::default();
        assert_eq!(mode.cycle(), "DEINTERLACE: ON");
        assert_eq!(mode.cycle(), "DEINTERLACE: OFF");
        assert_eq!(mode.cycle(), "DEINTERLACE: AUTO");
        assert_eq!(mode, Mode::Auto);
    }

    #[test]
    fn temporal_references_exclude_seeks_and_timestamp_gaps() {
        assert!(adjacent_pts(1.0, 1.033, 1.0 / 30.0));
        for (start, end, duration) in [
            (1.0, 1.0, 0.04),
            (1.0, 0.96, 0.04),
            (1.0, 1.08, 0.04),
            (1.0, 1.04, 0.0),
            (1.0, 1.04, f64::NAN),
        ] {
            assert!(!adjacent_pts(start, end, duration));
        }
    }
}
