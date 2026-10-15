use crate::decoder::VideoFrame;
use crate::subtitles::{SubtitleContent, SubtitleCue};
use crate::window::{WaylandInput, Window};
use crate::{Result, ffi, overlay};
use std::ffi::{CStr, CString, c_void};
use std::ptr;

pub(crate) struct Renderer<'window> {
    context: *mut ffi::UpVideoRenderer,
    overlays: overlay::Overlays,
    autocrop: crate::autocrop::AutoCrop,
    deinterlace: crate::deinterlace::Mode,
    zoom: crate::zoom::Zoom,
    corner_radius: u16,
    corner_pixels: f32,
    corner_disabled: bool,
    shape_key: Option<(i32, i32, i32, i32, u32)>,
    _window: &'window Window,
}

pub(crate) struct VideoFrames<'a> {
    pub(crate) current: &'a VideoFrame,
    pub(crate) previous: Option<&'a VideoFrame>,
    pub(crate) next: Option<&'a VideoFrame>,
}

pub(crate) struct RendererOverlays<'a> {
    pub(crate) info: Option<(&'a CStr, f32)>,
    pub(crate) details: Option<&'a CStr>,
    pub(crate) metadata: Option<&'a CStr>,
    pub(crate) position: Option<(&'a CStr, f32)>,
    pub(crate) scrubber: Option<(f32, f32)>,
    pub(crate) chapter_markers: &'a [f32],
    pub(crate) subtitle: Option<&'a SubtitleCue>,
    pub(crate) palette: &'a CStr,
}

fn processing_details(
    details: &str,
    deinterlace: &str,
    zoom: &str,
    crop: Option<&ffi::UpVideoCrop>,
) -> String {
    let mut text = String::new();
    for line in details.lines() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
        if line.starts_with("RESOLUTION:")
            && let Some(crop) = crop
            && (crop.right - crop.left < crop.width || crop.bottom - crop.top < crop.height)
        {
            text.push_str(&format!(
                " ({}x{} cropped)",
                crop.right - crop.left,
                crop.bottom - crop.top
            ));
        }
        if line.starts_with("DECODE:") {
            text.push_str("\nDEINTERLACE: ");
            text.push_str(deinterlace);
            text.push_str("\nZOOM: ");
            text.push_str(zoom);
        }
    }
    text
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
            autocrop: crate::autocrop::AutoCrop::default(),
            deinterlace: crate::deinterlace::Mode::default(),
            zoom: crate::zoom::Zoom::default(),
            corner_radius: 0,
            corner_pixels: 0.0,
            corner_disabled: false,
            shape_key: None,
            _window: window,
        })
    }

    pub(crate) unsafe fn device(&self) -> *mut c_void {
        unsafe { ffi::up_video_renderer_device(self.context) }
    }

    pub(crate) fn set_corner_radius(&mut self, radius: u16) {
        self.corner_radius = radius;
    }

    pub(crate) fn update_window_shape(&mut self, input: Option<&WaylandInput<'_>>) -> Result<bool> {
        let (width, height, pw, ph) = self._window.dimensions()?;
        if width <= 0 || height <= 0 || pw <= 0 || ph <= 0 {
            return Ok(false);
        }
        let scale = unsafe { ffi::up_window_corner_scale(self._window.as_ptr()) };
        let pixels = if !self.corner_disabled && input.is_some() && scale.is_finite() && scale > 0.0
        {
            (f32::from(self.corner_radius) * scale).min(pw.min(ph) as f32 * 0.5)
        } else {
            0.0
        };
        let key = (width, height, pw, ph, pixels.to_bits());
        if self.shape_key == Some(key) {
            return Ok(false);
        }
        if let Some(input) = input {
            let rectangles = if pixels > 0.0 {
                crate::geometry::WindowGeometry::new(width, height, pw, ph)
                    .unwrap()
                    .with_radius(pixels)
                    .input_rectangles()
            } else {
                Vec::new()
            };
            input.set_region(&rectangles, pixels)?;
        }
        self.shape_key = Some(key);
        self.corner_pixels = pixels;
        self.overlays
            .set_corner_inset(crate::geometry::corner_inset(pixels));
        Ok(true)
    }

    pub(crate) fn cycle_deinterlace(&mut self) -> &'static str {
        self.deinterlace.cycle()
    }

    pub(crate) fn toggle_autocrop(&mut self) -> bool {
        self.autocrop.toggle()
    }

    pub(crate) fn toggle_zoom(&mut self) -> bool {
        let crop = self.zoom.toggle(self.autocrop.enabled());
        if crop != self.autocrop.enabled() {
            self.autocrop.toggle();
        }
        self.zoom.enabled()
    }

    pub(crate) fn reset_autocrop(&mut self) {
        self.autocrop.reset();
    }

    pub(crate) fn update_autocrop(&mut self, frame: &VideoFrame, paused: bool) -> bool {
        let unavailable = self.autocrop.unavailable();
        let changed = self.autocrop.update(frame, paused);
        changed || unavailable != self.autocrop.unavailable()
    }

    pub(crate) fn set_close_hovered(&mut self, hovered: bool) -> bool {
        self.overlays.set_close_hovered(hovered)
    }

    pub(crate) fn display(
        &mut self,
        frames: VideoFrames<'_>,
        width: i32,
        height: i32,
        top_bar_alpha: f32,
        title: &CStr,
        overlays: RendererOverlays<'_>,
    ) -> Result<()> {
        let frame = frames.current;
        let field = self.deinterlace.frame_field(frame);
        // Use the same current-frame crop for the info panel and rendering.
        // An asynchronous result for a previous source size must not be shown.
        let crop = self
            .autocrop
            .crop()
            .filter(|crop| (crop.width, crop.height) == frame.dimensions());
        let fill = self.zoom.fill(
            self.autocrop.enabled(),
            crop.is_some(),
            self.autocrop.unavailable(),
        );
        let previous = frames
            .previous
            .filter(|prev| field != 0 && crate::deinterlace::adjacent(prev, frame));
        let next = frames
            .next
            .filter(|next| field != 0 && crate::deinterlace::adjacent(frame, next));
        let info = overlays
            .info
            .map_or(c"", |(text, _)| text)
            .to_string_lossy();
        let details = overlays.details.map_or_else(String::new, |details| {
            processing_details(
                &details.to_string_lossy(),
                self.deinterlace.status(field != 0),
                self.zoom.status(fill),
                crop.as_ref(),
            )
        });
        let position = overlays
            .position
            .map_or(c"", |(text, _)| text)
            .to_string_lossy();
        let prepared = self.overlays.prepare(
            overlay::Content {
                title: &title.to_string_lossy(),
                info: &info,
                details: &details,
                metadata: &overlays.metadata.map_or(c"", |text| text).to_string_lossy(),
                position: &position,
                palette: &overlays.palette.to_string_lossy(),
            },
            overlay::Visibility {
                top_bar: top_bar_alpha,
                info: overlays.info.map_or(0.0, |(_, alpha)| alpha),
                position: overlays.position.map_or(0.0, |(_, alpha)| alpha),
                scrubber: overlays.scrubber,
                chapter_markers: overlays.chapter_markers,
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
        let result = unsafe {
            ffi::up_video_renderer_display(
                self.context,
                frame.as_ptr().cast(),
                previous.map_or(ptr::null_mut(), |frame| frame.as_ptr().cast()),
                next.map_or(ptr::null_mut(), |frame| frame.as_ptr().cast()),
                field,
                width,
                height,
                &overlay,
                crop.as_ref().map_or(ptr::null(), |crop| crop),
                i32::from(fill),
                subtitle_text,
                subtitle_pixels,
                subtitle_width,
                subtitle_height,
                subtitle_serial,
                self.corner_pixels,
                c"
                    vec2 half_size = window_geometry.xy * 0.5;
                    float radius = min(window_geometry.z, min(half_size.x, half_size.y));
                    vec2 distance_to_corner = abs(gl_FragCoord.xy - half_size) - half_size + radius;
                    float distance_to_edge = length(max(distance_to_corner, vec2(0.0)))
                        + min(max(distance_to_corner.x, distance_to_corner.y), 0.0) - radius;
                    float coverage = clamp(0.5 - distance_to_edge, 0.0, 1.0);
                    color = vec4(coverage);
                "
                .as_ptr(),
            )
        };
        if result < 0 {
            return Err(
                unsafe { CStr::from_ptr(ffi::up_video_renderer_error(self.context)) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        if result > 0 {
            // The native swapchain cannot composite alpha. Restore square input
            // and control geometry on the next loop as well as square pixels.
            self.corner_disabled = true;
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
        // Finish outstanding GPU readbacks before destroying the Vulkan device.
        self.autocrop.shutdown();
        unsafe { ffi::up_video_renderer_destroy(self.context) };
    }
}
