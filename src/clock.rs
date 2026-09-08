use std::time::{Duration, Instant};

pub(crate) const AUDIO_CLOCK_TOLERANCE: f64 = 0.080;
pub(crate) struct WallClock {
    origin_pts: f64,
    started: Instant,
    paused_at: Option<Instant>,
    paused_duration: Duration,
}

impl WallClock {
    pub(crate) fn new(origin_pts: f64) -> Self {
        Self {
            origin_pts,
            started: Instant::now(),
            paused_at: None,
            paused_duration: Duration::ZERO,
        }
    }

    pub(crate) fn now(&self) -> f64 {
        let end = self.paused_at.unwrap_or_else(Instant::now);
        self.origin_pts + (end - self.started - self.paused_duration).as_secs_f64()
    }

    pub(crate) fn set_paused(&mut self, paused: bool) {
        match (paused, self.paused_at) {
            (true, None) => self.paused_at = Some(Instant::now()),
            (false, Some(started)) => {
                self.paused_duration += Instant::now() - started;
                self.paused_at = None;
            }
            _ => {}
        }
    }

    pub(crate) fn synchronize(&mut self, audio_pts: f64) {
        // Keep a smooth clock within the device's normal period jitter, but
        // re-anchor after an underrun or sustained clock drift.
        if (self.now() - audio_pts).abs() > AUDIO_CLOCK_TOLERANCE {
            self.seek(audio_pts);
        }
    }

    pub(crate) fn seek(&mut self, pts: f64) {
        let now = Instant::now();
        self.origin_pts = pts;
        self.started = now;
        self.paused_duration = Duration::ZERO;
        if self.paused_at.is_some() {
            self.paused_at = Some(now);
        }
    }
}

pub(crate) fn completed_seek_anchor(
    video_pts: Option<f64>,
    audio_clock: Option<f64>,
    has_audio: bool,
    eof: bool,
    buffer_full: bool,
    fallback: f64,
) -> Option<f64> {
    if (video_pts.is_none() || (has_audio && audio_clock.is_none())) && !eof && !buffer_full {
        return None;
    }
    // A full buffer must be consumed before demuxing can reach the other
    // stream. Start at the earlier timestamp and schedule late audio separately.
    Some(match (video_pts, audio_clock) {
        (Some(video), Some(audio)) => video.min(audio),
        (video, audio) => audio.or(video).unwrap_or(fallback),
    })
}
