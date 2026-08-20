#![allow(clippy::missing_safety_doc)]

use std::collections::VecDeque;
use std::env;
use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[allow(warnings, clippy::all)]
mod ffi;

const AUDIO_RATE: i32 = 48_000;
const AUDIO_CHANNELS: i32 = 2;
const AUDIO_BYTES_PER_FRAME: i64 = (size_of::<f32>() * AUDIO_CHANNELS as usize) as i64;
const AUDIO_QUEUE_TARGET_BYTES: i32 = AUDIO_RATE * AUDIO_BYTES_PER_FRAME as i32 * 150 / 1000;
const VIDEO_QUEUE_TARGET: usize = 16;
const VIDEO_QUEUE_MAX: usize = 24;
const VIDEO_PRESENTATION_LEAD: f64 = 0.012;
const SEEK_SECONDS: f64 = 10.0;
const TOP_BAR_HEIGHT_PIXELS: f32 = 42.0;
const SCRUBBER_HIT_HEIGHT_PIXELS: f32 = 42.0;
const SCRUBBER_MARGIN_PIXELS: f32 = 14.0;
const RESIZE_BORDER_LOGICAL: f32 = 10.0;
const AV_NOPTS_VALUE: i64 = i64::MIN;

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Eq, PartialEq)]
enum Action {
    CycleSubtitles,
    Quit,
    SeekBackward,
    SeekForward,
    ToggleFullscreen,
    ToggleInfo,
    TogglePause,
    ToggleSubtitles,
}

fn action_for_key(key: u32) -> Option<Action> {
    match key {
        ffi::UpKey_UP_KEY_Q => Some(Action::Quit),
        ffi::UpKey_UP_KEY_J => Some(Action::CycleSubtitles),
        ffi::UpKey_UP_KEY_LEFT => Some(Action::SeekBackward),
        ffi::UpKey_UP_KEY_RIGHT => Some(Action::SeekForward),
        ffi::UpKey_UP_KEY_F => Some(Action::ToggleFullscreen),
        ffi::UpKey_UP_KEY_I => Some(Action::ToggleInfo),
        ffi::UpKey_UP_KEY_SPACE => Some(Action::TogglePause),
        ffi::UpKey_UP_KEY_S => Some(Action::ToggleSubtitles),
        _ => None,
    }
}

unsafe fn sdl_error() -> String {
    let error = unsafe { ffi::up_platform_error() };
    if error.is_null() {
        "unknown SDL error".into()
    } else {
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

unsafe fn ffmpeg_error(code: i32) -> String {
    let mut buffer = [0_i8; 128];
    if unsafe { ffi::up_av_error_string(code, buffer.as_mut_ptr(), buffer.len()) } < 0 {
        return format!("FFmpeg error {code}");
    }
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

unsafe fn ffmpeg_name(pointer: *const std::ffi::c_char) -> Option<String> {
    (!pointer.is_null()).then(|| {
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .to_uppercase()
    })
}

fn format_bitrate(bits_per_second: i64) -> String {
    if bits_per_second >= 1_000_000 {
        format!("{:.1} MBPS", bits_per_second as f64 / 1_000_000.0)
    } else if bits_per_second >= 1_000 {
        format!("{:.0} KBPS", bits_per_second as f64 / 1_000.0)
    } else if bits_per_second > 0 {
        format!("{bits_per_second} BPS")
    } else {
        "UNKNOWN".to_owned()
    }
}

fn format_video_bitrate(declared: i64, metadata: Option<i64>, container: i64) -> String {
    if declared > 0 {
        format_bitrate(declared)
    } else if let Some(metadata) = metadata.filter(|value| *value > 0) {
        format_bitrate(metadata)
    } else if container > 0 {
        format!("{} (CONTAINER)", format_bitrate(container))
    } else {
        "UNKNOWN".to_owned()
    }
}

fn mark_assumed(name: Option<String>, assumed: bool) -> String {
    let name = name.unwrap_or_else(|| "UNKNOWN".to_owned());
    if assumed {
        format!("{name} (ASSUMED)")
    } else {
        name
    }
}

fn hdr_status(kind: ffi::UpHdrKind, assumed: bool) -> &'static str {
    match kind {
        ffi::UpHdrKind_UP_HDR_KIND_PQ => "YES (PQ)",
        ffi::UpHdrKind_UP_HDR_KIND_HLG => "YES (HLG)",
        ffi::UpHdrKind_UP_HDR_KIND_UNKNOWN => "UNKNOWN",
        ffi::UpHdrKind_UP_HDR_KIND_SDR if assumed => "NO (ASSUMED)",
        _ => "NO",
    }
}

fn close_button_contains(
    x: f32,
    y: f32,
    logical_width: i32,
    logical_height: i32,
    pixel_width: i32,
    pixel_height: i32,
) -> bool {
    if logical_width <= 0 || logical_height <= 0 || pixel_width <= 0 || pixel_height <= 0 {
        return false;
    }
    let button_width = TOP_BAR_HEIGHT_PIXELS * logical_width as f32 / pixel_width as f32;
    let button_height = TOP_BAR_HEIGHT_PIXELS * logical_height as f32 / pixel_height as f32;
    x >= logical_width as f32 - button_width
        && x < logical_width as f32
        && y >= 0.0
        && y < button_height
}

fn scrubber_target(
    x: f32,
    y: f32,
    logical_width: i32,
    logical_height: i32,
    pixel_width: i32,
    pixel_height: i32,
    duration: f64,
) -> Option<f64> {
    if logical_width <= 0
        || logical_height <= 0
        || pixel_width <= 0
        || pixel_height <= 0
        || !duration.is_finite()
        || duration <= 0.0
    {
        return None;
    }
    let hit_height = SCRUBBER_HIT_HEIGHT_PIXELS * logical_height as f32 / pixel_height as f32;
    if x <= RESIZE_BORDER_LOGICAL
        || x >= logical_width as f32 - RESIZE_BORDER_LOGICAL
        || y < logical_height as f32 - hit_height
        || y >= logical_height as f32 - RESIZE_BORDER_LOGICAL
    {
        return None;
    }
    let margin = SCRUBBER_MARGIN_PIXELS * logical_width as f32 / pixel_width as f32;
    let track_width = logical_width as f32 - 2.0 * margin;
    (track_width > 0.0).then(|| ((x - margin) / track_width).clamp(0.0, 1.0) as f64 * duration)
}

struct Sdl;

impl Sdl {
    unsafe fn init() -> Result<Self> {
        // This player deliberately has no X11 or non-PipeWire runtime path.
        unsafe {
            env::set_var("SDL_VIDEODRIVER", "wayland");
            env::set_var("SDL_AUDIODRIVER", "pipewire");
        }
        if unsafe { ffi::up_platform_init() } == 0 {
            return Err(format!("SDL initialization failed: {}", unsafe {
                sdl_error()
            }));
        }
        Ok(Self)
    }
}

impl Drop for Sdl {
    fn drop(&mut self) {
        unsafe { ffi::up_platform_quit() };
    }
}

struct Window(*mut ffi::UpWindow);

impl Window {
    unsafe fn create(title: &CStr) -> Result<Self> {
        let window = unsafe { ffi::up_window_create(title.as_ptr(), 1280, 720) };
        if window.is_null() {
            return Err(format!("could not create the Wayland window: {}", unsafe {
                sdl_error()
            }));
        }
        Ok(Self(window))
    }

    unsafe fn pixel_size(&self) -> Result<(i32, i32)> {
        let mut width = 0;
        let mut height = 0;
        if unsafe { ffi::up_window_pixel_size(self.0, &mut width, &mut height) } == 0 {
            return Err(format!("could not query window size: {}", unsafe {
                sdl_error()
            }));
        }
        Ok((width, height))
    }

    unsafe fn close_button_contains(&self, x: f32, y: f32) -> bool {
        let mut logical_width = 0;
        let mut logical_height = 0;
        let mut pixel_width = 0;
        let mut pixel_height = 0;
        if unsafe { ffi::up_window_size(self.0, &mut logical_width, &mut logical_height) } == 0
            || unsafe { ffi::up_window_pixel_size(self.0, &mut pixel_width, &mut pixel_height) }
                == 0
        {
            return false;
        }
        close_button_contains(
            x,
            y,
            logical_width,
            logical_height,
            pixel_width,
            pixel_height,
        )
    }

    unsafe fn scrubber_target(&self, x: f32, y: f32, duration: Option<f64>) -> Option<f64> {
        let duration = duration?;
        let mut logical_width = 0;
        let mut logical_height = 0;
        let mut pixel_width = 0;
        let mut pixel_height = 0;
        if unsafe { ffi::up_window_size(self.0, &mut logical_width, &mut logical_height) } == 0
            || unsafe { ffi::up_window_pixel_size(self.0, &mut pixel_width, &mut pixel_height) }
                == 0
        {
            return None;
        }
        scrubber_target(
            x,
            y,
            logical_width,
            logical_height,
            pixel_width,
            pixel_height,
            duration,
        )
    }

    unsafe fn set_minimum_size(&self) -> Result<()> {
        if unsafe { ffi::up_window_set_minimum_size(self.0, 320, 180) } == 0 {
            return Err(format!(
                "could not set the minimum window size: {}",
                unsafe { sdl_error() }
            ));
        }
        Ok(())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe { ffi::up_window_destroy(self.0) };
    }
}

struct WaylandInput(*mut ffi::UpWaylandInput);

impl WaylandInput {
    unsafe fn create(window: &Window) -> Result<Self> {
        let input = unsafe { ffi::up_wayland_input_create(window.0.cast()) };
        if input.is_null() {
            return Err("out of memory while creating Wayland input".into());
        }
        if !unsafe { ffi::up_wayland_input_ready(input) } {
            let message = unsafe { CStr::from_ptr(ffi::up_wayland_input_error(input)) }
                .to_string_lossy()
                .into_owned();
            unsafe { ffi::up_wayland_input_destroy(input) };
            return Err(message);
        }
        Ok(Self(input))
    }
}

impl Drop for WaylandInput {
    fn drop(&mut self) {
        unsafe { ffi::up_wayland_input_destroy(self.0) };
    }
}

enum MprisCommand {
    Quit,
    Play,
    Pause,
    PlayPause,
    Stop,
    Seek(i64),
    SetPosition(i64),
}

struct Mpris(*mut ffi::UpMpris);

impl Mpris {
    unsafe fn create(title: &CStr, path: &CStr, duration_us: i64) -> Option<Self> {
        let mpris = unsafe { ffi::up_mpris_create(title.as_ptr(), path.as_ptr(), duration_us) };
        if mpris.is_null() {
            eprintln!("warning: out of memory while enabling MPRIS");
            return None;
        }
        if unsafe { ffi::up_mpris_active(mpris) } == 0 {
            let error = unsafe { CStr::from_ptr(ffi::up_mpris_error(mpris)) }.to_string_lossy();
            eprintln!("warning: MPRIS unavailable: {error}");
            unsafe { ffi::up_mpris_destroy(mpris) };
            return None;
        }
        Some(Self(mpris))
    }

    fn dispatch(&self) {
        unsafe { ffi::up_mpris_dispatch(self.0) };
    }

    fn take_command(&self) -> Option<MprisCommand> {
        let mut value = 0;
        let command = unsafe { ffi::up_mpris_take_command(self.0, &mut value) };
        match command {
            ffi::UpMprisCommand_UP_MPRIS_COMMAND_QUIT => Some(MprisCommand::Quit),
            ffi::UpMprisCommand_UP_MPRIS_COMMAND_PLAY => Some(MprisCommand::Play),
            ffi::UpMprisCommand_UP_MPRIS_COMMAND_PAUSE => Some(MprisCommand::Pause),
            ffi::UpMprisCommand_UP_MPRIS_COMMAND_PLAY_PAUSE => Some(MprisCommand::PlayPause),
            ffi::UpMprisCommand_UP_MPRIS_COMMAND_STOP => Some(MprisCommand::Stop),
            ffi::UpMprisCommand_UP_MPRIS_COMMAND_SEEK => Some(MprisCommand::Seek(value)),
            ffi::UpMprisCommand_UP_MPRIS_COMMAND_SET_POSITION => {
                Some(MprisCommand::SetPosition(value))
            }
            _ => None,
        }
    }

    fn update(&self, status: ffi::UpMprisStatus, position_us: i64) {
        unsafe { ffi::up_mpris_update(self.0, status, position_us) };
    }

    fn seeked(&self, position_us: i64) {
        unsafe { ffi::up_mpris_seeked(self.0, position_us) };
    }
}

impl Drop for Mpris {
    fn drop(&mut self) {
        unsafe { ffi::up_mpris_destroy(self.0) };
    }
}

struct Renderer(*mut ffi::UpVideoRenderer);

struct RendererOverlays<'a> {
    info: Option<(&'a CStr, f32)>,
    details: Option<&'a CStr>,
    position: Option<(&'a CStr, f32)>,
    scrubber: Option<(f32, f32)>,
    subtitle: Option<&'a SubtitleCue>,
}

impl Renderer {
    unsafe fn create(window: &Window) -> Result<Self> {
        let renderer = unsafe { ffi::up_video_renderer_create(window.0.cast()) };
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
        Ok(Self(renderer))
    }

    unsafe fn device(&self) -> *mut c_void {
        unsafe { ffi::up_video_renderer_device(self.0) }
    }

    unsafe fn display(
        &self,
        frame: *mut ffi::UpAvFrame,
        width: i32,
        height: i32,
        top_bar_alpha: f32,
        title: &CStr,
        overlays: RendererOverlays<'_>,
    ) -> Result<()> {
        let (info, info_alpha) = overlays
            .info
            .map_or((ptr::null(), 0.0), |(text, alpha)| (text.as_ptr(), alpha));
        let details = overlays.details.map_or(ptr::null(), CStr::as_ptr);
        let (position, position_alpha) = overlays
            .position
            .map_or((ptr::null(), 0.0), |(text, alpha)| (text.as_ptr(), alpha));
        let (scrubber_progress, scrubber_alpha) = overlays.scrubber.unwrap_or((-1.0, 0.0));
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
                self.0,
                frame.cast(),
                width,
                height,
                top_bar_alpha,
                title.as_ptr(),
                info,
                info_alpha,
                details,
                position,
                position_alpha,
                scrubber_progress,
                scrubber_alpha,
                subtitle_text,
                subtitle_pixels,
                subtitle_width,
                subtitle_height,
                subtitle_serial,
            )
        } < 0
        {
            return Err(
                unsafe { CStr::from_ptr(ffi::up_video_renderer_error(self.0)) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        Ok(())
    }

    unsafe fn resize(&self, width: i32, height: i32) -> Result<()> {
        if unsafe { ffi::up_video_renderer_resize(self.0, width, height) } < 0 {
            return Err(
                unsafe { CStr::from_ptr(ffi::up_video_renderer_error(self.0)) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        Ok(())
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe { ffi::up_video_renderer_destroy(self.0) };
    }
}

struct Decoder {
    context: *mut ffi::UpAvDecoder,
    stream_index: i32,
    time_base: f64,
    uses_vulkan: bool,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { ffi::up_av_decoder_free(&mut self.context) };
    }
}

impl Decoder {
    unsafe fn open(
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
            uses_vulkan: unsafe { ffi::up_av_decoder_uses_vulkan(context) } != 0,
        })
    }
}

struct VideoFrame {
    frame: *mut ffi::UpAvFrame,
    pts: f64,
    duration: f64,
}

enum SubtitleContent {
    Clear,
    Text(String),
    Bitmap {
        width: i32,
        height: i32,
        pixels: Vec<u8>,
    },
}

struct SubtitleCue {
    track: usize,
    start: f64,
    end: f64,
    serial: u64,
    content: SubtitleContent,
}

struct VideoInfo {
    lines: [String; 10],
    frame_rate: Option<f64>,
}

impl VideoInfo {
    unsafe fn inspect(media: &Media, frame: *const ffi::UpAvFrame) -> Self {
        let mut info: ffi::UpVideoInfo = unsafe { std::mem::zeroed() };
        assert_ne!(
            unsafe { ffi::up_av_video_info(media.format, media.video.context, frame, &mut info) },
            0,
            "opened video has inspectable stream information"
        );
        let codec = unsafe { ffmpeg_name(info.codec) }.unwrap_or_else(|| "UNKNOWN".to_owned());
        let profile = unsafe { ffmpeg_name(info.profile) };
        let codec = profile.map_or(codec.clone(), |profile| format!("{codec} {profile}"));

        let frame_rate =
            (info.frame_rate.is_finite() && info.frame_rate > 0.0).then_some(info.frame_rate);
        let bit_rate = format_video_bitrate(
            info.declared_bitrate,
            (info.metadata_bitrate > 0).then_some(info.metadata_bitrate),
            info.container_bitrate,
        );
        let pixel_format = unsafe { ffmpeg_name(info.pixel_format) }
            .unwrap_or_else(|| "UNKNOWN PIXEL FORMAT".to_owned());
        let decode_path = if media.video.uses_vulkan {
            "VULKAN HW"
        } else {
            "SOFTWARE"
        };
        let resolution_line = format!("RESOLUTION: {}X{}", info.width, info.height);
        let color_space = mark_assumed(
            unsafe { ffmpeg_name(info.color_space) },
            info.color_space_assumed != 0,
        );
        let color_primaries = mark_assumed(
            unsafe { ffmpeg_name(info.color_primaries) },
            info.color_primaries_assumed != 0,
        );
        let color_transfer = mark_assumed(
            unsafe { ffmpeg_name(info.color_transfer) },
            info.color_transfer_assumed != 0,
        );
        let color_range = mark_assumed(
            unsafe { ffmpeg_name(info.color_range) },
            info.color_range_assumed != 0,
        );
        let hdr = hdr_status(info.hdr_kind, info.color_transfer_assumed != 0);
        Self {
            lines: [
                format!("CODEC: {codec}"),
                resolution_line,
                format!("BITRATE: {bit_rate}"),
                format!("PIXEL FORMAT: {pixel_format}"),
                format!("DECODE: {decode_path}"),
                format!("MATRIX: {color_space}"),
                format!("PRIMARIES: {color_primaries}"),
                format!("TRANSFER: {color_transfer}"),
                format!("RANGE: {color_range}"),
                format!("HDR: {hdr}"),
            ],
            frame_rate,
        }
    }

    fn overlay_text(&self) -> CString {
        CString::new(self.lines.join("\n")).expect("video information has no NUL bytes")
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

struct AudioOutput {
    stream: *mut ffi::UpAudioStream,
    converter: *mut ffi::UpAvAudioConverter,
    first_pts: Option<f64>,
    submitted_frames: i64,
    resumed: bool,
}

impl AudioOutput {
    unsafe fn create() -> Result<Self> {
        let stream = unsafe { ffi::up_audio_stream_create(AUDIO_RATE, AUDIO_CHANNELS) };
        if stream.is_null() {
            return Err(format!("could not open PipeWire audio: {}", unsafe {
                sdl_error()
            }));
        }

        Ok(Self {
            stream,
            converter: ptr::null_mut(),
            first_pts: None,
            submitted_frames: 0,
            resumed: false,
        })
    }

    unsafe fn initialize_converter(&mut self, frame: *const ffi::UpAvFrame) -> Result<()> {
        if !self.converter.is_null() {
            return Ok(());
        }
        let mut error = 0;
        self.converter = unsafe {
            ffi::up_av_audio_converter_create(frame, AUDIO_RATE, AUDIO_CHANNELS, &mut error)
        };
        if self.converter.is_null() {
            return Err(format!(
                "could not configure audio conversion: {}",
                unsafe { ffmpeg_error(error) }
            ));
        }
        Ok(())
    }

    unsafe fn push(
        &mut self,
        frame: *const ffi::UpAvFrame,
        time_base: f64,
        discard_before: Option<f64>,
    ) -> Result<bool> {
        unsafe { self.initialize_converter(frame)? };

        let timestamp = unsafe { ffi::up_av_frame_timestamp(frame) };
        let frame_pts = (timestamp != AV_NOPTS_VALUE).then_some(timestamp as f64 * time_base);

        let capacity = unsafe { ffi::up_av_audio_converter_capacity(self.converter, frame) };
        if capacity < 0 {
            return Err("could not calculate converted audio size".into());
        }
        let mut samples = vec![0_f32; capacity as usize * AUDIO_CHANNELS as usize];
        let converted = unsafe {
            ffi::up_av_audio_converter_convert(
                self.converter,
                frame,
                samples.as_mut_ptr(),
                capacity,
            )
        };
        if converted < 0 {
            return Err(format!("audio conversion failed: {}", unsafe {
                ffmpeg_error(converted)
            }));
        }
        let skipped = discard_before
            .zip(frame_pts)
            .map_or(0, |(target, pts)| {
                ((target - pts).max(0.0) * AUDIO_RATE as f64).ceil() as i32
            })
            .min(converted);
        let queued = converted - skipped;
        if self.first_pts.is_none() && queued > 0 {
            self.first_pts = Some(frame_pts.unwrap_or(0.0) + skipped as f64 / AUDIO_RATE as f64);
        }
        let bytes = queued as usize * AUDIO_BYTES_PER_FRAME as usize;
        if bytes > 0
            && unsafe {
                ffi::up_audio_stream_put(
                    self.stream,
                    samples
                        .as_ptr()
                        .add(skipped as usize * AUDIO_CHANNELS as usize)
                        .cast::<c_void>(),
                    bytes as i32,
                )
            } == 0
        {
            return Err(format!("could not queue audio: {}", unsafe { sdl_error() }));
        }
        self.submitted_frames += queued as i64;
        Ok(queued > 0)
    }

    unsafe fn queued_bytes(&self) -> i32 {
        unsafe { ffi::up_audio_stream_queued(self.stream) }.max(0)
    }

    unsafe fn clock(&self) -> Option<f64> {
        let base = self.first_pts?;
        let queued_frames = unsafe { self.queued_bytes() } as i64 / AUDIO_BYTES_PER_FRAME;
        Some(base + (self.submitted_frames - queued_frames) as f64 / AUDIO_RATE as f64)
    }

    unsafe fn resume(&mut self) -> Result<()> {
        if !self.resumed {
            if unsafe { ffi::up_audio_stream_resume(self.stream) } == 0 {
                return Err(format!("could not start audio: {}", unsafe { sdl_error() }));
            }
            self.resumed = true;
        }
        Ok(())
    }

    unsafe fn set_paused(&self, paused: bool) -> Result<()> {
        let ok = if paused {
            unsafe { ffi::up_audio_stream_pause(self.stream) }
        } else {
            unsafe { ffi::up_audio_stream_resume(self.stream) }
        };
        if ok == 0 {
            return Err(format!("could not change audio pause state: {}", unsafe {
                sdl_error()
            }));
        }
        Ok(())
    }

    unsafe fn reset(&mut self) -> Result<()> {
        if unsafe { ffi::up_audio_stream_clear(self.stream) } == 0 {
            return Err(format!("could not clear queued audio: {}", unsafe {
                sdl_error()
            }));
        }
        unsafe { ffi::up_av_audio_converter_free(&mut self.converter) };
        self.first_pts = None;
        self.submitted_frames = 0;
        Ok(())
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        unsafe {
            ffi::up_av_audio_converter_free(&mut self.converter);
            ffi::up_audio_stream_destroy(self.stream);
        }
    }
}

struct Media {
    format: *mut ffi::UpAvFormat,
    packet: *mut ffi::UpAvPacket,
    video: Decoder,
    audio_decoder: Option<Decoder>,
    audio: Option<AudioOutput>,
    subtitle_decoders: Vec<Decoder>,
    video_queue: VecDeque<VideoFrame>,
    subtitle_queue: VecDeque<SubtitleCue>,
    subtitle_serial: u64,
    log_subtitles: bool,
    eof: bool,
    drained: bool,
    first_video_pts: Option<f64>,
    video_seek_target: Option<f64>,
    audio_seek_target: Option<f64>,
    subtitle_seek_target: Option<f64>,
}

impl Media {
    unsafe fn open(path: &Path, vulkan_device: *mut c_void, log_subtitles: bool) -> Result<Self> {
        let path = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| "media path contains a NUL byte".to_string())?;
        let mut format = ptr::null_mut();
        let ret = unsafe { ffi::up_av_format_open(&mut format, path.as_ptr()) };
        if ret < 0 {
            return Err(format!("could not open media: {}", unsafe {
                ffmpeg_error(ret)
            }));
        }

        let result = (|| {
            let ret = unsafe { ffi::up_av_format_find_stream_info(format) };
            if ret < 0 {
                return Err(format!("could not inspect media streams: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }

            let video_index = unsafe {
                ffi::up_av_find_best_stream(format, ffi::UpMediaType_UP_MEDIA_TYPE_VIDEO, -1)
            };
            if video_index < 0 {
                return Err("the input has no video stream".into());
            }
            let video = unsafe { Decoder::open(format, video_index, Some(vulkan_device))? };
            let video_name =
                unsafe { CStr::from_ptr(ffi::up_av_stream_codec_name(format, video_index as u32)) }
                    .to_string_lossy();
            let decode_path = if video.uses_vulkan {
                "Vulkan Video"
            } else {
                "software decode, Vulkan presentation"
            };
            eprintln!(
                "video: {video_name} {}x{} via {decode_path}",
                unsafe { ffi::up_av_decoder_width(video.context) },
                unsafe { ffi::up_av_decoder_height(video.context) }
            );

            let audio_index = unsafe {
                ffi::up_av_find_best_stream(
                    format,
                    ffi::UpMediaType_UP_MEDIA_TYPE_AUDIO,
                    video_index,
                )
            };
            let (audio_decoder, audio) = if audio_index >= 0 {
                let audio_name = unsafe {
                    CStr::from_ptr(ffi::up_av_stream_codec_name(format, audio_index as u32))
                }
                .to_string_lossy();
                eprintln!("audio: {audio_name} via PipeWire");
                (
                    Some(unsafe { Decoder::open(format, audio_index, None)? }),
                    Some(unsafe { AudioOutput::create()? }),
                )
            } else {
                (None, None)
            };

            let mut subtitle_indices = (0..unsafe { ffi::up_av_stream_count(format) } as usize)
                .filter(|&index| unsafe {
                    ffi::up_av_stream_type(format, index as u32)
                        == ffi::UpMediaType_UP_MEDIA_TYPE_SUBTITLE
                })
                .collect::<Vec<_>>();
            subtitle_indices.sort_by_key(|&index| unsafe {
                ffi::up_av_stream_is_default(format, index as u32) == 0
            });
            let mut subtitle_decoders = Vec::new();
            for stream_index in subtitle_indices {
                let subtitle_name = unsafe {
                    CStr::from_ptr(ffi::up_av_stream_codec_name(format, stream_index as u32))
                }
                .to_string_lossy();
                match unsafe { Decoder::open(format, stream_index as i32, None) } {
                    Ok(decoder) => {
                        eprintln!(
                            "subtitle track {}: {subtitle_name}",
                            subtitle_decoders.len() + 1
                        );
                        subtitle_decoders.push(decoder);
                    }
                    Err(error) => eprintln!(
                        "subtitle stream {stream_index} ({subtitle_name}) unavailable: {error}"
                    ),
                }
            }
            if !subtitle_decoders.is_empty() {
                eprintln!("subtitles: S toggles, J switches tracks");
            }

            let packet = unsafe { ffi::up_av_packet_alloc() };
            if packet.is_null() {
                return Err("out of memory while allocating a packet".into());
            }

            Ok(Self {
                format,
                packet,
                video,
                audio_decoder,
                audio,
                subtitle_decoders,
                video_queue: VecDeque::new(),
                subtitle_queue: VecDeque::new(),
                subtitle_serial: 0,
                log_subtitles,
                eof: false,
                drained: false,
                first_video_pts: None,
                video_seek_target: None,
                audio_seek_target: None,
                subtitle_seek_target: None,
            })
        })();

        if result.is_err() {
            unsafe { ffi::up_av_format_close(&mut format) };
        }
        result
    }

    unsafe fn receive_video(&mut self) -> Result<()> {
        loop {
            let mut frame = ptr::null_mut();
            let ret = unsafe { ffi::up_av_decoder_receive_frame(self.video.context, &mut frame) };
            if ret < 0 {
                break;
            }
            if self.video.uses_vulkan && unsafe { ffi::up_av_frame_is_vulkan(frame) } == 0 {
                unsafe { ffi::up_av_frame_free(&mut frame) };
                return Err("the selected codec/profile is not supported by Vulkan Video".into());
            }
            let timestamp = unsafe { ffi::up_av_frame_timestamp(frame) };
            let pts = if timestamp == AV_NOPTS_VALUE {
                self.video_queue.back().map_or(0.0, |previous| {
                    previous.pts + previous.duration.max(1.0 / 60.0)
                })
            } else {
                timestamp as f64 * self.video.time_base
            };
            let raw_duration = unsafe { ffi::up_av_frame_duration(frame) };
            let duration = if raw_duration > 0 {
                raw_duration as f64 * self.video.time_base
            } else {
                0.0
            };
            if self
                .video_seek_target
                .is_some_and(|target| pts + duration.max(1.0 / 120.0) < target)
            {
                unsafe { ffi::up_av_frame_free(&mut frame) };
                continue;
            }
            self.video_seek_target = None;
            self.first_video_pts.get_or_insert(pts);
            self.video_queue.push_back(VideoFrame {
                frame,
                pts,
                duration,
            });
        }
        Ok(())
    }

    unsafe fn receive_audio(&mut self) -> Result<()> {
        let Some(decoder) = self.audio_decoder.as_ref() else {
            return Ok(());
        };
        let context = decoder.context;
        let time_base = decoder.time_base;
        loop {
            let mut frame = ptr::null_mut();
            let ret = unsafe { ffi::up_av_decoder_receive_frame(context, &mut frame) };
            if ret < 0 {
                break;
            }
            if let Some(audio) = self.audio.as_mut() {
                let result = unsafe { audio.push(frame, time_base, self.audio_seek_target) };
                unsafe { ffi::up_av_frame_free(&mut frame) };
                if result? {
                    self.audio_seek_target = None;
                }
            } else {
                unsafe { ffi::up_av_frame_free(&mut frame) };
            }
        }
        Ok(())
    }

    unsafe fn decode_subtitle_packet(&mut self, track: usize) -> Result<()> {
        let decoder = &self.subtitle_decoders[track];
        let context = decoder.context;
        let time_base = decoder.time_base;
        let mut ret = 0;
        let mut subtitle = unsafe { ffi::up_av_decode_subtitle(context, self.packet, &mut ret) };
        if ret < 0 {
            return Err(format!("subtitle decoder rejected a packet: {}", unsafe {
                ffmpeg_error(ret)
            }));
        }
        if subtitle.is_null() {
            return Ok(());
        }
        let packet_pts = unsafe { ffi::up_av_packet_pts(self.packet) };
        let packet_duration = unsafe { ffi::up_av_packet_duration(self.packet) };
        let mut subtitle_info: ffi::UpSubtitleInfo = unsafe { std::mem::zeroed() };
        unsafe { ffi::up_av_subtitle_info(subtitle, &mut subtitle_info) };

        let cue = {
            let base_pts = if subtitle_info.pts != AV_NOPTS_VALUE {
                subtitle_info.pts as f64 / 1_000_000.0
            } else if packet_pts != AV_NOPTS_VALUE {
                packet_pts as f64 * time_base
            } else {
                0.0
            };
            let start = base_pts + subtitle_info.start_display_time as f64 / 1000.0;
            let end = if subtitle_info.end_display_time > subtitle_info.start_display_time {
                base_pts + subtitle_info.end_display_time as f64 / 1000.0
            } else if packet_duration > 0 {
                start + packet_duration as f64 * time_base
            } else {
                f64::INFINITY
            };

            let video_width = unsafe { ffi::up_av_decoder_width(self.video.context) };
            let video_height = unsafe { ffi::up_av_decoder_height(self.video.context) };
            let subtitle_width = unsafe { ffi::up_av_decoder_width(context) };
            let subtitle_height = unsafe { ffi::up_av_decoder_height(context) };
            let canvas_width = if subtitle_width > 0 {
                subtitle_width
            } else {
                video_width
            };
            let canvas_height = if subtitle_height > 0 {
                subtitle_height
            } else {
                video_height
            };
            let pixel_count = usize::try_from(canvas_width)
                .ok()
                .and_then(|width| {
                    usize::try_from(canvas_height)
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .and_then(|pixels| pixels.checked_mul(4))
                .filter(|bytes| *bytes <= 512 * 1024 * 1024);
            let mut bitmap = pixel_count.map(|bytes| vec![0_u8; bytes]);
            let mut has_bitmap = false;
            let mut text = Vec::new();

            for index in 0..subtitle_info.rect_count {
                let mut rect: ffi::UpSubtitleRectView = unsafe { std::mem::zeroed() };
                if unsafe { ffi::up_av_subtitle_rect(subtitle, index, &mut rect) } == 0 {
                    continue;
                }
                match rect.type_ {
                    ffi::UpSubtitleRectType_UP_SUBTITLE_RECT_BITMAP => {
                        let Some(pixels) = bitmap.as_mut() else {
                            continue;
                        };
                        if rect.width <= 0
                            || rect.height <= 0
                            || rect.line_size <= 0
                            || rect.pixels.is_null()
                            || rect.palette.is_null()
                        {
                            continue;
                        }
                        let x0 = rect.x.clamp(0, canvas_width);
                        let y0 = rect.y.clamp(0, canvas_height);
                        let x1 = rect.x.saturating_add(rect.width).clamp(0, canvas_width);
                        let y1 = rect.y.saturating_add(rect.height).clamp(0, canvas_height);
                        for y in y0..y1 {
                            let source_y = y - rect.y;
                            let source = unsafe {
                                rect.pixels.add(source_y as usize * rect.line_size as usize)
                            };
                            for x in x0..x1 {
                                let palette_index = unsafe { *source.add((x - rect.x) as usize) };
                                if palette_index as i32 >= rect.color_count {
                                    continue;
                                }
                                let color = unsafe {
                                    ptr::read_unaligned(
                                        rect.palette.cast::<u32>().add(palette_index as usize),
                                    )
                                };
                                let destination =
                                    (y as usize * canvas_width as usize + x as usize) * 4;
                                pixels[destination] = (color >> 16) as u8;
                                pixels[destination + 1] = (color >> 8) as u8;
                                pixels[destination + 2] = color as u8;
                                pixels[destination + 3] = (color >> 24) as u8;
                            }
                        }
                        has_bitmap = true;
                    }
                    ffi::UpSubtitleRectType_UP_SUBTITLE_RECT_TEXT
                    | ffi::UpSubtitleRectType_UP_SUBTITLE_RECT_ASS => {
                        let ass = rect.type_ == ffi::UpSubtitleRectType_UP_SUBTITLE_RECT_ASS;
                        if !rect.text.is_null() {
                            let value = unsafe { CStr::from_ptr(rect.text) }.to_string_lossy();
                            let value = subtitle_dialogue_text(&value, ass);
                            if !value.is_empty() {
                                text.push(value);
                            }
                        }
                    }
                    _ => {}
                }
            }

            let content = if has_bitmap {
                SubtitleContent::Bitmap {
                    width: canvas_width,
                    height: canvas_height,
                    pixels: bitmap.expect("bitmap storage was allocated"),
                }
            } else if !text.is_empty() {
                SubtitleContent::Text(text.join("\n"))
            } else {
                SubtitleContent::Clear
            };
            self.subtitle_serial = self.subtitle_serial.wrapping_add(1).max(1);
            SubtitleCue {
                track,
                start,
                end,
                serial: self.subtitle_serial,
                content,
            }
        };
        unsafe { ffi::up_av_subtitle_free(&mut subtitle) };
        if self
            .subtitle_seek_target
            .is_some_and(|target| cue.end <= target)
        {
            return Ok(());
        }
        if self.log_subtitles {
            let kind = match &cue.content {
                SubtitleContent::Clear => "clear".to_owned(),
                SubtitleContent::Text(text) => format!("text {} chars", text.len()),
                SubtitleContent::Bitmap { width, height, .. } => {
                    format!("bitmap {width}x{height}")
                }
            };
            eprintln!(
                "subtitle track {} cue: {:.3}-{:.3} {kind}",
                cue.track + 1,
                cue.start,
                cue.end
            );
        }
        self.subtitle_queue.push_back(cue);
        Ok(())
    }

    unsafe fn decode_packet(&mut self) -> Result<()> {
        let stream_index = unsafe { ffi::up_av_packet_stream_index(self.packet) };
        if stream_index == self.video.stream_index {
            let ret = unsafe { ffi::up_av_decoder_send_packet(self.video.context, self.packet) };
            if ret < 0 {
                return Err(format!("video decoder rejected a packet: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }
            unsafe { self.receive_video()? };
        } else if self
            .audio_decoder
            .as_ref()
            .is_some_and(|decoder| decoder.stream_index == stream_index)
        {
            let context = self.audio_decoder.as_ref().unwrap().context;
            let ret = unsafe { ffi::up_av_decoder_send_packet(context, self.packet) };
            if ret < 0 {
                return Err(format!("audio decoder rejected a packet: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }
            unsafe { self.receive_audio()? };
        } else if let Some(track) = self
            .subtitle_decoders
            .iter()
            .position(|decoder| decoder.stream_index == stream_index)
        {
            unsafe { self.decode_subtitle_packet(track)? };
        }
        Ok(())
    }

    unsafe fn drain(&mut self) -> Result<()> {
        if self.drained {
            return Ok(());
        }
        self.drained = true;
        unsafe {
            ffi::up_av_decoder_send_packet(self.video.context, ptr::null());
            self.receive_video()?;
        }
        if let Some(decoder) = self.audio_decoder.as_ref() {
            unsafe {
                ffi::up_av_decoder_send_packet(decoder.context, ptr::null());
                self.receive_audio()?;
            }
        }
        Ok(())
    }

    unsafe fn fill_queues(&mut self) -> Result<()> {
        if self.eof {
            return unsafe { self.drain() };
        }

        for _ in 0..96 {
            let audio_needs_data = self
                .audio
                .as_ref()
                .is_some_and(|audio| unsafe { audio.queued_bytes() } < AUDIO_QUEUE_TARGET_BYTES);
            if self.video_queue.len() >= VIDEO_QUEUE_MAX
                || (self.video_queue.len() >= VIDEO_QUEUE_TARGET && !audio_needs_data)
            {
                break;
            }

            let ret = unsafe { ffi::up_av_read_frame(self.format, self.packet) };
            if ret < 0 {
                self.eof = true;
                unsafe { self.drain()? };
                break;
            }
            let decode = unsafe { self.decode_packet() };
            unsafe { ffi::up_av_packet_unref(self.packet) };
            decode?;
        }
        Ok(())
    }

    unsafe fn start_audio(&mut self) -> Result<()> {
        if let Some(audio) = self.audio.as_mut() {
            unsafe { audio.resume()? };
        }
        Ok(())
    }

    unsafe fn set_paused(&self, paused: bool) -> Result<()> {
        if let Some(audio) = self.audio.as_ref() {
            unsafe { audio.set_paused(paused)? };
        }
        Ok(())
    }

    fn duration(&self) -> Option<f64> {
        let duration = unsafe { ffi::up_av_format_duration(self.format) };
        (duration.is_finite() && duration > 0.0).then_some(duration)
    }

    unsafe fn nearest_keyframe(&self, target: f64) -> Option<f64> {
        if self.video.time_base <= 0.0 {
            return None;
        }
        let entry_time = |backward| {
            let mut entry = 0.0;
            (unsafe {
                ffi::up_av_index_entry_time(
                    self.format,
                    self.video.stream_index,
                    target,
                    i32::from(backward),
                    &mut entry,
                )
            } != 0)
                .then_some(entry)
        };
        closest_seek_point(target, entry_time(true), entry_time(false))
    }

    unsafe fn seek(&mut self, requested_target: f64) -> Result<f64> {
        if self.video.time_base <= 0.0 {
            return Err("video stream has an invalid time base".into());
        }
        // Keyframe seeking avoids decoding an entire GOP before presenting a
        // new position. That matters for 8K60 AV1, where decoding is already
        // close to real time and keyframes can be several seconds apart.
        let target = unsafe { self.nearest_keyframe(requested_target) }.unwrap_or(requested_target);
        let ret = unsafe { ffi::up_av_seek(self.format, self.video.stream_index, target) };
        if ret < 0 {
            return Err(format!("could not seek: {}", unsafe { ffmpeg_error(ret) }));
        }

        unsafe {
            ffi::up_av_packet_unref(self.packet);
            ffi::up_av_decoder_flush(self.video.context);
        }
        if let Some(decoder) = self.audio_decoder.as_ref() {
            unsafe { ffi::up_av_decoder_flush(decoder.context) };
        }
        for decoder in &self.subtitle_decoders {
            unsafe { ffi::up_av_decoder_flush(decoder.context) };
        }
        if let Some(audio) = self.audio.as_mut() {
            unsafe { audio.reset()? };
        }
        self.video_queue.clear();
        self.subtitle_queue.clear();
        self.eof = false;
        self.drained = false;
        self.video_seek_target = Some(target);
        self.audio_seek_target = self.audio.as_ref().map(|_| target);
        self.subtitle_seek_target = (!self.subtitle_decoders.is_empty()).then_some(target);
        Ok(target)
    }

    unsafe fn audio_clock(&self) -> Option<f64> {
        self.audio
            .as_ref()
            .and_then(|audio| unsafe { audio.clock() })
    }

    unsafe fn audio_empty(&self) -> bool {
        self.audio
            .as_ref()
            .is_none_or(|audio| unsafe { audio.queued_bytes() } == 0)
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        unsafe {
            ffi::up_av_packet_free(&mut self.packet);
            ffi::up_av_format_close(&mut self.format);
        }
    }
}

// All FFmpeg, resampler, and SDL audio state is exclusively accessed while
// holding DecodeWorker's mutex. Vulkan queue access is synchronized by the
// lock callbacks installed on the shared FFmpeg/libplacebo Vulkan device.
unsafe impl Send for Media {}

struct DecodeWorker {
    media: Arc<Mutex<Media>>,
    running: Arc<AtomicBool>,
    fill_nanoseconds: Arc<AtomicU64>,
    outstanding_frames: Arc<AtomicUsize>,
    frames: mpsc::Receiver<QueuedVideoFrame>,
    errors: mpsc::Receiver<String>,
    thread: Option<JoinHandle<()>>,
}

struct QueuedVideoFrame {
    frame: VideoFrame,
    outstanding_frames: Arc<AtomicUsize>,
}

impl Drop for QueuedVideoFrame {
    fn drop(&mut self) {
        self.outstanding_frames.fetch_sub(1, Ordering::Release);
    }
}

impl DecodeWorker {
    fn start(media: Media, measure_performance: bool) -> Self {
        let media = Arc::new(Mutex::new(media));
        let running = Arc::new(AtomicBool::new(true));
        let fill_nanoseconds = Arc::new(AtomicU64::new(0));
        let outstanding_frames = Arc::new(AtomicUsize::new(0));
        let (frame_sender, frames) = mpsc::channel();
        let (error_sender, errors) = mpsc::channel();
        let thread_media = Arc::clone(&media);
        let thread_running = Arc::clone(&running);
        let thread_fill_nanoseconds = Arc::clone(&fill_nanoseconds);
        let thread_outstanding_frames = Arc::clone(&outstanding_frames);
        let thread = thread::spawn(move || {
            while thread_running.load(Ordering::Acquire) {
                let started = Instant::now();
                let result = match thread_media.lock() {
                    Ok(mut media) => {
                        let result = unsafe { media.fill_queues() };
                        while result.is_ok()
                            && thread_outstanding_frames.load(Ordering::Acquire) < VIDEO_QUEUE_MAX
                        {
                            let Some(frame) = media.video_queue.pop_front() else {
                                break;
                            };
                            thread_outstanding_frames.fetch_add(1, Ordering::Release);
                            if frame_sender
                                .send(QueuedVideoFrame {
                                    frame,
                                    outstanding_frames: Arc::clone(&thread_outstanding_frames),
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        result
                    }
                    Err(_) => Err("decoder state lock was poisoned".into()),
                };
                if measure_performance {
                    let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                    thread_fill_nanoseconds.fetch_add(elapsed, Ordering::Relaxed);
                }
                if let Err(error) = result {
                    let _ = error_sender.send(error);
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        Self {
            media,
            running,
            fill_nanoseconds,
            outstanding_frames,
            frames,
            errors,
            thread: Some(thread),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, Media>> {
        self.media
            .lock()
            .map_err(|_| "decoder state lock was poisoned".into())
    }

    fn try_lock(&self) -> Result<Option<MutexGuard<'_, Media>>> {
        match self.media.try_lock() {
            Ok(media) => Ok(Some(media)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Poisoned(_)) => Err("decoder state lock was poisoned".into()),
        }
    }

    fn check_error(&self) -> Result<()> {
        match self.errors.try_recv() {
            Ok(error) => Err(error),
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => Ok(()),
        }
    }

    fn receive_frames(&self, queue: &mut VecDeque<QueuedVideoFrame>) {
        while queue.len() < VIDEO_QUEUE_MAX {
            match self.frames.try_recv() {
                Ok(frame) => queue.push_back(frame),
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    fn receive_subtitles(&self, queue: &mut VecDeque<SubtitleCue>) -> Result<()> {
        if let Some(mut media) = self.try_lock()? {
            queue.append(&mut media.subtitle_queue);
        }
        Ok(())
    }

    fn clear_frames(
        &self,
        queue: &mut VecDeque<QueuedVideoFrame>,
        current: &mut Option<QueuedVideoFrame>,
    ) {
        queue.clear();
        *current = None;
        while self.frames.try_recv().is_ok() {}
    }

    fn clear_subtitles(
        &self,
        incoming: &mut VecDeque<SubtitleCue>,
        queues: &mut [VecDeque<SubtitleCue>],
        current: &mut [Option<SubtitleCue>],
    ) {
        incoming.clear();
        for queue in queues {
            queue.clear();
        }
        for subtitle in current {
            *subtitle = None;
        }
    }

    fn take_fill_time(&self) -> Duration {
        Duration::from_nanos(self.fill_nanoseconds.swap(0, Ordering::Relaxed))
    }
}

impl Drop for DecodeWorker {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        while self.frames.try_recv().is_ok() {}
        debug_assert_eq!(self.outstanding_frames.load(Ordering::Acquire), 0);
    }
}

struct WallClock {
    origin_pts: f64,
    started: Instant,
    paused_at: Option<Instant>,
    paused_duration: Duration,
}

impl WallClock {
    fn new(origin_pts: f64) -> Self {
        Self {
            origin_pts,
            started: Instant::now(),
            paused_at: None,
            paused_duration: Duration::ZERO,
        }
    }

    fn now(&self) -> f64 {
        let end = self.paused_at.unwrap_or_else(Instant::now);
        self.origin_pts + (end - self.started - self.paused_duration).as_secs_f64()
    }

    fn set_paused(&mut self, paused: bool) {
        match (paused, self.paused_at) {
            (true, None) => self.paused_at = Some(Instant::now()),
            (false, Some(started)) => {
                self.paused_duration += Instant::now() - started;
                self.paused_at = None;
            }
            _ => {}
        }
    }

    fn seek(&mut self, pts: f64) {
        let now = Instant::now();
        self.origin_pts = pts;
        self.started = now;
        self.paused_duration = Duration::ZERO;
        if self.paused_at.is_some() {
            self.paused_at = Some(now);
        }
    }
}

fn format_position(seconds: f64) -> String {
    let total = seconds.max(0.0).floor() as u64;
    let hours = total / 3600;
    let minutes = total / 60 % 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn timeline_text(position: f64, duration: Option<f64>) -> CString {
    let position = format_position(position);
    let text = duration.map_or(position.clone(), |duration| {
        format!("{position} / {}", format_position(duration))
    });
    CString::new(text).expect("position text has no NUL bytes")
}

fn subtitle_dialogue_text(raw: &str, ass: bool) -> String {
    let dialogue = if ass {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("Dialogue:") {
            trimmed.splitn(10, ',').nth(9).unwrap_or(trimmed)
        } else {
            trimmed.splitn(9, ',').nth(8).unwrap_or(trimmed)
        }
    } else {
        raw
    };
    let mut text = String::with_capacity(dialogue.len());
    let mut in_override = false;
    let mut characters = dialogue.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '{' => in_override = true,
            '}' if in_override => in_override = false,
            '\\' if !in_override => match characters.next() {
                Some('N' | 'n') => text.push('\n'),
                Some('h') => text.push(' '),
                Some(other) => {
                    text.push('\\');
                    text.push(other);
                }
                None => text.push('\\'),
            },
            '\0' => {}
            '’' | '‘' => text.push('\''),
            '“' | '”' => text.push('"'),
            '–' | '—' => text.push('-'),
            _ if !in_override && character.is_whitespace() => text.push(' '),
            _ if !in_override && !character.is_control() => text.push(character),
            _ => {}
        }
    }
    text.trim().to_owned()
}

fn closest_seek_point(target: f64, before: Option<f64>, after: Option<f64>) -> Option<f64> {
    match (before, after) {
        (Some(before), Some(after)) => Some(if target - before <= after - target {
            before
        } else {
            after
        }),
        (Some(before), None) => Some(before),
        (None, Some(after)) => Some(after),
        (None, None) => None,
    }
}

struct PositionNotice {
    text: Option<CString>,
    shown_at: Instant,
    alpha: f32,
}

impl PositionNotice {
    fn new() -> Self {
        Self {
            text: None,
            shown_at: Instant::now(),
            alpha: 0.0,
        }
    }

    fn show(&mut self, position: f64, duration: Option<f64>) {
        self.show_text(timeline_text(position, duration));
    }

    fn show_text(&mut self, text: CString) {
        self.text = Some(text);
        self.shown_at = Instant::now();
        self.alpha = 1.0;
    }

    fn update(&mut self) -> bool {
        if self.text.is_none() {
            return false;
        }
        let elapsed = self.shown_at.elapsed().as_secs_f32();
        let next = if elapsed < 1.0 {
            1.0
        } else {
            (1.0 - (elapsed - 1.0) / 0.4).max(0.0)
        };
        let changed = (next - self.alpha).abs() > f32::EPSILON;
        self.alpha = next;
        if self.alpha == 0.0 {
            self.text = None;
            return true;
        }
        changed
    }
}

fn next_subtitle_track(current: usize, count: usize) -> usize {
    if count == 0 { 0 } else { (current + 1) % count }
}

fn subtitle_status_text(visible: bool, selected: usize, count: usize) -> CString {
    let status = if visible {
        format!("SUBTITLES: {} / {count}", selected + 1)
    } else {
        "SUBTITLES: OFF".to_owned()
    };
    CString::new(status).expect("subtitle status has no NUL bytes")
}

struct TopBar {
    focused: bool,
    last_motion: Instant,
    last_update: Instant,
    alpha: f32,
}

impl TopBar {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            focused: true,
            last_motion: now,
            last_update: now,
            alpha: 1.0,
        }
    }

    fn mouse_activity(&mut self) {
        self.last_motion = Instant::now();
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            self.mouse_activity();
        }
    }

    fn update(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now - self.last_update;
        self.last_update = now;
        let target = if self.focused && now - self.last_motion < Duration::from_millis(1500) {
            1.0
        } else {
            0.0
        };
        let previous = self.alpha;
        let step = elapsed.as_secs_f32() / 0.25;
        if self.alpha < target {
            self.alpha = (self.alpha + step).min(target);
        } else if self.alpha > target {
            self.alpha = (self.alpha - step).max(target);
        }
        (self.alpha - previous).abs() > f32::EPSILON
    }
}

struct PresentationStats {
    shown: u64,
    dropped: u64,
}

impl PresentationStats {
    fn new() -> Self {
        Self {
            shown: 0,
            dropped: 0,
        }
    }

    fn presented(&mut self) {
        self.shown += 1;
    }

    fn drop_frames(&mut self, count: usize) {
        self.dropped += count as u64;
    }

    fn text(&self, frame_rate: Option<f64>) -> CString {
        let frame_rate = frame_rate.map_or_else(|| "UNKNOWN".to_owned(), |fps| format!("{fps:.3}"));
        CString::new(format!(
            "FPS: {frame_rate}  SHOWN: {}  DROPPED: {}",
            self.shown, self.dropped
        ))
        .expect("statistics text has no NUL bytes")
    }
}

unsafe fn toggle_fullscreen(window: &Window, fullscreen: &mut bool) -> Result<()> {
    *fullscreen = !*fullscreen;
    if unsafe { ffi::up_window_set_fullscreen(window.0, i32::from(*fullscreen)) } == 0 {
        return Err(format!("could not toggle fullscreen: {}", unsafe {
            sdl_error()
        }));
    }
    Ok(())
}

unsafe fn set_playback_paused(
    decoder: &DecodeWorker,
    clock: &mut WallClock,
    paused: &mut bool,
    requested: bool,
) -> Result<()> {
    if *paused == requested {
        return Ok(());
    }
    let media = decoder.lock()?;
    unsafe { media.set_paused(requested)? };
    clock.set_paused(requested);
    *paused = requested;
    Ok(())
}

unsafe fn seek_by(
    media: &mut Media,
    clock: &mut WallClock,
    notice: &mut PositionNotice,
    offset: f64,
    playback_start: f64,
    duration: Option<f64>,
) -> Result<f64> {
    let maximum = duration.map_or(f64::MAX, |duration| {
        playback_start + (duration - 0.05).max(0.0)
    });
    let target = (clock.now() + offset).clamp(playback_start, maximum);
    let target = unsafe { media.seek(target)? };
    while media.video_queue.is_empty() && !media.eof {
        unsafe { media.fill_queues()? };
    }
    let displayed_target = media.video_queue.front().map_or(target, |frame| frame.pts);
    clock.seek(displayed_target);
    let position = displayed_target - playback_start;
    notice.show(position, duration);
    Ok(position)
}

unsafe fn seek_to(
    media: &mut Media,
    clock: &mut WallClock,
    notice: &mut PositionNotice,
    position: f64,
    playback_start: f64,
    duration: f64,
) -> Result<f64> {
    let position = position.clamp(0.0, (duration - 0.05).max(0.0));
    let target = playback_start + position;
    let target = unsafe { media.seek(target)? };
    while media.video_queue.is_empty() && !media.eof {
        unsafe { media.fill_queues()? };
    }
    let displayed_target = media.video_queue.front().map_or(target, |frame| frame.pts);
    clock.seek(displayed_target);
    let position = displayed_target - playback_start;
    notice.show(position, Some(duration));
    Ok(position)
}

fn seconds_to_microseconds(seconds: f64) -> i64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        0
    } else {
        (seconds * 1_000_000.0).min(i64::MAX as f64) as i64
    }
}

fn media_title(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("undefined-player");
    let stem = stem
        .strip_suffix(']')
        .and_then(|value| value.rsplit_once(" ["))
        .filter(|(_, id)| {
            id.len() == 11
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .map_or(stem, |(title, _)| title);
    let title = stem.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        "undefined-player".into()
    } else {
        title
    }
}

fn display_title(path: &Path) -> String {
    media_title(path).to_uppercase()
}

unsafe fn run(path: PathBuf, perf_log: bool) -> Result<()> {
    let _sdl = unsafe { Sdl::init()? };
    let metadata_title = CString::new(media_title(&path))
        .map_err(|_| "media filename contains a NUL byte".to_string())?;
    let title = CString::new(display_title(&path))
        .map_err(|_| "media filename contains a NUL byte".to_string())?;
    let path_text = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| "media path contains a NUL byte".to_string())?;
    let window = unsafe { Window::create(&title)? };
    let _wayland_input = match unsafe { WaylandInput::create(&window) } {
        Ok(input) => Some(input),
        Err(error) => {
            eprintln!("warning: {error}; drag-anywhere is unavailable");
            None
        }
    };
    let renderer = unsafe { Renderer::create(&window)? };
    let mut media = unsafe { Media::open(&path, renderer.device(), perf_log)? };

    unsafe { media.fill_queues()? };
    if media.video_queue.is_empty() {
        return Err("the video decoder produced no frames".into());
    }
    let video_info =
        unsafe { VideoInfo::inspect(&media, media.video_queue.front().unwrap().frame) };
    let video_details = video_info.overlay_text();
    unsafe { window.set_minimum_size()? };
    let (mut width, mut height) = unsafe { window.pixel_size()? };
    unsafe { renderer.resize(width, height)? };
    let first_frame = media.video_queue.front().unwrap();
    unsafe {
        renderer.display(
            first_frame.frame,
            width,
            height,
            1.0,
            &title,
            RendererOverlays {
                info: None,
                details: None,
                position: None,
                scrubber: None,
                subtitle: None,
            },
        )?
    };
    unsafe { media.start_audio()? };

    let clock_origin =
        unsafe { media.audio_clock() }.unwrap_or(media.first_video_pts.unwrap_or(0.0));
    let playback_start = clock_origin;
    let media_duration = media.duration();
    let mpris = unsafe {
        Mpris::create(
            &metadata_title,
            &path_text,
            media_duration.map_or(0, seconds_to_microseconds),
        )
    };
    let subtitle_track_count = media.subtitle_decoders.len();
    let subtitles_available = subtitle_track_count > 0;
    let decoder = DecodeWorker::start(media, perf_log);
    let mut clock = WallClock::new(clock_origin);
    let mut current_video = None;
    let mut video_queue = VecDeque::new();
    let mut subtitle_queue = VecDeque::new();
    let mut subtitle_queues = (0..subtitle_track_count)
        .map(|_| VecDeque::new())
        .collect::<Vec<VecDeque<SubtitleCue>>>();
    let mut current_subtitles = (0..subtitle_track_count)
        .map(|_| None)
        .collect::<Vec<Option<SubtitleCue>>>();
    let mut running = true;
    let mut paused = false;
    let mut mpris_stopped = false;
    let mut fullscreen = false;
    let mut info_visible = false;
    let mut subtitles_visible = subtitles_available;
    let mut selected_subtitle_track = 0;
    let mut redraw = true;
    let mut new_frame_pending = true;
    let mut top_bar = TopBar::new();
    let mut position_notice = PositionNotice::new();
    let mut scrubbing = false;
    let mut scrub_preview = None;
    let mut pending_scrub_target = None;
    let mut stats = PresentationStats::new();
    let mut last_info_refresh = Instant::now();
    let mut last_perf_report = Instant::now();
    let mut report_shown = 0;
    let mut report_dropped = 0;
    let mut report_fill_time = Duration::ZERO;
    let mut report_display_time = Duration::ZERO;
    let mut report_display_calls = 0_u64;
    while running {
        let mut event: ffi::UpEvent = unsafe { std::mem::zeroed() };
        while unsafe { ffi::up_platform_poll_event(&mut event) } != 0 {
            match event.type_ {
                ffi::UpEventType_UP_EVENT_QUIT | ffi::UpEventType_UP_EVENT_WINDOW_CLOSE => {
                    running = false
                }
                ffi::UpEventType_UP_EVENT_WINDOW_RESIZED => {
                    (width, height) = unsafe { window.pixel_size()? };
                    unsafe { renderer.resize(width, height)? };
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_WINDOW_EXPOSED => redraw = true,
                ffi::UpEventType_UP_EVENT_WINDOW_FOCUS_GAINED => {
                    top_bar.set_focused(true);
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_WINDOW_FOCUS_LOST => {
                    top_bar.set_focused(false);
                    if scrubbing {
                        pending_scrub_target = scrub_preview.take();
                    }
                    scrubbing = false;
                    unsafe { ffi::up_platform_capture_mouse(0) };
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_MOUSE_MOTION => {
                    top_bar.mouse_activity();
                    if scrubbing
                        && let Some(target) =
                            unsafe { window.scrubber_target(event.x, event.y, media_duration) }
                    {
                        scrub_preview = Some(target);
                        position_notice.show(target, media_duration);
                    }
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_MOUSE_BUTTON_DOWN => {
                    top_bar.mouse_activity();
                    redraw = true;
                    if event.button == ffi::UP_MOUSE_BUTTON_LEFT as u8 {
                        if let Some(target) =
                            unsafe { window.scrubber_target(event.x, event.y, media_duration) }
                        {
                            scrubbing = true;
                            scrub_preview = Some(target);
                            position_notice.show(target, media_duration);
                            unsafe { ffi::up_platform_capture_mouse(1) };
                        } else if unsafe { window.close_button_contains(event.x, event.y) } {
                            running = false;
                        } else if event.clicks >= 2 {
                            unsafe { toggle_fullscreen(&window, &mut fullscreen)? };
                        }
                    }
                }
                ffi::UpEventType_UP_EVENT_MOUSE_BUTTON_UP => {
                    if event.button == ffi::UP_MOUSE_BUTTON_LEFT as u8 && scrubbing {
                        if let Some(target) =
                            unsafe { window.scrubber_target(event.x, event.y, media_duration) }
                        {
                            pending_scrub_target = Some(target);
                            position_notice.show(target, media_duration);
                        } else {
                            pending_scrub_target = scrub_preview;
                        }
                        scrubbing = false;
                        scrub_preview = None;
                        unsafe { ffi::up_platform_capture_mouse(0) };
                        redraw = true;
                    }
                }
                ffi::UpEventType_UP_EVENT_KEY_DOWN => {
                    if event.repeat != 0 {
                        continue;
                    }
                    match action_for_key(event.key) {
                        Some(Action::Quit) => running = false,
                        Some(Action::SeekBackward) => {
                            let mut media = decoder.lock()?;
                            decoder.clear_frames(&mut video_queue, &mut current_video);
                            decoder.clear_subtitles(
                                &mut subtitle_queue,
                                &mut subtitle_queues,
                                &mut current_subtitles,
                            );
                            let position = unsafe {
                                seek_by(
                                    &mut media,
                                    &mut clock,
                                    &mut position_notice,
                                    -SEEK_SECONDS,
                                    playback_start,
                                    media_duration,
                                )?
                            };
                            if let Some(mpris) = &mpris {
                                mpris.seeked(seconds_to_microseconds(position));
                            }
                            mpris_stopped = false;
                            redraw = true;
                            new_frame_pending = true;
                        }
                        Some(Action::SeekForward) => {
                            let mut media = decoder.lock()?;
                            decoder.clear_frames(&mut video_queue, &mut current_video);
                            decoder.clear_subtitles(
                                &mut subtitle_queue,
                                &mut subtitle_queues,
                                &mut current_subtitles,
                            );
                            let position = unsafe {
                                seek_by(
                                    &mut media,
                                    &mut clock,
                                    &mut position_notice,
                                    SEEK_SECONDS,
                                    playback_start,
                                    media_duration,
                                )?
                            };
                            if let Some(mpris) = &mpris {
                                mpris.seeked(seconds_to_microseconds(position));
                            }
                            mpris_stopped = false;
                            redraw = true;
                            new_frame_pending = true;
                        }
                        Some(Action::ToggleFullscreen) => {
                            unsafe { toggle_fullscreen(&window, &mut fullscreen)? };
                            redraw = true;
                        }
                        Some(Action::ToggleInfo) => {
                            info_visible = !info_visible;
                            redraw = true;
                        }
                        Some(Action::TogglePause) => {
                            let requested = !paused;
                            unsafe {
                                set_playback_paused(&decoder, &mut clock, &mut paused, requested)?
                            };
                            mpris_stopped = false;
                        }
                        Some(Action::ToggleSubtitles) if subtitles_available => {
                            subtitles_visible = !subtitles_visible;
                            position_notice.show_text(subtitle_status_text(
                                subtitles_visible,
                                selected_subtitle_track,
                                subtitle_track_count,
                            ));
                            redraw = true;
                        }
                        Some(Action::CycleSubtitles) if subtitles_available => {
                            selected_subtitle_track =
                                next_subtitle_track(selected_subtitle_track, subtitle_track_count);
                            subtitles_visible = true;
                            position_notice.show_text(subtitle_status_text(
                                true,
                                selected_subtitle_track,
                                subtitle_track_count,
                            ));
                            redraw = true;
                        }
                        Some(Action::ToggleSubtitles | Action::CycleSubtitles) => {}
                        None => {}
                    }
                }
                _ => {}
            }
        }

        if let Some(mpris) = &mpris {
            mpris.dispatch();
            while let Some(command) = mpris.take_command() {
                match command {
                    MprisCommand::Quit => running = false,
                    MprisCommand::Play => {
                        unsafe { set_playback_paused(&decoder, &mut clock, &mut paused, false)? };
                        mpris_stopped = false;
                    }
                    MprisCommand::Pause => {
                        unsafe { set_playback_paused(&decoder, &mut clock, &mut paused, true)? };
                        mpris_stopped = false;
                    }
                    MprisCommand::PlayPause => {
                        let requested = if mpris_stopped { false } else { !paused };
                        unsafe {
                            set_playback_paused(&decoder, &mut clock, &mut paused, requested)?
                        };
                        mpris_stopped = false;
                    }
                    MprisCommand::Stop => {
                        unsafe { set_playback_paused(&decoder, &mut clock, &mut paused, true)? };
                        mpris_stopped = true;
                        if media_duration.is_some() {
                            pending_scrub_target = Some(0.0);
                        }
                    }
                    MprisCommand::Seek(offset_us) => {
                        let mut media = decoder.lock()?;
                        decoder.clear_frames(&mut video_queue, &mut current_video);
                        decoder.clear_subtitles(
                            &mut subtitle_queue,
                            &mut subtitle_queues,
                            &mut current_subtitles,
                        );
                        let position = unsafe {
                            seek_by(
                                &mut media,
                                &mut clock,
                                &mut position_notice,
                                offset_us as f64 / 1_000_000.0,
                                playback_start,
                                media_duration,
                            )?
                        };
                        mpris.seeked(seconds_to_microseconds(position));
                        mpris_stopped = false;
                        redraw = true;
                        new_frame_pending = true;
                    }
                    MprisCommand::SetPosition(position_us) => {
                        if media_duration.is_some() {
                            pending_scrub_target = Some(position_us.max(0) as f64 / 1_000_000.0);
                        }
                    }
                }
            }
        }
        if !running {
            break;
        }
        if let (Some(position), Some(duration)) = (pending_scrub_target.take(), media_duration) {
            let mut media = decoder.lock()?;
            decoder.clear_frames(&mut video_queue, &mut current_video);
            decoder.clear_subtitles(
                &mut subtitle_queue,
                &mut subtitle_queues,
                &mut current_subtitles,
            );
            let position = unsafe {
                seek_to(
                    &mut media,
                    &mut clock,
                    &mut position_notice,
                    position,
                    playback_start,
                    duration,
                )?
            };
            if let Some(mpris) = &mpris {
                mpris.seeked(seconds_to_microseconds(position));
            }
            redraw = true;
            new_frame_pending = true;
        }
        decoder.check_error()?;
        decoder.receive_frames(&mut video_queue);
        decoder.receive_subtitles(&mut subtitle_queue)?;
        while let Some(subtitle) = subtitle_queue.pop_front() {
            let track = subtitle.track;
            if let Some(queue) = subtitle_queues.get_mut(track) {
                queue.push_back(subtitle);
            }
        }

        // SDL/PipeWire consumes audio in period-sized chunks (1024 samples on
        // this machine), so the continuous audio-anchored wall clock is used
        // for video presentation instead of the quantized queue counter.
        let playback_time = clock.now();
        if let Some(mpris) = &mpris {
            let status = if mpris_stopped {
                ffi::UpMprisStatus_UP_MPRIS_STATUS_STOPPED
            } else if paused {
                ffi::UpMprisStatus_UP_MPRIS_STATUS_PAUSED
            } else {
                ffi::UpMprisStatus_UP_MPRIS_STATUS_PLAYING
            };
            mpris.update(
                status,
                seconds_to_microseconds(playback_time - playback_start),
            );
        }

        for (queue, current) in subtitle_queues.iter_mut().zip(current_subtitles.iter_mut()) {
            while queue
                .front()
                .is_some_and(|subtitle| subtitle.start <= playback_time)
            {
                *current = queue.pop_front();
                redraw = true;
            }
            if current
                .as_ref()
                .is_some_and(|subtitle| subtitle.end <= playback_time)
            {
                *current = None;
                redraw = true;
            }
        }

        if !paused {
            let mut due_frames = 0;
            if video_queue
                .front()
                .is_some_and(|frame| frame.frame.pts <= playback_time)
            {
                current_video = video_queue.pop_front();
                due_frames = 1;
                redraw = true;

                // A 144 Hz display can present a two-frame 60 FPS backlog in
                // order and catch up. Skip only sustained lateness.
                if video_queue
                    .get(1)
                    .is_some_and(|frame| frame.frame.pts <= playback_time)
                {
                    while video_queue
                        .front()
                        .is_some_and(|frame| frame.frame.pts <= playback_time)
                    {
                        current_video = video_queue.pop_front();
                        due_frames += 1;
                    }
                }
            }
            // Render at most one future frame early so it reaches the FIFO
            // presentation queue before its PTS.
            if due_frames == 0
                && video_queue
                    .front()
                    .is_some_and(|frame| frame.frame.pts <= playback_time + VIDEO_PRESENTATION_LEAD)
            {
                current_video = video_queue.pop_front();
                due_frames = 1;
                redraw = true;
            }
            if due_frames > 0 {
                stats.drop_frames(due_frames - 1);
                new_frame_pending = true;
            }
        }
        if current_video.is_none() && !video_queue.is_empty() {
            current_video = video_queue.pop_front();
            redraw = true;
            new_frame_pending = true;
        }

        redraw |= top_bar.update();
        redraw |= position_notice.update();
        if info_visible && last_info_refresh.elapsed() >= Duration::from_millis(100) {
            last_info_refresh = Instant::now();
            redraw = true;
        }

        if redraw && let Some(frame) = current_video.as_ref().map(|queued| &queued.frame) {
            let stats_info = info_visible.then(|| stats.text(video_info.frame_rate));
            let controls_visible = top_bar.alpha > 0.001;
            let persistent_position = (position_notice.text.is_none()
                && (info_visible || controls_visible))
                .then(|| timeline_text(playback_time - playback_start, media_duration));
            let position = position_notice
                .text
                .as_deref()
                .or(persistent_position.as_deref());
            let position_alpha = if position_notice.text.is_some() {
                position_notice.alpha
            } else if info_visible {
                1.0
            } else if controls_visible {
                top_bar.alpha
            } else {
                0.0
            };
            let scrubber = media_duration.map(|duration| {
                let position = scrub_preview.unwrap_or(playback_time - playback_start);
                let progress = (position / duration).clamp(0.0, 1.0);
                (progress as f32, top_bar.alpha)
            });
            let display_started = Instant::now();
            unsafe {
                renderer.display(
                    frame.frame,
                    width,
                    height,
                    top_bar.alpha,
                    &title,
                    RendererOverlays {
                        info: stats_info.as_deref().map(|text| (text, 1.0)),
                        details: info_visible.then_some(video_details.as_c_str()),
                        position: position.map(|text| (text, position_alpha)),
                        scrubber,
                        subtitle: subtitles_visible
                            .then(|| current_subtitles[selected_subtitle_track].as_ref())
                            .flatten(),
                    },
                )?
            };
            if perf_log {
                report_display_time += display_started.elapsed();
                report_display_calls += 1;
            }
            if new_frame_pending {
                stats.presented();
                new_frame_pending = false;
            }
            redraw = false;
        }

        if perf_log && last_perf_report.elapsed() >= Duration::from_secs(2) {
            let elapsed = last_perf_report.elapsed().as_secs_f64();
            let shown = stats.shown - report_shown;
            report_fill_time += decoder.take_fill_time();
            let fill_ms = report_fill_time.as_secs_f64() * 1000.0 / shown.max(1) as f64;
            let display_ms =
                report_display_time.as_secs_f64() * 1000.0 / report_display_calls.max(1) as f64;
            eprintln!(
                "perf: {:.1} shown/s, {:.1} dropped/s, {:.2} ms fill, {:.2} ms display",
                shown as f64 / elapsed,
                (stats.dropped - report_dropped) as f64 / elapsed,
                fill_ms,
                display_ms,
            );
            last_perf_report = Instant::now();
            report_shown = stats.shown;
            report_dropped = stats.dropped;
            report_fill_time = Duration::ZERO;
            report_display_time = Duration::ZERO;
            report_display_calls = 0;
        }

        if let Some(media) = decoder.try_lock()?
            && media.eof
            && media.video_queue.is_empty()
            && video_queue.is_empty()
            && decoder.outstanding_frames.load(Ordering::Acquire)
                == usize::from(current_video.is_some())
            && unsafe { media.audio_empty() }
            && current_video.as_ref().is_none_or(|frame| {
                playback_time >= frame.frame.pts + frame.frame.duration.max(0.1)
            })
        {
            break;
        }

        unsafe { ffi::up_platform_delay(2) };
    }

    Ok(())
}

fn main() {
    let mut arguments = env::args_os();
    let program = arguments
        .next()
        .and_then(|path| PathBuf::from(path).file_name().map(|name| name.to_owned()))
        .unwrap_or_else(|| "undefined-player".into());
    let mut path = None;
    let mut perf_log = false;
    for argument in arguments {
        if argument == "--perf" {
            perf_log = true;
        } else if path.is_none() {
            path = Some(PathBuf::from(argument));
        } else {
            eprintln!("only one media file can be played at a time");
            std::process::exit(2);
        }
    }
    let Some(path) = path else {
        eprintln!("usage: {} [--perf] VIDEO", Path::new(&program).display());
        std::process::exit(2);
    };
    if !path.is_file() {
        eprintln!("{} is not a file", path.display());
        std::process::exit(2);
    }

    if let Err(error) = unsafe { run(path, perf_log) } {
        eprintln!("undefined-player: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_keys_map_to_requested_actions() {
        assert_eq!(
            action_for_key(ffi::UpKey_UP_KEY_F),
            Some(Action::ToggleFullscreen)
        );
        assert_eq!(
            action_for_key(ffi::UpKey_UP_KEY_I),
            Some(Action::ToggleInfo)
        );
        assert_eq!(
            action_for_key(ffi::UpKey_UP_KEY_LEFT),
            Some(Action::SeekBackward)
        );
        assert_eq!(
            action_for_key(ffi::UpKey_UP_KEY_RIGHT),
            Some(Action::SeekForward)
        );
        assert_eq!(
            action_for_key(ffi::UpKey_UP_KEY_SPACE),
            Some(Action::TogglePause)
        );
        assert_eq!(
            action_for_key(ffi::UpKey_UP_KEY_S),
            Some(Action::ToggleSubtitles)
        );
        assert_eq!(
            action_for_key(ffi::UpKey_UP_KEY_J),
            Some(Action::CycleSubtitles)
        );
        assert_eq!(action_for_key(ffi::UpKey_UP_KEY_Q), Some(Action::Quit));
        assert_eq!(action_for_key(ffi::UpKey_UP_KEY_OTHER), None);
    }

    #[test]
    fn subtitle_tracks_cycle_and_report_status() {
        assert_eq!(next_subtitle_track(0, 3), 1);
        assert_eq!(next_subtitle_track(2, 3), 0);
        assert_eq!(next_subtitle_track(0, 0), 0);
        assert_eq!(
            subtitle_status_text(true, 1, 3).to_bytes(),
            b"SUBTITLES: 2 / 3"
        );
        assert_eq!(
            subtitle_status_text(false, 1, 3).to_bytes(),
            b"SUBTITLES: OFF"
        );
    }

    #[test]
    fn close_button_uses_physical_top_bar_size() {
        assert!(close_button_contains(1270.0, 10.0, 1280, 720, 1280, 720));
        assert!(!close_button_contains(1200.0, 10.0, 1280, 720, 1280, 720));
        assert!(!close_button_contains(1270.0, 60.0, 1280, 720, 1280, 720));

        // A 42-pixel button is 21 logical units at 2x scaling.
        assert!(close_button_contains(1265.0, 15.0, 1280, 720, 2560, 1440));
        assert!(!close_button_contains(1250.0, 15.0, 1280, 720, 2560, 1440));
        assert!(!close_button_contains(1265.0, 25.0, 1280, 720, 2560, 1440));
    }

    #[test]
    fn positions_are_formatted_for_short_and_long_media() {
        assert_eq!(format_position(0.0), "0:00");
        assert_eq!(format_position(83.9), "1:23");
        assert_eq!(format_position(3723.0), "1:02:03");
        assert_eq!(
            timeline_text(20.0, Some(313.0)).to_str().unwrap(),
            "0:20 / 5:13"
        );
    }

    #[test]
    fn display_title_collapses_filename_whitespace() {
        assert_eq!(
            media_title(Path::new(
                "Kyoto Hidden Valleys Drive 🌿 Arashiyama to Kibune [8zcPIr0mDzU].webm"
            )),
            "Kyoto Hidden Valleys Drive 🌿 Arashiyama to Kibune"
        );
        assert_eq!(
            display_title(Path::new("Kyoto  Hidden   Valley.mkv")),
            "KYOTO HIDDEN VALLEY"
        );
        assert_eq!(
            display_title(Path::new(
                "Kyoto Hidden Valleys Drive 🌿 Arashiyama to Kibune ⧸ 8K 60fps HDR ⧸ Relaxing Piano [8zcPIr0mDzU].webm"
            )),
            "KYOTO HIDDEN VALLEYS DRIVE 🌿 ARASHIYAMA TO KIBUNE ⧸ 8K 60FPS HDR ⧸ RELAXING PIANO"
        );
    }

    #[test]
    fn mpris_positions_use_microseconds() {
        assert_eq!(seconds_to_microseconds(1.25), 1_250_000);
        assert_eq!(seconds_to_microseconds(-1.0), 0);
        assert_eq!(seconds_to_microseconds(f64::NAN), 0);
    }

    #[test]
    fn bitrates_use_compact_overlay_units() {
        assert_eq!(format_bitrate(18_750_000), "18.8 MBPS");
        assert_eq!(format_bitrate(192_000), "192 KBPS");
        assert_eq!(format_bitrate(0), "UNKNOWN");
        assert_eq!(
            format_video_bitrate(0, Some(23_963_146), 29_807_000),
            "24.0 MBPS"
        );
        assert_eq!(
            format_video_bitrate(0, None, 28_846_000),
            "28.8 MBPS (CONTAINER)"
        );
    }

    #[test]
    fn hdr_status_distinguishes_pq_hlg_sdr_and_unknown() {
        assert_eq!(hdr_status(ffi::UpHdrKind_UP_HDR_KIND_PQ, false), "YES (PQ)");
        assert_eq!(
            hdr_status(ffi::UpHdrKind_UP_HDR_KIND_HLG, false),
            "YES (HLG)"
        );
        assert_eq!(hdr_status(ffi::UpHdrKind_UP_HDR_KIND_SDR, false), "NO");
        assert_eq!(
            hdr_status(ffi::UpHdrKind_UP_HDR_KIND_UNKNOWN, false),
            "UNKNOWN"
        );
        assert_eq!(
            hdr_status(ffi::UpHdrKind_UP_HDR_KIND_SDR, true),
            "NO (ASSUMED)"
        );
    }

    #[test]
    fn video_details_are_kept_separate_from_runtime_statistics() {
        let info = VideoInfo {
            lines: [
                "CODEC: HEVC MAIN 10".into(),
                "RESOLUTION: 3840X2160".into(),
                "BITRATE: 18.8 MBPS".into(),
                "PIXEL FORMAT: YUV420P10LE".into(),
                "DECODE: VULKAN HW".into(),
                "MATRIX: BT2020NC".into(),
                "PRIMARIES: BT2020".into(),
                "TRANSFER: SMPTE2084".into(),
                "RANGE: TV".into(),
                "HDR: YES (PQ)".into(),
            ],
            frame_rate: Some(24_000.0 / 1001.0),
        };
        assert_eq!(
            info.overlay_text().to_bytes(),
            b"CODEC: HEVC MAIN 10\nRESOLUTION: 3840X2160\nBITRATE: 18.8 MBPS\nPIXEL FORMAT: YUV420P10LE\nDECODE: VULKAN HW\nMATRIX: BT2020NC\nPRIMARIES: BT2020\nTRANSFER: SMPTE2084\nRANGE: TV\nHDR: YES (PQ)"
        );
        assert_eq!(
            PresentationStats::new().text(info.frame_rate).to_bytes(),
            b"FPS: 23.976  SHOWN: 0  DROPPED: 0"
        );
    }

    #[test]
    fn scrubber_maps_mouse_position_and_avoids_resize_edges() {
        assert_eq!(
            scrubber_target(640.0, 700.0, 1280, 720, 2560, 1440, 100.0),
            Some(50.0)
        );
        assert_eq!(
            scrubber_target(640.0, 680.0, 1280, 720, 2560, 1440, 100.0),
            None
        );
        assert_eq!(
            scrubber_target(640.0, 715.0, 1280, 720, 2560, 1440, 100.0),
            None
        );
        assert_eq!(
            scrubber_target(5.0, 700.0, 1280, 720, 2560, 1440, 100.0),
            None
        );
    }

    #[test]
    fn seeking_chooses_the_closest_available_keyframe() {
        assert_eq!(closest_seek_point(10.0, Some(6.0), Some(12.0)), Some(12.0));
        assert_eq!(closest_seek_point(10.0, Some(8.0), Some(14.0)), Some(8.0));
        assert_eq!(closest_seek_point(10.0, Some(8.0), None), Some(8.0));
        assert_eq!(closest_seek_point(10.0, None, None), None);
    }

    #[test]
    fn ass_dialogue_is_reduced_to_overlay_text() {
        assert_eq!(
            subtitle_dialogue_text(
                "0,0,Default,,0,0,0,,{\\i1}Hello, world!\\NSecond line",
                true
            ),
            "Hello, world!\nSecond line"
        );
        assert_eq!(
            subtitle_dialogue_text("Français — 日本語 — 希布來語", false),
            "Français - 日本語 - 希布來語"
        );
    }
}
