use crate::luma::Luma;
use crate::{decoder::VideoFrame, ffi};
use std::collections::VecDeque;

// Keep temporal samples bounded, retaining adjacent scan lines and their
// parity. Downscaling the image first would erase the combing we need to see.
struct Sample {
    pixels: Vec<[u8; 4]>,
    dimensions: (i32, i32),
    pts: f64,
    duration: f64,
}

impl Sample {
    fn new(frame: &VideoFrame) -> Option<Self> {
        let luma = Luma::new(frame)?;
        let (width, height) = (luma.0.width as usize, luma.0.height as usize);
        if width < 8 || height < 8 {
            return None;
        }
        let columns = width.min(128);
        let available_pairs = (height - 4) / 2;
        let pairs = available_pairs.min(128);
        let mut pixels = Vec::with_capacity(columns * pairs);
        for pair in 0..pairs {
            let y = 2 + 2 * (pair * (available_pairs - 1) / (pairs - 1));
            for column in 0..columns {
                let x = column * (width - 1) / (columns - 1);
                pixels.push(std::array::from_fn(|row| luma.sample(x, y + row - 1)));
            }
        }
        Some(Self {
            pixels,
            dimensions: frame.dimensions(),
            pts: frame.pts,
            duration: frame.duration,
        })
    }
}

// Compare the agreement of alternating scan lines across three pictures,
// following the temporal-coherence principle used by FFmpeg's idet filter.
// Returns 0 for progressive, 1/2 for top/bottom first, or None without evidence.
fn classify(previous: &Sample, current: &Sample, next: &Sample) -> Option<i32> {
    let mut temporal = [0_u64; 2];
    let mut spatial = 0_u64;
    for ((previous, current), next) in previous
        .pixels
        .iter()
        .zip(&current.pixels)
        .zip(&next.pixels)
    {
        for parity in 0..2 {
            let neighbors = i32::from(current[parity]) + i32::from(current[parity + 2]);
            let difference = |pixel| (neighbors - 2 * i32::from(pixel)).unsigned_abs() as u64;
            temporal[parity] += difference(previous[parity + 1]);
            temporal[parity ^ 1] += difference(next[parity + 1]);
            spatial += difference(current[parity + 1]);
        }
    }
    if temporal[0].min(temporal[1]) as f64 > 1.5 * spatial as f64 {
        Some(0)
    } else if temporal[0] as f64 > 1.10 * temporal[1] as f64 {
        Some(1)
    } else if temporal[1] as f64 > 1.10 * temporal[0] as f64 {
        Some(2)
    } else {
        None
    }
}

#[derive(Default)]
pub(crate) struct Detector {
    samples: VecDeque<Sample>,
    candidate: Option<i32>,
    count: u8,
    detected: Option<i32>,
}

impl Detector {
    pub(crate) fn analyze(&mut self, frame: &VideoFrame) -> Option<i32> {
        // Preserve the fast path for progressive hardware video. Only flagged
        // interlaced hardware frames need readback, on the decode worker.
        if unsafe { ffi::up_av_frame_is_vulkan(frame.as_ptr()) } != 0
            && unsafe { ffi::up_av_frame_field(frame.as_ptr()) } == 0
        {
            *self = Self::default();
            return None;
        }
        let Some(sample) = Sample::new(frame) else {
            *self = Self::default();
            return None;
        };
        self.observe_sample(sample)
    }

    fn observe_sample(&mut self, sample: Sample) -> Option<i32> {
        if self.samples.back().is_some_and(|previous| {
            previous.dimensions != sample.dimensions
                || !adjacent_pts(previous.pts, sample.pts, previous.duration)
        }) {
            *self = Self::default();
        }
        self.samples.push_back(sample);
        if self.samples.len() == 3 {
            let verdict = classify(&self.samples[0], &self.samples[1], &self.samples[2]);
            self.observe(verdict);
            self.samples.pop_front();
        }
        self.detected
    }

    fn observe(&mut self, verdict: Option<i32>) {
        if verdict.is_none() {
            return;
        }
        if verdict == self.candidate {
            self.count = self.count.saturating_add(1);
        } else {
            self.candidate = verdict;
            self.count = 1;
        }
        // Require stronger evidence before turning deinterlacing off. Static
        // pictures remain inconclusive and retain the last reliable decision.
        if self.count >= if verdict == Some(0) { 4 } else { 2 } {
            self.detected = verdict;
        }
    }
}

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
        let flagged = unsafe { ffi::up_av_frame_field(frame.as_ptr()) };
        if self == Self::Auto {
            frame.detected_field.unwrap_or(flagged)
        } else {
            self.field(flagged)
        }
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

    fn moving_sample(index: usize, field: i32) -> Sample {
        let mut pixels = Vec::new();
        for pair in 0..30 {
            let y = 2 + pair * 2;
            for x in 0..64 {
                pixels.push(std::array::from_fn(|row| {
                    let line = y + row - 1;
                    let time = if field == 0 {
                        index * 2
                    } else {
                        index * 2 + usize::from(line % 2 == usize::from(field == 1))
                    };
                    if (x + time * 3 + line / 8) % 32 < 16 {
                        32
                    } else {
                        208
                    }
                }));
            }
        }
        Sample {
            pixels,
            dimensions: (64, 64),
            pts: index as f64 * 0.04,
            duration: 0.04,
        }
    }

    #[test]
    fn picture_detection_tracks_motion_and_recovers_after_a_mode_change() {
        let mut detector = Detector::default();
        let mut index = 0;
        for field in [0, 1, 2, 0] {
            for _ in 0..12 {
                let detected = detector.observe_sample(moving_sample(index, field));
                index += 1;
                if index % 12 == 0 {
                    assert_eq!(detected, Some(field));
                }
            }
        }
        let mut discontinuous = moving_sample(index + 10, 1);
        assert!(detector.observe_sample(discontinuous).is_none());
        for _ in 0..8 {
            index += 1;
            detector.observe_sample(moving_sample(index + 10, 1));
        }
        assert_eq!(detector.detected, Some(1));
        discontinuous = moving_sample(index + 11, 1);
        discontinuous.dimensions = (128, 64);
        assert!(detector.observe_sample(discontinuous).is_none());
    }

    #[test]
    fn static_pictures_and_isolated_guesses_cannot_disable_deinterlacing() {
        let still = Sample {
            pixels: vec![[16; 4]; 128],
            dimensions: (64, 64),
            pts: 0.0,
            duration: 0.04,
        };
        assert_eq!(classify(&still, &still, &still), None);
        let detailed = moving_sample(5, 0);
        assert_eq!(classify(&detailed, &detailed, &detailed), None);
        let mut detector = Detector::default();
        detector.observe(Some(0));
        detector.observe(Some(1));
        detector.observe(Some(0));
        detector.observe(None);
        assert_eq!(detector.detected, None);
        for _ in 0..3 {
            detector.observe(Some(0));
        }
        assert_eq!(detector.detected, Some(0));
        detector.observe(None);
        assert_eq!(detector.detected, Some(0));
        detector.observe(Some(1));
        detector.observe(Some(1));
        assert_eq!(detector.detected, Some(1));
    }

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
