// Shared media-control vocabulary for Linux MPRIS and macOS Now Playing.
#[cfg_attr(target_os = "macos", allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MprisCommand {
    Quit,
    Play,
    Pause,
    PlayPause,
    Stop,
    Seek(i64),
    SetPosition(i64),
    NextChapter,
    PreviousChapter,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

pub(crate) fn seconds_to_microseconds(seconds: f64) -> i64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        0
    } else {
        (seconds * 1_000_000.0).min(i64::MAX as f64) as i64
    }
}

