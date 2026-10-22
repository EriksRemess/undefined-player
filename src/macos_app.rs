//! Finder open-file events and the native app's Open dialog.
use crate::{Result, source::MediaSource};
use std::ffi::{CStr, CString, OsStr, c_char};
use std::os::unix::ffi::OsStrExt;

unsafe extern "C" {
    fn up_mac_app_is_bundle() -> bool;
    fn up_mac_app_take_file() -> *mut c_char;
    fn up_mac_app_choose_file() -> *mut c_char;
    fn up_mac_app_free_path(path: *mut c_char);
    fn up_mac_app_show_error(message: *const c_char);
}

pub(crate) fn is_bundle() -> bool {
    unsafe { up_mac_app_is_bundle() }
}

fn source_from_path(path: *mut c_char) -> Result<Option<MediaSource>> {
    if path.is_null() {
        return Ok(None);
    }
    let argument = unsafe { OsStr::from_bytes(CStr::from_ptr(path).to_bytes()).to_owned() };
    unsafe { up_mac_app_free_path(path) };
    MediaSource::from_argument(argument).map(Some)
}

pub(crate) fn take_source() -> Result<Option<MediaSource>> {
    source_from_path(unsafe { up_mac_app_take_file() })
}

pub(crate) fn choose_source() -> Result<Option<MediaSource>> {
    source_from_path(unsafe { up_mac_app_choose_file() })
}

pub(crate) fn show_error(message: &str) {
    if let Ok(message) = CString::new(message) {
        unsafe { up_mac_app_show_error(message.as_ptr()) };
    }
}
