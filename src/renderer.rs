use crate::decoder::VideoFrame;
use crate::subtitles::{SubtitleContent, SubtitleCue};
use crate::window::Window;
use crate::{Result, ffi, overlay};
use std::ffi::{CStr, CString, c_void};
use std::ptr;

pub(crate) struct Renderer<'window> {
    context: *mut ffi::UpVideoRenderer,
    overlays: overlay::Overlays,
    _window: &'window Window,
}

pub(crate) struct RendererOverlays<'a> {
    pub(crate) info: Option<(&'a CStr, f32)>,
    pub(crate) details: Option<&'a CStr>,
    pub(crate) position: Option<(&'a CStr, f32)>,
    pub(crate) scrubber: Option<(f32, f32)>,
    pub(crate) subtitle: Option<&'a SubtitleCue>,
}

impl<'window> Renderer<'window> {
    pub(crate) fn create(window: &'window Window) -> Result<Self> {
        let renderer = unsafe { ffi::up_video_renderer_create(window.as_ptr().cast()) };
        if renderer.is_null() {
            return Err("out of memory while creating the Vulkan renderer".into());
        }
        if unsafe { ffi::up_video_renderer_device(renderer) }.is_null() {
            let message = unsafe { CStr::from_ptr(ffi::up_video_renderer_error(renderer)) }
                .to_string_lossy()
                .into_owned();
            unsafe { ffi::up_video_renderer_destroy(renderer) };
            return Err(message);
        }
        Ok(Self {
            context: renderer,
            overlays: overlay::Overlays::default(),
            _window: window,
        })
    }

    pub(crate) unsafe fn device(&self) -> *mut c_void {
        unsafe { ffi::up_video_renderer_device(self.context) }
    }

    pub(crate) fn display(
        &mut self,
        frame: &VideoFrame,
        width: i32,
        height: i32,
        top_bar_alpha: f32,
        title: &CStr,
        overlays: RendererOverlays<'_>,
    ) -> Result<()> {
        let info = overlays
            .info
            .map_or(c"", |(text, _)| text)
            .to_string_lossy();
        let details = overlays.details.unwrap_or(c"").to_string_lossy();
        let position = overlays
            .position
            .map_or(c"", |(text, _)| text)
            .to_string_lossy();
        let prepared = self.overlays.prepare(
            overlay::Content {
                title: &title.to_string_lossy(),
                info: &info,
                details: &details,
                position: &position,
            },
            overlay::Visibility {
                top_bar: top_bar_alpha,
                info: overlays.info.map_or(0.0, |(_, alpha)| alpha),
                position: overlays.position.map_or(0.0, |(_, alpha)| alpha),
                scrubber: overlays.scrubber,
            },
            width,
            height,
        )?;
        let overlay = prepared.descriptor();
        let rendered_subtitle = overlays.subtitle.and_then(|cue| match &cue.content {
            SubtitleContent::Text(text) => {
                Some(CString::new(text.as_str()).expect("decoded subtitle text has no NUL bytes"))
            }
            _ => None,
        });
        let (subtitle_text, subtitle_pixels, subtitle_width, subtitle_height, subtitle_serial) =
            match overlays.subtitle {
                Some(cue) => match &cue.content {
                    SubtitleContent::Text(_) => (
                        rendered_subtitle.as_ref().unwrap().as_ptr(),
                        ptr::null(),
                        0,
                        0,
                        cue.serial,
                    ),
                    SubtitleContent::Bitmap {
                        width,
                        height,
                        pixels,
                    } => (ptr::null(), pixels.as_ptr(), *width, *height, cue.serial),
                    SubtitleContent::Clear => (ptr::null(), ptr::null(), 0, 0, 0),
                },
                None => (ptr::null(), ptr::null(), 0, 0, 0),
            };
        if unsafe {
            ffi::up_video_renderer_display(
                self.context,
                frame.as_ptr().cast(),
                width,
                height,
                &overlay,
                subtitle_text,
                subtitle_pixels,
                subtitle_width,
                subtitle_height,
                subtitle_serial,
            )
        } < 0
        {
            return Err(
                unsafe { CStr::from_ptr(ffi::up_video_renderer_error(self.context)) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        Ok(())
    }

    pub(crate) fn resize(&self, width: i32, height: i32) -> Result<()> {
        if unsafe { ffi::up_video_renderer_resize(self.context, width, height) } < 0 {
            return Err(
                unsafe { CStr::from_ptr(ffi::up_video_renderer_error(self.context)) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        Ok(())
    }
}

impl Drop for Renderer<'_> {
    fn drop(&mut self) {
        unsafe { ffi::up_video_renderer_destroy(self.context) };
    }
}
