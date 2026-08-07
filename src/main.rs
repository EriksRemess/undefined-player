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
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

const SDL_WINDOW_RESIZABLE: u64 = 0x20;
const SDL_WINDOW_BORDERLESS: u64 = 0x10;
const SDL_WINDOW_HIGH_PIXEL_DENSITY: u64 = 0x2000;
const SDL_WINDOW_VULKAN: u64 = 0x1000_0000;
const SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK: u32 = u32::MAX;
const AUDIO_RATE: i32 = 48_000;
const AUDIO_CHANNELS: i32 = 2;
const AUDIO_BYTES_PER_FRAME: i64 = (size_of::<f32>() * AUDIO_CHANNELS as usize) as i64;
const AUDIO_QUEUE_TARGET_BYTES: i32 = AUDIO_RATE * AUDIO_BYTES_PER_FRAME as i32 * 150 / 1000;
const VIDEO_QUEUE_TARGET: usize = 16;
const VIDEO_QUEUE_MAX: usize = 24;
const VIDEO_PRESENTATION_LEAD: f64 = 0.012;
const SEEK_SECONDS: f64 = 10.0;
const TOP_BAR_HEIGHT_PIXELS: f32 = 42.0;
const AV_NOPTS_VALUE: i64 = i64::MIN;

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Eq, PartialEq)]
enum Action {
    Quit,
    SeekBackward,
    SeekForward,
    ToggleFullscreen,
    ToggleInfo,
    TogglePause,
}

fn action_for_key(key: u32) -> Option<Action> {
    match key {
        ffi::SDLK_Q => Some(Action::Quit),
        ffi::SDLK_LEFT => Some(Action::SeekBackward),
        ffi::SDLK_RIGHT => Some(Action::SeekForward),
        ffi::SDLK_F => Some(Action::ToggleFullscreen),
        ffi::SDLK_I => Some(Action::ToggleInfo),
        ffi::SDLK_SPACE => Some(Action::TogglePause),
        _ => None,
    }
}

unsafe fn sdl_error() -> String {
    let error = unsafe { ffi::SDL_GetError() };
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
    if unsafe { ffi::av_strerror(code, buffer.as_mut_ptr(), buffer.len()) } < 0 {
        return format!("FFmpeg error {code}");
    }
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

fn rational(value: ffi::AVRational) -> f64 {
    if value.den == 0 {
        0.0
    } else {
        value.num as f64 / value.den as f64
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

struct Sdl;

impl Sdl {
    unsafe fn init() -> Result<Self> {
        // This player deliberately has no X11 or non-PipeWire runtime path.
        unsafe {
            env::set_var("SDL_VIDEODRIVER", "wayland");
            env::set_var("SDL_AUDIODRIVER", "pipewire");
        }
        if !unsafe { ffi::SDL_Init(ffi::SDL_INIT_VIDEO | ffi::SDL_INIT_AUDIO) } {
            return Err(format!("SDL initialization failed: {}", unsafe {
                sdl_error()
            }));
        }
        Ok(Self)
    }
}

impl Drop for Sdl {
    fn drop(&mut self) {
        unsafe { ffi::SDL_Quit() };
    }
}

struct Window(*mut ffi::SDL_Window);

unsafe extern "C" fn resize_hit_test(
    window: *mut ffi::SDL_Window,
    point: *const ffi::SDL_Point,
    _data: *mut c_void,
) -> ffi::SDL_HitTestResult {
    const BORDER: i32 = 10;
    let mut width = 0;
    let mut height = 0;
    let mut pixel_width = 0;
    let mut pixel_height = 0;
    unsafe { ffi::SDL_GetWindowSize(window, &mut width, &mut height) };
    unsafe { ffi::SDL_GetWindowSizeInPixels(window, &mut pixel_width, &mut pixel_height) };
    let point = unsafe { &*point };
    if close_button_contains(
        point.x as f32,
        point.y as f32,
        width,
        height,
        pixel_width,
        pixel_height,
    ) {
        return ffi::SDL_HitTestResult_SDL_HITTEST_NORMAL;
    }
    let left = point.x <= BORDER;
    let right = point.x >= width - BORDER;
    let top = point.y <= BORDER;
    let bottom = point.y >= height - BORDER;

    match (top, bottom, left, right) {
        (true, _, true, _) => ffi::SDL_HitTestResult_SDL_HITTEST_RESIZE_TOPLEFT,
        (true, _, _, true) => ffi::SDL_HitTestResult_SDL_HITTEST_RESIZE_TOPRIGHT,
        (_, true, true, _) => ffi::SDL_HitTestResult_SDL_HITTEST_RESIZE_BOTTOMLEFT,
        (_, true, _, true) => ffi::SDL_HitTestResult_SDL_HITTEST_RESIZE_BOTTOMRIGHT,
        (true, _, _, _) => ffi::SDL_HitTestResult_SDL_HITTEST_RESIZE_TOP,
        (_, true, _, _) => ffi::SDL_HitTestResult_SDL_HITTEST_RESIZE_BOTTOM,
        (_, _, true, _) => ffi::SDL_HitTestResult_SDL_HITTEST_RESIZE_LEFT,
        (_, _, _, true) => ffi::SDL_HitTestResult_SDL_HITTEST_RESIZE_RIGHT,
        _ => ffi::SDL_HitTestResult_SDL_HITTEST_NORMAL,
    }
}

impl Window {
    unsafe fn create(title: &CStr) -> Result<Self> {
        let flags = SDL_WINDOW_VULKAN
            | SDL_WINDOW_RESIZABLE
            | SDL_WINDOW_HIGH_PIXEL_DENSITY
            | SDL_WINDOW_BORDERLESS;
        let window = unsafe { ffi::SDL_CreateWindow(title.as_ptr(), 1280, 720, flags) };
        if window.is_null() {
            return Err(format!("could not create the Wayland window: {}", unsafe {
                sdl_error()
            }));
        }
        if !unsafe { ffi::SDL_SetWindowHitTest(window, Some(resize_hit_test), ptr::null_mut()) } {
            let error = unsafe { sdl_error() };
            unsafe { ffi::SDL_DestroyWindow(window) };
            return Err(format!("could not enable mouse resizing: {error}"));
        }
        Ok(Self(window))
    }

    unsafe fn pixel_size(&self) -> Result<(i32, i32)> {
        let mut width = 0;
        let mut height = 0;
        if !unsafe { ffi::SDL_GetWindowSizeInPixels(self.0, &mut width, &mut height) } {
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
        if !unsafe { ffi::SDL_GetWindowSize(self.0, &mut logical_width, &mut logical_height) }
            || !unsafe {
                ffi::SDL_GetWindowSizeInPixels(self.0, &mut pixel_width, &mut pixel_height)
            }
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

    unsafe fn set_minimum_size(&self) -> Result<()> {
        if !unsafe { ffi::SDL_SetWindowMinimumSize(self.0, 320, 180) } {
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
        unsafe { ffi::SDL_DestroyWindow(self.0) };
    }
}

struct WaylandInput(*mut ffi::UpWaylandInput);

impl WaylandInput {
    unsafe fn create(window: &Window) -> Result<Self> {
        let input = unsafe { ffi::up_wayland_input_create(window.0) };
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

struct Renderer(*mut ffi::UpVideoRenderer);

struct RendererOverlays<'a> {
    info: Option<(&'a CStr, f32)>,
    position: Option<(&'a CStr, f32)>,
}

impl Renderer {
    unsafe fn create(window: &Window) -> Result<Self> {
        let renderer = unsafe { ffi::up_video_renderer_create(window.0) };
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

    unsafe fn device(&self) -> *mut ffi::AVBufferRef {
        unsafe { ffi::up_video_renderer_device(self.0) }
    }

    unsafe fn display(
        &self,
        frame: *mut ffi::AVFrame,
        width: i32,
        height: i32,
        top_bar_alpha: f32,
        title: &CStr,
        overlays: RendererOverlays<'_>,
    ) -> Result<()> {
        let (info, info_alpha) = overlays
            .info
            .map_or((ptr::null(), 0.0), |(text, alpha)| (text.as_ptr(), alpha));
        let (position, position_alpha) = overlays
            .position
            .map_or((ptr::null(), 0.0), |(text, alpha)| (text.as_ptr(), alpha));
        if unsafe {
            ffi::up_video_renderer_display(
                self.0,
                frame,
                width,
                height,
                top_bar_alpha,
                title.as_ptr(),
                info,
                info_alpha,
                position,
                position_alpha,
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
    context: *mut ffi::AVCodecContext,
    stream_index: i32,
    time_base: ffi::AVRational,
    uses_vulkan: bool,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { ffi::avcodec_free_context(&mut self.context) };
    }
}

unsafe extern "C" fn choose_vulkan_format(
    _context: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    let mut current = formats;
    while !current.is_null() && unsafe { *current } != ffi::AVPixelFormat_AV_PIX_FMT_NONE {
        if unsafe { *current } == ffi::AVPixelFormat_AV_PIX_FMT_VULKAN {
            return ffi::AVPixelFormat_AV_PIX_FMT_VULKAN;
        }
        current = unsafe { current.add(1) };
    }
    ffi::AVPixelFormat_AV_PIX_FMT_NONE
}

unsafe fn decoder_supports_vulkan(codec: *const ffi::AVCodec) -> bool {
    let mut index = 0;
    loop {
        let config = unsafe { ffi::avcodec_get_hw_config(codec, index) };
        if config.is_null() {
            return false;
        }
        if unsafe {
            (*config).device_type == ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_VULKAN
                && (*config).pix_fmt == ffi::AVPixelFormat_AV_PIX_FMT_VULKAN
                && ((*config).methods as u32 & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) != 0
        } {
            return true;
        }
        index += 1;
    }
}

impl Decoder {
    unsafe fn open(
        format: *mut ffi::AVFormatContext,
        stream_index: i32,
        vulkan_device: Option<*mut ffi::AVBufferRef>,
    ) -> Result<Self> {
        let stream = unsafe { *(*format).streams.add(stream_index as usize) };
        let parameters = unsafe { (*stream).codecpar };
        let codec = if vulkan_device.is_some() {
            // Do not use avcodec_find_decoder(): this local FFmpeg intentionally
            // puts CUVID first. The generic decoder is what exposes Vulkan Video.
            let name = unsafe { ffi::avcodec_get_name((*parameters).codec_id) };
            unsafe { ffi::avcodec_find_decoder_by_name(name) }
        } else {
            unsafe { ffi::avcodec_find_decoder((*parameters).codec_id) }
        };
        if codec.is_null() {
            return Err("no decoder is available for the selected stream".into());
        }
        let uses_vulkan = vulkan_device.is_some() && unsafe { decoder_supports_vulkan(codec) };

        let mut context = unsafe { ffi::avcodec_alloc_context3(codec) };
        if context.is_null() {
            return Err("out of memory while allocating a decoder".into());
        }
        let result = (|| {
            let ret = unsafe { ffi::avcodec_parameters_to_context(context, parameters) };
            if ret < 0 {
                return Err(format!("could not configure decoder: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }
            unsafe { (*context).pkt_timebase = (*stream).time_base };

            if uses_vulkan {
                let device = vulkan_device.expect("a Vulkan decoder has a Vulkan device");
                unsafe {
                    (*context).get_format = Some(choose_vulkan_format);
                    (*context).hw_device_ctx = ffi::av_buffer_ref(device);
                    (*context).extra_hw_frames = 16;
                }
                if unsafe { (*context).hw_device_ctx }.is_null() {
                    return Err("could not retain the Vulkan decoder device".into());
                }
            }

            let ret = unsafe { ffi::avcodec_open2(context, codec, ptr::null_mut()) };
            if ret < 0 {
                return Err(format!("could not open decoder: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }
            Ok(Self {
                context,
                stream_index,
                time_base: unsafe { (*stream).time_base },
                uses_vulkan,
            })
        })();
        if result.is_err() {
            unsafe { ffi::avcodec_free_context(&mut context) };
        }
        result
    }
}

struct VideoFrame {
    frame: *mut ffi::AVFrame,
    pts: f64,
    duration: f64,
}

// The worker transfers exclusive ownership of each reference-counted AVFrame
// to the presentation thread; the pointer is never accessed concurrently.
unsafe impl Send for VideoFrame {}

impl Drop for VideoFrame {
    fn drop(&mut self) {
        unsafe { ffi::av_frame_free(&mut self.frame) };
    }
}

struct AudioOutput {
    stream: *mut ffi::SDL_AudioStream,
    resampler: *mut ffi::SwrContext,
    output_layout: ffi::AVChannelLayout,
    first_pts: Option<f64>,
    submitted_frames: i64,
    resumed: bool,
}

impl AudioOutput {
    unsafe fn create() -> Result<Self> {
        let spec = ffi::SDL_AudioSpec {
            format: ffi::SDL_AudioFormat_SDL_AUDIO_F32,
            channels: AUDIO_CHANNELS,
            freq: AUDIO_RATE,
        };
        let stream = unsafe {
            ffi::SDL_OpenAudioDeviceStream(
                SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK,
                &spec,
                None,
                ptr::null_mut(),
            )
        };
        if stream.is_null() {
            return Err(format!("could not open PipeWire audio: {}", unsafe {
                sdl_error()
            }));
        }

        let mut output_layout = unsafe { std::mem::zeroed() };
        unsafe { ffi::av_channel_layout_default(&mut output_layout, AUDIO_CHANNELS) };
        Ok(Self {
            stream,
            resampler: ptr::null_mut(),
            output_layout,
            first_pts: None,
            submitted_frames: 0,
            resumed: false,
        })
    }

    unsafe fn initialize_resampler(&mut self, frame: *const ffi::AVFrame) -> Result<()> {
        if !self.resampler.is_null() {
            return Ok(());
        }
        let ret = unsafe {
            ffi::swr_alloc_set_opts2(
                &mut self.resampler,
                &self.output_layout,
                ffi::AVSampleFormat_AV_SAMPLE_FMT_FLT,
                AUDIO_RATE,
                &(*frame).ch_layout,
                (*frame).format,
                (*frame).sample_rate,
                0,
                ptr::null_mut(),
            )
        };
        if ret < 0 || self.resampler.is_null() {
            return Err(format!(
                "could not configure audio conversion: {}",
                unsafe { ffmpeg_error(ret) }
            ));
        }
        let ret = unsafe { ffi::swr_init(self.resampler) };
        if ret < 0 {
            return Err(format!(
                "could not initialize audio conversion: {}",
                unsafe { ffmpeg_error(ret) }
            ));
        }
        Ok(())
    }

    unsafe fn push(
        &mut self,
        frame: *const ffi::AVFrame,
        time_base: ffi::AVRational,
        discard_before: Option<f64>,
    ) -> Result<bool> {
        unsafe { self.initialize_resampler(frame)? };

        let timestamp = unsafe { (*frame).best_effort_timestamp };
        let frame_pts =
            (timestamp != AV_NOPTS_VALUE).then(|| timestamp as f64 * rational(time_base));

        let capacity = unsafe { ffi::swr_get_out_samples(self.resampler, (*frame).nb_samples) };
        if capacity < 0 {
            return Err("could not calculate converted audio size".into());
        }
        let mut samples = vec![0_f32; capacity as usize * AUDIO_CHANNELS as usize];
        let output = [samples.as_mut_ptr().cast::<u8>()];
        let converted = unsafe {
            ffi::swr_convert(
                self.resampler,
                output.as_ptr(),
                capacity,
                (*frame).extended_data.cast::<*const u8>(),
                (*frame).nb_samples,
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
            && !unsafe {
                ffi::SDL_PutAudioStreamData(
                    self.stream,
                    samples
                        .as_ptr()
                        .add(skipped as usize * AUDIO_CHANNELS as usize)
                        .cast::<c_void>(),
                    bytes as i32,
                )
            }
        {
            return Err(format!("could not queue audio: {}", unsafe { sdl_error() }));
        }
        self.submitted_frames += queued as i64;
        Ok(queued > 0)
    }

    unsafe fn queued_bytes(&self) -> i32 {
        unsafe { ffi::SDL_GetAudioStreamQueued(self.stream) }.max(0)
    }

    unsafe fn clock(&self) -> Option<f64> {
        let base = self.first_pts?;
        let queued_frames = unsafe { self.queued_bytes() } as i64 / AUDIO_BYTES_PER_FRAME;
        Some(base + (self.submitted_frames - queued_frames) as f64 / AUDIO_RATE as f64)
    }

    unsafe fn resume(&mut self) -> Result<()> {
        if !self.resumed {
            if !unsafe { ffi::SDL_ResumeAudioStreamDevice(self.stream) } {
                return Err(format!("could not start audio: {}", unsafe { sdl_error() }));
            }
            self.resumed = true;
        }
        Ok(())
    }

    unsafe fn set_paused(&self, paused: bool) -> Result<()> {
        let ok = if paused {
            unsafe { ffi::SDL_PauseAudioStreamDevice(self.stream) }
        } else {
            unsafe { ffi::SDL_ResumeAudioStreamDevice(self.stream) }
        };
        if !ok {
            return Err(format!("could not change audio pause state: {}", unsafe {
                sdl_error()
            }));
        }
        Ok(())
    }

    unsafe fn reset(&mut self) -> Result<()> {
        if !unsafe { ffi::SDL_ClearAudioStream(self.stream) } {
            return Err(format!("could not clear queued audio: {}", unsafe {
                sdl_error()
            }));
        }
        unsafe { ffi::swr_free(&mut self.resampler) };
        self.first_pts = None;
        self.submitted_frames = 0;
        Ok(())
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        unsafe {
            ffi::swr_free(&mut self.resampler);
            ffi::av_channel_layout_uninit(&mut self.output_layout);
            ffi::SDL_DestroyAudioStream(self.stream);
        }
    }
}

struct Media {
    format: *mut ffi::AVFormatContext,
    packet: *mut ffi::AVPacket,
    video: Decoder,
    audio_decoder: Option<Decoder>,
    audio: Option<AudioOutput>,
    video_queue: VecDeque<VideoFrame>,
    eof: bool,
    drained: bool,
    first_video_pts: Option<f64>,
    video_seek_target: Option<f64>,
    audio_seek_target: Option<f64>,
}

impl Media {
    unsafe fn open(path: &Path, vulkan_device: *mut ffi::AVBufferRef) -> Result<Self> {
        let path = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| "media path contains a NUL byte".to_string())?;
        let mut format = ptr::null_mut();
        let ret = unsafe {
            ffi::avformat_open_input(&mut format, path.as_ptr(), ptr::null(), ptr::null_mut())
        };
        if ret < 0 {
            return Err(format!("could not open media: {}", unsafe {
                ffmpeg_error(ret)
            }));
        }

        let result = (|| {
            let ret = unsafe { ffi::avformat_find_stream_info(format, ptr::null_mut()) };
            if ret < 0 {
                return Err(format!("could not inspect media streams: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }

            let video_index = unsafe {
                ffi::av_find_best_stream(
                    format,
                    ffi::AVMediaType_AVMEDIA_TYPE_VIDEO,
                    -1,
                    -1,
                    ptr::null_mut(),
                    0,
                )
            };
            if video_index < 0 {
                return Err("the input has no video stream".into());
            }
            let video = unsafe { Decoder::open(format, video_index, Some(vulkan_device))? };
            let video_stream = unsafe { *(*format).streams.add(video_index as usize) };
            let video_parameters = unsafe { (*video_stream).codecpar };
            let video_name =
                unsafe { CStr::from_ptr(ffi::avcodec_get_name((*video_parameters).codec_id)) }
                    .to_string_lossy();
            let decode_path = if video.uses_vulkan {
                "NVIDIA Vulkan Video"
            } else {
                "software decode, Vulkan presentation"
            };
            eprintln!(
                "video: {video_name} {}x{} via {decode_path}",
                unsafe { (*video_parameters).width },
                unsafe { (*video_parameters).height }
            );

            let audio_index = unsafe {
                ffi::av_find_best_stream(
                    format,
                    ffi::AVMediaType_AVMEDIA_TYPE_AUDIO,
                    -1,
                    video_index,
                    ptr::null_mut(),
                    0,
                )
            };
            let (audio_decoder, audio) = if audio_index >= 0 {
                let audio_stream = unsafe { *(*format).streams.add(audio_index as usize) };
                let audio_parameters = unsafe { (*audio_stream).codecpar };
                let audio_name =
                    unsafe { CStr::from_ptr(ffi::avcodec_get_name((*audio_parameters).codec_id)) }
                        .to_string_lossy();
                eprintln!("audio: {audio_name} via PipeWire");
                (
                    Some(unsafe { Decoder::open(format, audio_index, None)? }),
                    Some(unsafe { AudioOutput::create()? }),
                )
            } else {
                (None, None)
            };

            let packet = unsafe { ffi::av_packet_alloc() };
            if packet.is_null() {
                return Err("out of memory while allocating a packet".into());
            }

            Ok(Self {
                format,
                packet,
                video,
                audio_decoder,
                audio,
                video_queue: VecDeque::new(),
                eof: false,
                drained: false,
                first_video_pts: None,
                video_seek_target: None,
                audio_seek_target: None,
            })
        })();

        if result.is_err() {
            unsafe { ffi::avformat_close_input(&mut format) };
        }
        result
    }

    unsafe fn receive_video(&mut self) -> Result<()> {
        loop {
            let mut frame = unsafe { ffi::av_frame_alloc() };
            if frame.is_null() {
                return Err("out of memory while decoding video".into());
            }
            let ret = unsafe { ffi::avcodec_receive_frame(self.video.context, frame) };
            if ret < 0 {
                unsafe { ffi::av_frame_free(&mut frame) };
                break;
            }
            if self.video.uses_vulkan
                && unsafe { (*frame).format } != ffi::AVPixelFormat_AV_PIX_FMT_VULKAN
            {
                unsafe { ffi::av_frame_free(&mut frame) };
                return Err(
                    "the selected codec/profile is not supported by NVIDIA Vulkan Video".into(),
                );
            }
            let timestamp = unsafe { (*frame).best_effort_timestamp };
            let pts = if timestamp == AV_NOPTS_VALUE {
                self.video_queue.back().map_or(0.0, |previous| {
                    previous.pts + previous.duration.max(1.0 / 60.0)
                })
            } else {
                timestamp as f64 * rational(self.video.time_base)
            };
            let raw_duration = unsafe { (*frame).duration };
            let duration = if raw_duration > 0 {
                raw_duration as f64 * rational(self.video.time_base)
            } else {
                0.0
            };
            if self
                .video_seek_target
                .is_some_and(|target| pts + duration.max(1.0 / 120.0) < target)
            {
                unsafe { ffi::av_frame_free(&mut frame) };
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
            let mut frame = unsafe { ffi::av_frame_alloc() };
            if frame.is_null() {
                return Err("out of memory while decoding audio".into());
            }
            let ret = unsafe { ffi::avcodec_receive_frame(context, frame) };
            if ret < 0 {
                unsafe { ffi::av_frame_free(&mut frame) };
                break;
            }
            if let Some(audio) = self.audio.as_mut() {
                let result = unsafe { audio.push(frame, time_base, self.audio_seek_target) };
                unsafe { ffi::av_frame_free(&mut frame) };
                if result? {
                    self.audio_seek_target = None;
                }
            } else {
                unsafe { ffi::av_frame_free(&mut frame) };
            }
        }
        Ok(())
    }

    unsafe fn decode_packet(&mut self) -> Result<()> {
        let stream_index = unsafe { (*self.packet).stream_index };
        if stream_index == self.video.stream_index {
            let ret = unsafe { ffi::avcodec_send_packet(self.video.context, self.packet) };
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
            let ret = unsafe { ffi::avcodec_send_packet(context, self.packet) };
            if ret < 0 {
                return Err(format!("audio decoder rejected a packet: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }
            unsafe { self.receive_audio()? };
        }
        Ok(())
    }

    unsafe fn drain(&mut self) -> Result<()> {
        if self.drained {
            return Ok(());
        }
        self.drained = true;
        unsafe {
            ffi::avcodec_send_packet(self.video.context, ptr::null());
            self.receive_video()?;
        }
        if let Some(decoder) = self.audio_decoder.as_ref() {
            unsafe {
                ffi::avcodec_send_packet(decoder.context, ptr::null());
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

            let ret = unsafe { ffi::av_read_frame(self.format, self.packet) };
            if ret < 0 {
                self.eof = true;
                unsafe { self.drain()? };
                break;
            }
            let decode = unsafe { self.decode_packet() };
            unsafe { ffi::av_packet_unref(self.packet) };
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
        let duration = unsafe { (*self.format).duration };
        (duration != AV_NOPTS_VALUE && duration > 0)
            .then(|| duration as f64 / ffi::AV_TIME_BASE as f64)
    }

    unsafe fn seek(&mut self, target: f64) -> Result<()> {
        let time_base = rational(self.video.time_base);
        if time_base <= 0.0 {
            return Err("video stream has an invalid time base".into());
        }
        let timestamp = (target / time_base).round() as i64;
        let ret = unsafe {
            ffi::av_seek_frame(
                self.format,
                self.video.stream_index,
                timestamp,
                ffi::AVSEEK_FLAG_BACKWARD as i32,
            )
        };
        if ret < 0 {
            return Err(format!("could not seek: {}", unsafe { ffmpeg_error(ret) }));
        }

        unsafe {
            ffi::av_packet_unref(self.packet);
            ffi::avcodec_flush_buffers(self.video.context);
        }
        if let Some(decoder) = self.audio_decoder.as_ref() {
            unsafe { ffi::avcodec_flush_buffers(decoder.context) };
        }
        if let Some(audio) = self.audio.as_mut() {
            unsafe { audio.reset()? };
        }
        self.video_queue.clear();
        self.eof = false;
        self.drained = false;
        self.video_seek_target = Some(target);
        self.audio_seek_target = self.audio.as_ref().map(|_| target);
        Ok(())
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
            ffi::av_packet_free(&mut self.packet);
            ffi::avformat_close_input(&mut self.format);
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

    fn clear_frames(
        &self,
        queue: &mut VecDeque<QueuedVideoFrame>,
        current: &mut Option<QueuedVideoFrame>,
    ) {
        queue.clear();
        *current = None;
        while self.frames.try_recv().is_ok() {}
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
        self.text = Some(timeline_text(position, duration));
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
    recent: VecDeque<Instant>,
}

impl PresentationStats {
    fn new() -> Self {
        Self {
            shown: 0,
            dropped: 0,
            recent: VecDeque::new(),
        }
    }

    fn presented(&mut self) {
        self.shown += 1;
        self.recent.push_back(Instant::now());
        self.prune();
    }

    fn drop_frames(&mut self, count: usize) {
        self.dropped += count as u64;
    }

    fn prune(&mut self) {
        let cutoff = Instant::now() - Duration::from_secs(1);
        while self.recent.front().is_some_and(|time| *time < cutoff) {
            self.recent.pop_front();
        }
    }

    fn text(&mut self) -> CString {
        self.prune();
        CString::new(format!(
            "FPS: {:.1}  SHOWN: {}  DROPPED: {}",
            self.recent.len() as f32,
            self.shown,
            self.dropped
        ))
        .expect("statistics text has no NUL bytes")
    }
}

unsafe fn toggle_fullscreen(window: &Window, fullscreen: &mut bool) -> Result<()> {
    *fullscreen = !*fullscreen;
    if !unsafe { ffi::SDL_SetWindowFullscreen(window.0, *fullscreen) } {
        return Err(format!("could not toggle fullscreen: {}", unsafe {
            sdl_error()
        }));
    }
    Ok(())
}

unsafe fn seek_by(
    media: &mut Media,
    clock: &mut WallClock,
    notice: &mut PositionNotice,
    offset: f64,
    playback_start: f64,
    duration: Option<f64>,
) -> Result<()> {
    let maximum = duration.map_or(f64::MAX, |duration| {
        playback_start + (duration - 0.05).max(0.0)
    });
    let target = (clock.now() + offset).clamp(playback_start, maximum);
    unsafe { media.seek(target)? };
    while media.video_queue.is_empty() && !media.eof {
        unsafe { media.fill_queues()? };
    }
    clock.seek(target);
    notice.show(target - playback_start, duration);
    Ok(())
}

unsafe fn run(path: PathBuf, perf_log: bool) -> Result<()> {
    let _sdl = unsafe { Sdl::init()? };
    let title = CString::new(
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("undefined-player"),
    )
    .map_err(|_| "media filename contains a NUL byte".to_string())?;
    let window = unsafe { Window::create(&title)? };
    let _wayland_input = unsafe { WaylandInput::create(&window)? };
    let renderer = unsafe { Renderer::create(&window)? };
    let mut media = unsafe { Media::open(&path, renderer.device())? };

    unsafe { media.fill_queues()? };
    if media.video_queue.is_empty() {
        return Err("the video decoder produced no frames".into());
    }
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
                position: None,
            },
        )?
    };
    unsafe { media.start_audio()? };

    let clock_origin =
        unsafe { media.audio_clock() }.unwrap_or(media.first_video_pts.unwrap_or(0.0));
    let playback_start = clock_origin;
    let media_duration = media.duration();
    let decoder = DecodeWorker::start(media, perf_log);
    let mut clock = WallClock::new(clock_origin);
    let mut current_video = None;
    let mut video_queue = VecDeque::new();
    let mut running = true;
    let mut paused = false;
    let mut fullscreen = false;
    let mut info_visible = false;
    let mut redraw = true;
    let mut new_frame_pending = true;
    let mut top_bar = TopBar::new();
    let mut position_notice = PositionNotice::new();
    let mut stats = PresentationStats::new();
    let mut last_info_refresh = Instant::now();
    let mut last_perf_report = Instant::now();
    let mut report_shown = 0;
    let mut report_dropped = 0;
    let mut report_fill_time = Duration::ZERO;
    let mut report_display_time = Duration::ZERO;
    let mut report_display_calls = 0_u64;
    while running {
        let mut event: ffi::SDL_Event = unsafe { std::mem::zeroed() };
        while unsafe { ffi::SDL_PollEvent(&mut event) } {
            let event_type = unsafe { event.type_ };
            match event_type {
                ffi::SDL_EventType_SDL_EVENT_QUIT
                | ffi::SDL_EventType_SDL_EVENT_WINDOW_CLOSE_REQUESTED => running = false,
                ffi::SDL_EventType_SDL_EVENT_WINDOW_RESIZED
                | ffi::SDL_EventType_SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED => {
                    (width, height) = unsafe { window.pixel_size()? };
                    unsafe { renderer.resize(width, height)? };
                    redraw = true;
                }
                ffi::SDL_EventType_SDL_EVENT_WINDOW_EXPOSED => redraw = true,
                ffi::SDL_EventType_SDL_EVENT_WINDOW_FOCUS_GAINED => {
                    top_bar.set_focused(true);
                    redraw = true;
                }
                ffi::SDL_EventType_SDL_EVENT_WINDOW_FOCUS_LOST => {
                    top_bar.set_focused(false);
                    redraw = true;
                }
                ffi::SDL_EventType_SDL_EVENT_MOUSE_MOTION => {
                    top_bar.mouse_activity();
                    redraw = true;
                }
                ffi::SDL_EventType_SDL_EVENT_MOUSE_BUTTON_DOWN => {
                    let button = unsafe { event.button };
                    top_bar.mouse_activity();
                    redraw = true;
                    if button.button == ffi::SDL_BUTTON_LEFT as u8 {
                        if unsafe { window.close_button_contains(button.x, button.y) } {
                            running = false;
                        } else if button.clicks >= 2 {
                            unsafe { toggle_fullscreen(&window, &mut fullscreen)? };
                        }
                    }
                }
                ffi::SDL_EventType_SDL_EVENT_KEY_DOWN => {
                    let keyboard = unsafe { event.key };
                    if keyboard.repeat {
                        continue;
                    }
                    match action_for_key(keyboard.key) {
                        Some(Action::Quit) => running = false,
                        Some(Action::SeekBackward) => {
                            let mut media = decoder.lock()?;
                            decoder.clear_frames(&mut video_queue, &mut current_video);
                            unsafe {
                                seek_by(
                                    &mut media,
                                    &mut clock,
                                    &mut position_notice,
                                    -SEEK_SECONDS,
                                    playback_start,
                                    media_duration,
                                )?
                            };
                            redraw = true;
                            new_frame_pending = true;
                        }
                        Some(Action::SeekForward) => {
                            let mut media = decoder.lock()?;
                            decoder.clear_frames(&mut video_queue, &mut current_video);
                            unsafe {
                                seek_by(
                                    &mut media,
                                    &mut clock,
                                    &mut position_notice,
                                    SEEK_SECONDS,
                                    playback_start,
                                    media_duration,
                                )?
                            };
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
                            paused = !paused;
                            let media = decoder.lock()?;
                            unsafe { media.set_paused(paused)? };
                            clock.set_paused(paused);
                        }
                        None => {}
                    }
                }
                _ => {}
            }
        }
        if !running {
            break;
        }
        decoder.check_error()?;
        decoder.receive_frames(&mut video_queue);

        // SDL/PipeWire consumes audio in period-sized chunks (1024 samples on
        // this machine), so the continuous audio-anchored wall clock is used
        // for video presentation instead of the quantized queue counter.
        let playback_time = clock.now();

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
            let stats_info = info_visible.then(|| stats.text());
            let persistent_position = (position_notice.text.is_none() && info_visible)
                .then(|| timeline_text(playback_time - playback_start, media_duration));
            let position = position_notice
                .text
                .as_deref()
                .or(persistent_position.as_deref());
            let position_alpha = if position_notice.text.is_some() {
                position_notice.alpha
            } else if info_visible {
                1.0
            } else {
                0.0
            };
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
                        position: position.map(|text| (text, position_alpha)),
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

        unsafe { ffi::SDL_Delay(2) };
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
        assert_eq!(action_for_key(ffi::SDLK_F), Some(Action::ToggleFullscreen));
        assert_eq!(action_for_key(ffi::SDLK_I), Some(Action::ToggleInfo));
        assert_eq!(action_for_key(ffi::SDLK_LEFT), Some(Action::SeekBackward));
        assert_eq!(action_for_key(ffi::SDLK_RIGHT), Some(Action::SeekForward));
        assert_eq!(action_for_key(ffi::SDLK_SPACE), Some(Action::TogglePause));
        assert_eq!(action_for_key(ffi::SDLK_Q), Some(Action::Quit));
        assert_eq!(action_for_key(ffi::SDLK_A), None);
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
}
