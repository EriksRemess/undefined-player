use crate::{Result, ffi};
use std::ffi::{CStr, c_void};
use std::ptr;

pub(crate) unsafe fn ffmpeg_error(code: i32) -> String {
    let mut buffer = [0_i8; 128];
    if unsafe { ffi::up_av_error_string(code, buffer.as_mut_ptr(), buffer.len()) } < 0 {
        return format!("FFmpeg error {code}");
    }
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

pub(crate) unsafe fn ffmpeg_error_is_again(code: i32) -> bool {
    unsafe { ffi::up_av_error_is_again(code) != 0 }
}

pub(crate) unsafe fn ffmpeg_error_is_eof(code: i32) -> bool {
    unsafe { ffi::up_av_error_is_eof(code) != 0 }
}

pub(crate) unsafe fn ffmpeg_name(pointer: *const std::ffi::c_char) -> Option<String> {
    (!pointer.is_null()).then(|| {
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .to_uppercase()
    })
}

pub(crate) unsafe fn stream_metadata(
    format: *const ffi::UpAvFormat,
    stream_index: u32,
    key: &'static CStr,
) -> Option<String> {
    let pointer = unsafe { ffi::up_av_stream_metadata(format, stream_index, key.as_ptr()) };
    if pointer.is_null() {
        return None;
    }
    let value = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .trim()
        .to_owned();
    (!value.is_empty()).then_some(value)
}

pub(crate) struct Decoder {
    context: *mut ffi::UpAvDecoder,
    pub(crate) stream_index: i32,
    pub(crate) time_base: f64,
    pub(crate) frame_duration: f64,
    pub(crate) uses_vulkan: bool,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { ffi::up_av_decoder_free(&mut self.context) };
    }
}

impl Decoder {
    pub(crate) fn as_ptr(&self) -> *mut ffi::UpAvDecoder {
        self.context
    }

    pub(crate) unsafe fn open(
        format: *mut ffi::UpAvFormat,
        stream_index: i32,
        vulkan_device: Option<*mut c_void>,
    ) -> Result<Self> {
        let context = unsafe {
            ffi::up_av_decoder_open(
                format,
                stream_index,
                vulkan_device.unwrap_or(ptr::null_mut()),
                i32::from(vulkan_device.is_some()),
            )
        };
        if context.is_null() {
            return Err(unsafe { CStr::from_ptr(ffi::up_av_decoder_error()) }
                .to_string_lossy()
                .into_owned());
        }
        Ok(Self {
            context,
            stream_index: unsafe { ffi::up_av_decoder_stream_index(context) },
            time_base: unsafe { ffi::up_av_decoder_time_base(context) },
            frame_duration: unsafe { ffi::up_av_decoder_frame_duration(context) },
            uses_vulkan: unsafe { ffi::up_av_decoder_uses_vulkan(context) } != 0,
        })
    }
}

pub(crate) struct VideoFrame {
    frame: *mut ffi::UpAvFrame,
    pub(crate) pts: f64,
    pub(crate) duration: f64,
}

impl VideoFrame {
    pub(crate) fn clone_reference(&self) -> Option<Self> {
        let frame = unsafe { ffi::up_av_frame_clone(self.frame) };
        (!frame.is_null()).then(|| unsafe { Self::from_raw(frame, self.pts, self.duration) })
    }

    /// Takes ownership of a non-null FFmpeg frame reference.
    pub(crate) unsafe fn from_raw(frame: *mut ffi::UpAvFrame, pts: f64, duration: f64) -> Self {
        Self {
            frame,
            pts,
            duration,
        }
    }

    pub(crate) fn as_ptr(&self) -> *mut ffi::UpAvFrame {
        self.frame
    }
}

// The worker transfers exclusive ownership of each reference-counted AVFrame
// to the presentation thread; the pointer is never accessed concurrently.
unsafe impl Send for VideoFrame {}

impl Drop for VideoFrame {
    fn drop(&mut self) {
        unsafe { ffi::up_av_frame_free(&mut self.frame) };
    }
}
