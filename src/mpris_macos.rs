//! macOS Now Playing and media keys, using the shared playback command policy.
use crate::source::MediaSource;
use std::ffi::{CStr, CString, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{self, NonNull};

include!("mpris_types.rs");

unsafe extern "C" {
    fn up_mac_media_create(
        title: *const c_char,
        artist: *const c_char,
        duration: f64,
        artwork: *const c_char,
    ) -> *mut c_void;
    fn up_mac_media_navigation(media: *mut c_void, previous: bool, next: bool);
    fn up_mac_media_poll(media: *mut c_void, value: *mut i64) -> i32;
    fn up_mac_media_update(media: *mut c_void, status: i32, position: f64, force: bool);
    fn up_mac_media_destroy(media: *mut c_void);
}

pub(crate) struct Mpris(NonNull<c_void>);
impl Mpris {
    pub(crate) fn create(
        title: &CStr,
        artist: Option<&str>,
        _source: &MediaSource,
        duration_us: i64,
        artwork: Option<&Path>,
    ) -> Option<Self> {
        let artist = artist.and_then(|text| CString::new(text).ok());
        let artwork = artwork.and_then(|path| CString::new(path.as_os_str().as_bytes()).ok());
        NonNull::new(unsafe {
            up_mac_media_create(
                title.as_ptr(),
                artist.as_ref().map_or(ptr::null(), |text| text.as_ptr()),
                duration_us.max(0) as f64 / 1_000_000.0,
                artwork.as_ref().map_or(ptr::null(), |path| path.as_ptr()),
            )
        })
        .map(Self)
    }
    pub(crate) fn navigation(&self, previous: bool, next: bool) {
        unsafe { up_mac_media_navigation(self.0.as_ptr(), previous, next) };
    }
    pub(crate) fn dispatch(&self) {} // SDL pumps the Cocoa main run loop.
    pub(crate) fn take_command(&self) -> Option<MprisCommand> {
        let mut value = 0;
        match unsafe { up_mac_media_poll(self.0.as_ptr(), &mut value) } {
            1 => Some(MprisCommand::Play),
            2 => Some(MprisCommand::Pause),
            3 => Some(MprisCommand::PlayPause),
            4 => Some(MprisCommand::Stop),
            5 => Some(MprisCommand::Seek(value)),
            6 => Some(MprisCommand::SetPosition(value)),
            7 => Some(MprisCommand::NextChapter),
            8 => Some(MprisCommand::PreviousChapter),
            _ => None,
        }
    }
    pub(crate) fn update(&self, status: PlaybackStatus, position_us: i64) {
        let status = match status {
            PlaybackStatus::Playing => 0,
            PlaybackStatus::Paused => 1,
            PlaybackStatus::Stopped => 2,
        };
        unsafe {
            up_mac_media_update(
                self.0.as_ptr(),
                status,
                position_us.max(0) as f64 / 1_000_000.0,
                false,
            )
        };
    }
    pub(crate) fn options(&self, _muted: bool, _looping: bool) {}
    pub(crate) fn seeked(&self, position_us: i64) {
        // The next update publishes the actual pause/play state as well.
        // Force the elapsed timestamp without temporarily resuming playback.
        unsafe {
            up_mac_media_update(
                self.0.as_ptr(),
                -1,
                position_us.max(0) as f64 / 1_000_000.0,
                true,
            )
        };
    }
}
impl Drop for Mpris {
    fn drop(&mut self) {
        unsafe { up_mac_media_destroy(self.0.as_ptr()) };
    }
}
