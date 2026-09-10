//! Player state and method policy live here; native/mpris.c owns GIO transport.
use crate::ffi;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{self, NonNull};

const TRACK_ID: &CStr = c"/com/github/undefined_player/track/1";
const COMMAND_CAPACITY: usize = 16;

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
impl PlaybackStatus {
    fn name(self) -> &'static CStr {
        match self {
            Self::Playing => c"Playing",
            Self::Paused => c"Paused",
            Self::Stopped => c"Stopped",
        }
    }
}

struct State {
    title: CString,
    artist: Option<CString>,
    art_uri: Option<CString>,
    can_previous: bool,
    can_next: bool,
    uri: Option<CString>,
    duration_us: i64,
    position_us: i64,
    status: PlaybackStatus,
    commands: VecDeque<MprisCommand>,
}
impl State {
    fn new(title: &CStr, artist: Option<&str>, path: &Path, duration_us: i64) -> Self {
        Self {
            title: if title.is_empty() {
                c"Unknown media"
            } else {
                title
            }
            .into(),
            artist: artist
                .filter(|artist| !artist.is_empty())
                .map(|artist| CString::new(artist).expect("artist has no NUL bytes")),
            art_uri: None,
            can_previous: false,
            can_next: false,
            uri: file_uri(path),
            duration_us: duration_us.max(0),
            position_us: 0,
            status: PlaybackStatus::Playing,
            commands: VecDeque::with_capacity(COMMAND_CAPACITY),
        }
    }

    fn command(&mut self, root: bool, method: &[u8], track_id: Option<&CStr>, value: i64) {
        let command = match (root, method) {
            (true, b"Quit") => MprisCommand::Quit,
            (false, b"Play") => MprisCommand::Play,
            (false, b"Pause") => MprisCommand::Pause,
            (false, b"PlayPause") => MprisCommand::PlayPause,
            (false, b"Stop") => MprisCommand::Stop,
            (false, b"Next") => MprisCommand::NextChapter,
            (false, b"Previous") => MprisCommand::PreviousChapter,
            (false, b"Seek") => MprisCommand::Seek(value),
            (false, b"SetPosition") if track_id == Some(TRACK_ID) => {
                MprisCommand::SetPosition(value)
            }
            _ => return,
        };
        if self.commands.len() == COMMAND_CAPACITY {
            self.commands.pop_front();
        }
        self.commands.push_back(command);
    }

    fn property(&self, root: bool, name: &[u8]) -> Option<ffi::UpMprisValue> {
        let mut value = ffi::UpMprisValue::default();
        match (root, name) {
            (true, b"CanQuit") | (false, b"CanPlay" | b"CanPause" | b"CanSeek" | b"CanControl") => {
                value.kind = ffi::UP_MPRIS_VALUE_BOOL;
                value.integer = 1;
            }
            (true, b"Fullscreen" | b"CanSetFullscreen" | b"CanRaise" | b"HasTrackList")
            | (false, b"Shuffle") => {
                value.kind = ffi::UP_MPRIS_VALUE_BOOL;
            }
            (false, b"CanGoNext" | b"CanGoPrevious") => {
                value.kind = ffi::UP_MPRIS_VALUE_BOOL;
                value.integer = i64::from(if name == b"CanGoNext" {
                    self.can_next
                } else {
                    self.can_previous
                });
            }
            (true, b"Identity") => {
                value.kind = ffi::UP_MPRIS_VALUE_STRING;
                value.text = c"Undefined Player".as_ptr();
            }
            (true, b"DesktopEntry") => {
                value.kind = ffi::UP_MPRIS_VALUE_STRING;
                value.text = c"undefined-player".as_ptr();
            }
            (true, b"SupportedUriSchemes" | b"SupportedMimeTypes") => {
                value.kind = ffi::UP_MPRIS_VALUE_EMPTY_STRINGS;
            }
            (false, b"PlaybackStatus") => {
                value.kind = ffi::UP_MPRIS_VALUE_STRING;
                value.text = self.status.name().as_ptr();
            }
            (false, b"LoopStatus") => {
                value.kind = ffi::UP_MPRIS_VALUE_STRING;
                value.text = c"None".as_ptr();
            }
            (false, b"Rate" | b"Volume" | b"MinimumRate" | b"MaximumRate") => {
                value.kind = ffi::UP_MPRIS_VALUE_DOUBLE;
                value.real = 1.0;
            }
            (false, b"Position") => {
                value.kind = ffi::UP_MPRIS_VALUE_INT64;
                value.integer = self.position_us;
            }
            (false, b"Metadata") => {
                value.kind = ffi::UP_MPRIS_VALUE_METADATA;
                value.track_id = TRACK_ID.as_ptr();
                value.title = self.title.as_ptr();
                value.artist = self
                    .artist
                    .as_ref()
                    .map_or(ptr::null(), |artist| artist.as_ptr());
                value.art_uri = self
                    .art_uri
                    .as_ref()
                    .map_or(ptr::null(), |uri| uri.as_ptr());
                value.uri = self.uri.as_ref().map_or(ptr::null(), |uri| uri.as_ptr());
                value.duration_us = self.duration_us;
            }
            _ => return None,
        }
        Some(value)
    }

    fn navigation(&mut self, previous: bool, next: bool) -> bool {
        let changed = (self.can_previous, self.can_next) != (previous, next);
        (self.can_previous, self.can_next) = (previous, next);
        changed
    }

    fn update(&mut self, status: PlaybackStatus, position_us: i64) -> bool {
        self.position_us = position_us.max(0);
        let changed = self.status != status;
        self.status = status;
        changed
    }
}

fn file_uri(path: &Path) -> Option<CString> {
    let absolute = std::path::absolute(path).ok()?;
    let mut uri = String::from("file://");
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in absolute.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(byte) {
            uri.push(char::from(*byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[(byte >> 4) as usize]));
            uri.push(char::from(HEX[(byte & 15) as usize]));
        }
    }
    CString::new(uri).ok()
}

// GIO dispatches on the context created by this object, exclusively when its
// owner calls dispatch(). No RefCell borrow is held across any native call.
unsafe extern "C" fn command_callback(
    data: *mut c_void,
    root: bool,
    method: *const c_char,
    track_id: *const c_char,
    value: i64,
) {
    let state = unsafe { &*data.cast::<RefCell<State>>() };
    let method = unsafe { CStr::from_ptr(method) };
    let track_id = if track_id.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(track_id) })
    };
    state
        .borrow_mut()
        .command(root, method.to_bytes(), track_id, value);
}

unsafe extern "C" fn property_callback(
    data: *mut c_void,
    root: bool,
    name: *const c_char,
    value: *mut ffi::UpMprisValue,
) -> bool {
    let state = unsafe { &*data.cast::<RefCell<State>>() };
    let name = unsafe { CStr::from_ptr(name) };
    if let Some(property) = state.borrow().property(root, name.to_bytes()) {
        // Metadata strings are owned for the entire registration lifetime;
        // transport serializes these pointers before another callback can run.
        unsafe { *value = property };
        true
    } else {
        false
    }
}

pub(crate) struct Mpris {
    native: NonNull<ffi::UpMpris>,
    // Box keeps callback data at a stable address when Mpris moves.
    state: Box<RefCell<State>>,
}
impl Mpris {
    pub(crate) fn create(
        title: &CStr,
        artist: Option<&str>,
        path: &Path,
        duration_us: i64,
        artwork: Option<&Path>,
    ) -> Option<Self> {
        let mut initial = State::new(title, artist, path, duration_us);
        initial.art_uri = artwork.and_then(file_uri);
        let state = Box::new(RefCell::new(initial));
        let callbacks = ffi::UpMprisCallbacks {
            data: (&*state as *const RefCell<State>).cast_mut().cast(),
            command: command_callback,
            property: property_callback,
        };
        let bus_name = CString::new(format!(
            "org.mpris.MediaPlayer2.undefined_player.instance{}",
            std::process::id()
        ))
        .unwrap();
        let xml = CString::new(include_str!("mpris.xml")).expect("MPRIS XML contains no NUL");
        let Some(native) = NonNull::new(unsafe {
            ffi::up_mpris_create(bus_name.as_ptr(), xml.as_ptr(), &callbacks)
        }) else {
            eprintln!("warning: out of memory while enabling MPRIS");
            return None;
        };
        let mpris = Self { native, state };
        if unsafe { ffi::up_mpris_active(native.as_ptr()) } == 0 {
            let error =
                unsafe { CStr::from_ptr(ffi::up_mpris_error(native.as_ptr())) }.to_string_lossy();
            eprintln!("warning: MPRIS unavailable: {error}");
            return None;
        }
        Some(mpris)
    }

    pub(crate) fn navigation(&self, previous: bool, next: bool) {
        let changed = self.state.borrow_mut().navigation(previous, next);
        if changed {
            unsafe { ffi::up_mpris_navigation_changed(self.native.as_ptr(), previous, next) };
        }
    }

    pub(crate) fn dispatch(&self) {
        unsafe { ffi::up_mpris_dispatch(self.native.as_ptr()) };
    }
    pub(crate) fn take_command(&self) -> Option<MprisCommand> {
        self.state.borrow_mut().commands.pop_front()
    }
    pub(crate) fn update(&self, status: PlaybackStatus, position_us: i64) {
        let changed = self.state.borrow_mut().update(status, position_us);
        if changed {
            unsafe { ffi::up_mpris_status_changed(self.native.as_ptr(), status.name().as_ptr()) };
        }
    }
    pub(crate) fn seeked(&self, position_us: i64) {
        let position_us = position_us.max(0);
        self.state.borrow_mut().position_us = position_us;
        unsafe { ffi::up_mpris_seeked(self.native.as_ptr(), position_us) };
    }
}
impl Drop for Mpris {
    fn drop(&mut self) {
        // Unregister callbacks before Rust drops their state and string storage.
        unsafe { ffi::up_mpris_destroy(self.native.as_ptr()) };
    }
}

pub(crate) fn seconds_to_microseconds(seconds: f64) -> i64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        0
    } else {
        (seconds * 1_000_000.0).min(i64::MAX as f64) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_are_bounded_ordered_and_track_scoped() {
        let mut state = State::new(c"Video", None, Path::new("/tmp/video.mkv"), 100);
        state.command(false, b"SetPosition", Some(c"/wrong/track"), 20);
        state.command(false, b"Next", None, 0);
        state.command(false, b"Previous", None, 0);
        assert_eq!(state.commands.pop_front(), Some(MprisCommand::NextChapter));
        assert_eq!(
            state.commands.pop_front(),
            Some(MprisCommand::PreviousChapter)
        );
        assert!(state.commands.is_empty());
        for value in 0..32 {
            state.command(false, b"Seek", None, value);
        }
        assert_eq!(state.commands.len(), COMMAND_CAPACITY);
        assert_eq!(state.commands.pop_front(), Some(MprisCommand::Seek(16)));
        state.commands.clear();
        state.command(false, b"SetPosition", Some(TRACK_ID), 20);
        state.command(true, b"Quit", None, 0);
        assert_eq!(
            state.commands.pop_front(),
            Some(MprisCommand::SetPosition(20))
        );
        assert_eq!(state.commands.pop_front(), Some(MprisCommand::Quit));
    }
    #[test]
    fn properties_follow_state_without_advertising_a_playlist() {
        let mut state = State::new(c"Video", None, Path::new("/tmp/video.mkv"), -1);
        assert!(!state.update(PlaybackStatus::Playing, -1));
        assert_eq!(state.position_us, 0);
        assert!(state.update(PlaybackStatus::Paused, 100));
        assert!(!state.update(PlaybackStatus::Paused, 200));
        assert_eq!(state.property(false, b"Position").unwrap().integer, 200);
        let status = state.property(false, b"PlaybackStatus").unwrap();
        assert_eq!(unsafe { CStr::from_ptr(status.text) }, c"Paused");
        assert_eq!(state.property(false, b"CanGoNext").unwrap().integer, 0);
        assert_eq!(state.property(false, b"CanGoPrevious").unwrap().integer, 0);
        assert_eq!(state.property(false, b"Metadata").unwrap().duration_us, 0);
    }
    #[test]
    fn desktop_metadata_and_chapter_capabilities_are_exposed() {
        let mut state = State::new(
            c"Seven Samurai",
            Some("Akira Kurosawa"),
            Path::new("/tmp/movie.mp4"),
            100,
        );
        state.art_uri = file_uri(Path::new("/tmp/cover image.jpg"));
        let metadata = state.property(false, b"Metadata").unwrap();
        assert_eq!(unsafe { CStr::from_ptr(metadata.title) }, c"Seven Samurai");
        assert_eq!(
            unsafe { CStr::from_ptr(metadata.artist) },
            c"Akira Kurosawa"
        );
        assert_eq!(
            unsafe { CStr::from_ptr(metadata.art_uri) },
            c"file:///tmp/cover%20image.jpg"
        );
        assert!(state.navigation(false, true));
        assert!(!state.navigation(false, true));
        assert_eq!(state.property(false, b"CanGoNext").unwrap().integer, 1);
        assert_eq!(state.property(false, b"CanGoPrevious").unwrap().integer, 0);
        assert!(state.navigation(true, false));
        assert_eq!(state.property(false, b"CanGoNext").unwrap().integer, 0);
        assert_eq!(state.property(false, b"CanGoPrevious").unwrap().integer, 1);
        let empty = State::new(c"Filename", None, Path::new("/tmp/movie.mp4"), 0);
        let metadata = empty.property(false, b"Metadata").unwrap();
        assert!(metadata.artist.is_null() && metadata.art_uri.is_null());
    }
    #[test]
    fn metadata_uris_preserve_special_and_non_utf8_filenames() {
        assert_eq!(
            file_uri(Path::new("/tmp/Tokyo 🗼 [id] #1%.webm"))
                .unwrap()
                .to_str()
                .unwrap(),
            "file:///tmp/Tokyo%20%F0%9F%97%BC%20%5Bid%5D%20%231%25.webm"
        );
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff.mkv"));
        assert_eq!(
            file_uri(path).unwrap().to_str().unwrap(),
            "file:///tmp/%FF.mkv"
        );
    }
}
