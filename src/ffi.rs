// Stable Rust declarations for the public C compatibility layer.
// Keep these signatures in sync with the corresponding headers in native/.

pub const UP_MOUSE_BUTTON_LEFT: u32 = 1;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpAvFormat {
    _unused: [u8; 0],
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpAvDecoder {
    _unused: [u8; 0],
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpAvPacket {
    _unused: [u8; 0],
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpAvFrame {
    _unused: [u8; 0],
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpAvAudioConverter {
    _unused: [u8; 0],
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpAvSubtitle {
    _unused: [u8; 0],
}
pub const UpMediaType_UP_MEDIA_TYPE_VIDEO: UpMediaType = 0;
pub const UpMediaType_UP_MEDIA_TYPE_AUDIO: UpMediaType = 1;
pub const UpMediaType_UP_MEDIA_TYPE_SUBTITLE: UpMediaType = 2;
pub type UpMediaType = ::std::os::raw::c_uint;
pub const UpHdrKind_UP_HDR_KIND_SDR: UpHdrKind = 0;
pub const UpHdrKind_UP_HDR_KIND_PQ: UpHdrKind = 1;
pub const UpHdrKind_UP_HDR_KIND_HLG: UpHdrKind = 2;
pub const UpHdrKind_UP_HDR_KIND_UNKNOWN: UpHdrKind = 3;
pub type UpHdrKind = ::std::os::raw::c_uint;
pub const UpSubtitleRectType_UP_SUBTITLE_RECT_OTHER: UpSubtitleRectType = 0;
pub const UpSubtitleRectType_UP_SUBTITLE_RECT_BITMAP: UpSubtitleRectType = 1;
pub const UpSubtitleRectType_UP_SUBTITLE_RECT_TEXT: UpSubtitleRectType = 2;
pub const UpSubtitleRectType_UP_SUBTITLE_RECT_ASS: UpSubtitleRectType = 3;
pub type UpSubtitleRectType = ::std::os::raw::c_uint;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpVideoInfo {
    pub codec: *const ::std::os::raw::c_char,
    pub profile: *const ::std::os::raw::c_char,
    pub pixel_format: *const ::std::os::raw::c_char,
    pub color_space: *const ::std::os::raw::c_char,
    pub color_primaries: *const ::std::os::raw::c_char,
    pub color_transfer: *const ::std::os::raw::c_char,
    pub color_range: *const ::std::os::raw::c_char,
    pub width: ::std::os::raw::c_int,
    pub height: ::std::os::raw::c_int,
    pub declared_bitrate: i64,
    pub metadata_bitrate: i64,
    pub container_bitrate: i64,
    pub frame_rate: f64,
    pub hdr_kind: UpHdrKind,
    pub color_space_assumed: ::std::os::raw::c_int,
    pub color_primaries_assumed: ::std::os::raw::c_int,
    pub color_transfer_assumed: ::std::os::raw::c_int,
    pub color_range_assumed: ::std::os::raw::c_int,
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpSubtitleInfo {
    pub pts: i64,
    pub start_display_time: u32,
    pub end_display_time: u32,
    pub rect_count: ::std::os::raw::c_uint,
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpSubtitleRectView {
    pub type_: UpSubtitleRectType,
    pub x: ::std::os::raw::c_int,
    pub y: ::std::os::raw::c_int,
    pub width: ::std::os::raw::c_int,
    pub height: ::std::os::raw::c_int,
    pub line_size: ::std::os::raw::c_int,
    pub color_count: ::std::os::raw::c_int,
    pub pixels: *const u8,
    pub palette: *const u8,
    pub text: *const ::std::os::raw::c_char,
}
unsafe extern "C" {
    pub fn up_av_error_string(
        code: ::std::os::raw::c_int,
        buffer: *mut ::std::os::raw::c_char,
        buffer_size: usize,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_format_open(
        format: *mut *mut UpAvFormat,
        path: *const ::std::os::raw::c_char,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_format_find_stream_info(format: *mut UpAvFormat) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_format_close(format: *mut *mut UpAvFormat);
}
unsafe extern "C" {
    pub fn up_av_find_best_stream(
        format: *mut UpAvFormat,
        type_: UpMediaType,
        related_stream: ::std::os::raw::c_int,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_stream_count(format: *const UpAvFormat) -> ::std::os::raw::c_uint;
}
unsafe extern "C" {
    pub fn up_av_stream_type(
        format: *const UpAvFormat,
        stream_index: ::std::os::raw::c_uint,
    ) -> UpMediaType;
}
unsafe extern "C" {
    pub fn up_av_stream_is_default(
        format: *const UpAvFormat,
        stream_index: ::std::os::raw::c_uint,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_stream_codec_name(
        format: *const UpAvFormat,
        stream_index: ::std::os::raw::c_uint,
    ) -> *const ::std::os::raw::c_char;
}
unsafe extern "C" {
    pub fn up_av_stream_metadata(
        format: *const UpAvFormat,
        stream_index: ::std::os::raw::c_uint,
        key: *const ::std::os::raw::c_char,
    ) -> *const ::std::os::raw::c_char;
}
unsafe extern "C" {
    pub fn up_av_format_duration(format: *const UpAvFormat) -> f64;
}
unsafe extern "C" {
    pub fn up_av_read_frame(
        format: *mut UpAvFormat,
        packet: *mut UpAvPacket,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_seek(
        format: *mut UpAvFormat,
        stream_index: ::std::os::raw::c_int,
        target_seconds: f64,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_decoder_open(
        format: *mut UpAvFormat,
        stream_index: ::std::os::raw::c_int,
        vulkan_device: *mut ::std::os::raw::c_void,
        prefer_vulkan: ::std::os::raw::c_int,
    ) -> *mut UpAvDecoder;
}
unsafe extern "C" {
    pub fn up_av_decoder_error() -> *const ::std::os::raw::c_char;
}
unsafe extern "C" {
    pub fn up_av_decoder_free(decoder: *mut *mut UpAvDecoder);
}
unsafe extern "C" {
    pub fn up_av_decoder_stream_index(decoder: *const UpAvDecoder) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_decoder_time_base(decoder: *const UpAvDecoder) -> f64;
}
unsafe extern "C" {
    pub fn up_av_decoder_uses_vulkan(decoder: *const UpAvDecoder) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_decoder_width(decoder: *const UpAvDecoder) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_decoder_height(decoder: *const UpAvDecoder) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_decoder_send_packet(
        decoder: *mut UpAvDecoder,
        packet: *const UpAvPacket,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_decoder_receive_frame(
        decoder: *mut UpAvDecoder,
        frame: *mut *mut UpAvFrame,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_decoder_flush(decoder: *mut UpAvDecoder);
}
unsafe extern "C" {
    pub fn up_av_packet_alloc() -> *mut UpAvPacket;
}
unsafe extern "C" {
    pub fn up_av_packet_free(packet: *mut *mut UpAvPacket);
}
unsafe extern "C" {
    pub fn up_av_packet_unref(packet: *mut UpAvPacket);
}
unsafe extern "C" {
    pub fn up_av_packet_stream_index(packet: *const UpAvPacket) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_packet_pts(packet: *const UpAvPacket) -> i64;
}
unsafe extern "C" {
    pub fn up_av_packet_duration(packet: *const UpAvPacket) -> i64;
}
unsafe extern "C" {
    pub fn up_av_frame_free(frame: *mut *mut UpAvFrame);
}
unsafe extern "C" {
    pub fn up_av_frame_is_vulkan(frame: *const UpAvFrame) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_frame_timestamp(frame: *const UpAvFrame) -> i64;
}
unsafe extern "C" {
    pub fn up_av_frame_duration(frame: *const UpAvFrame) -> i64;
}
unsafe extern "C" {
    pub fn up_av_video_info(
        format: *const UpAvFormat,
        decoder: *const UpAvDecoder,
        frame: *const UpAvFrame,
        info: *mut UpVideoInfo,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_audio_converter_create(
        frame: *const UpAvFrame,
        output_rate: ::std::os::raw::c_int,
        output_channels: ::std::os::raw::c_int,
        error: *mut ::std::os::raw::c_int,
    ) -> *mut UpAvAudioConverter;
}
unsafe extern "C" {
    pub fn up_av_audio_converter_free(converter: *mut *mut UpAvAudioConverter);
}
unsafe extern "C" {
    pub fn up_av_audio_converter_capacity(
        converter: *mut UpAvAudioConverter,
        frame: *const UpAvFrame,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_audio_converter_convert(
        converter: *mut UpAvAudioConverter,
        frame: *const UpAvFrame,
        output: *mut f32,
        output_frames: ::std::os::raw::c_int,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_av_decode_subtitle(
        decoder: *mut UpAvDecoder,
        packet: *const UpAvPacket,
        result: *mut ::std::os::raw::c_int,
    ) -> *mut UpAvSubtitle;
}
unsafe extern "C" {
    pub fn up_av_subtitle_free(subtitle: *mut *mut UpAvSubtitle);
}
unsafe extern "C" {
    pub fn up_av_subtitle_info(subtitle: *const UpAvSubtitle, info: *mut UpSubtitleInfo);
}
unsafe extern "C" {
    pub fn up_av_subtitle_rect(
        subtitle: *const UpAvSubtitle,
        index: ::std::os::raw::c_uint,
        view: *mut UpSubtitleRectView,
    ) -> ::std::os::raw::c_int;
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpWindow {
    _unused: [u8; 0],
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpAudioStream {
    _unused: [u8; 0],
}
pub const UpEventType_UP_EVENT_NONE: UpEventType = 0;
pub const UpEventType_UP_EVENT_QUIT: UpEventType = 1;
pub const UpEventType_UP_EVENT_WINDOW_CLOSE: UpEventType = 2;
pub const UpEventType_UP_EVENT_WINDOW_RESIZED: UpEventType = 3;
pub const UpEventType_UP_EVENT_WINDOW_EXPOSED: UpEventType = 4;
pub const UpEventType_UP_EVENT_WINDOW_FOCUS_GAINED: UpEventType = 5;
pub const UpEventType_UP_EVENT_WINDOW_FOCUS_LOST: UpEventType = 6;
pub const UpEventType_UP_EVENT_MOUSE_MOTION: UpEventType = 7;
pub const UpEventType_UP_EVENT_MOUSE_BUTTON_DOWN: UpEventType = 8;
pub const UpEventType_UP_EVENT_MOUSE_BUTTON_UP: UpEventType = 9;
pub const UpEventType_UP_EVENT_KEY_DOWN: UpEventType = 10;
pub type UpEventType = ::std::os::raw::c_uint;
pub const UpKey_UP_KEY_OTHER: UpKey = 0;
pub const UpKey_UP_KEY_Q: UpKey = 1;
pub const UpKey_UP_KEY_J: UpKey = 2;
pub const UpKey_UP_KEY_LEFT: UpKey = 3;
pub const UpKey_UP_KEY_RIGHT: UpKey = 4;
pub const UpKey_UP_KEY_F: UpKey = 5;
pub const UpKey_UP_KEY_I: UpKey = 6;
pub const UpKey_UP_KEY_SPACE: UpKey = 7;
pub const UpKey_UP_KEY_S: UpKey = 8;
pub const UpKey_UP_KEY_A: UpKey = 9;
pub type UpKey = ::std::os::raw::c_uint;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpEvent {
    pub type_: UpEventType,
    pub key: UpKey,
    pub repeat: ::std::os::raw::c_int,
    pub x: f32,
    pub y: f32,
    pub button: u8,
    pub clicks: u8,
}
unsafe extern "C" {
    pub fn up_platform_init() -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_platform_quit();
}
unsafe extern "C" {
    pub fn up_platform_error() -> *const ::std::os::raw::c_char;
}
unsafe extern "C" {
    pub fn up_platform_delay(milliseconds: u32);
}
unsafe extern "C" {
    pub fn up_platform_poll_event(event: *mut UpEvent) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_platform_capture_mouse(captured: ::std::os::raw::c_int);
}
unsafe extern "C" {
    pub fn up_window_create(
        title: *const ::std::os::raw::c_char,
        width: ::std::os::raw::c_int,
        height: ::std::os::raw::c_int,
    ) -> *mut UpWindow;
}
unsafe extern "C" {
    pub fn up_window_destroy(window: *mut UpWindow);
}
unsafe extern "C" {
    pub fn up_window_size(
        window: *mut UpWindow,
        width: *mut ::std::os::raw::c_int,
        height: *mut ::std::os::raw::c_int,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_window_pixel_size(
        window: *mut UpWindow,
        width: *mut ::std::os::raw::c_int,
        height: *mut ::std::os::raw::c_int,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_window_set_minimum_size(
        window: *mut UpWindow,
        width: ::std::os::raw::c_int,
        height: ::std::os::raw::c_int,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_window_set_fullscreen(
        window: *mut UpWindow,
        fullscreen: ::std::os::raw::c_int,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_audio_stream_create(
        rate: ::std::os::raw::c_int,
        channels: ::std::os::raw::c_int,
    ) -> *mut UpAudioStream;
}
unsafe extern "C" {
    pub fn up_audio_stream_destroy(stream: *mut UpAudioStream);
}
unsafe extern "C" {
    pub fn up_audio_stream_put(
        stream: *mut UpAudioStream,
        data: *const ::std::os::raw::c_void,
        bytes: ::std::os::raw::c_int,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_audio_stream_queued(stream: *mut UpAudioStream) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_audio_stream_resume(stream: *mut UpAudioStream) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_audio_stream_pause(stream: *mut UpAudioStream) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_audio_stream_clear(stream: *mut UpAudioStream) -> ::std::os::raw::c_int;
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpVideoRenderer {
    _unused: [u8; 0],
}
unsafe extern "C" {
    pub fn up_video_renderer_create(window: *mut ::std::os::raw::c_void) -> *mut UpVideoRenderer;
}
unsafe extern "C" {
    pub fn up_video_renderer_device(renderer: *mut UpVideoRenderer) -> *mut ::std::os::raw::c_void;
}
unsafe extern "C" {
    pub fn up_video_renderer_display(
        renderer: *mut UpVideoRenderer,
        frame: *mut ::std::os::raw::c_void,
        width: ::std::os::raw::c_int,
        height: ::std::os::raw::c_int,
        top_bar_alpha: f32,
        title: *const ::std::os::raw::c_char,
        info: *const ::std::os::raw::c_char,
        info_alpha: f32,
        details: *const ::std::os::raw::c_char,
        position: *const ::std::os::raw::c_char,
        position_alpha: f32,
        scrubber_progress: f32,
        scrubber_alpha: f32,
        subtitle_text: *const ::std::os::raw::c_char,
        subtitle_pixels: *const u8,
        subtitle_width: ::std::os::raw::c_int,
        subtitle_height: ::std::os::raw::c_int,
        subtitle_serial: u64,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_video_renderer_resize(
        renderer: *mut UpVideoRenderer,
        width: ::std::os::raw::c_int,
        height: ::std::os::raw::c_int,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_video_renderer_error(
        renderer: *const UpVideoRenderer,
    ) -> *const ::std::os::raw::c_char;
}
unsafe extern "C" {
    pub fn up_video_renderer_destroy(renderer: *mut UpVideoRenderer);
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpWaylandInput {
    _unused: [u8; 0],
}
unsafe extern "C" {
    pub fn up_wayland_input_create(window: *mut ::std::os::raw::c_void) -> *mut UpWaylandInput;
}
unsafe extern "C" {
    pub fn up_wayland_input_ready(input: *const UpWaylandInput) -> bool;
}
unsafe extern "C" {
    pub fn up_wayland_input_error(input: *const UpWaylandInput) -> *const ::std::os::raw::c_char;
}
unsafe extern "C" {
    pub fn up_wayland_input_destroy(input: *mut UpWaylandInput);
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UpMpris {
    _unused: [u8; 0],
}
pub const UpMprisCommand_UP_MPRIS_COMMAND_NONE: UpMprisCommand = 0;
pub const UpMprisCommand_UP_MPRIS_COMMAND_QUIT: UpMprisCommand = 1;
pub const UpMprisCommand_UP_MPRIS_COMMAND_PLAY: UpMprisCommand = 2;
pub const UpMprisCommand_UP_MPRIS_COMMAND_PAUSE: UpMprisCommand = 3;
pub const UpMprisCommand_UP_MPRIS_COMMAND_PLAY_PAUSE: UpMprisCommand = 4;
pub const UpMprisCommand_UP_MPRIS_COMMAND_STOP: UpMprisCommand = 5;
pub const UpMprisCommand_UP_MPRIS_COMMAND_SEEK: UpMprisCommand = 6;
pub const UpMprisCommand_UP_MPRIS_COMMAND_SET_POSITION: UpMprisCommand = 7;
pub type UpMprisCommand = ::std::os::raw::c_uint;
pub const UpMprisStatus_UP_MPRIS_STATUS_PLAYING: UpMprisStatus = 0;
pub const UpMprisStatus_UP_MPRIS_STATUS_PAUSED: UpMprisStatus = 1;
pub const UpMprisStatus_UP_MPRIS_STATUS_STOPPED: UpMprisStatus = 2;
pub type UpMprisStatus = ::std::os::raw::c_uint;
unsafe extern "C" {
    pub fn up_mpris_create(
        title: *const ::std::os::raw::c_char,
        filename: *const ::std::os::raw::c_char,
        duration_us: i64,
    ) -> *mut UpMpris;
}
unsafe extern "C" {
    pub fn up_mpris_active(mpris: *const UpMpris) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn up_mpris_error(mpris: *const UpMpris) -> *const ::std::os::raw::c_char;
}
unsafe extern "C" {
    pub fn up_mpris_dispatch(mpris: *mut UpMpris);
}
unsafe extern "C" {
    pub fn up_mpris_take_command(mpris: *mut UpMpris, value: *mut i64) -> UpMprisCommand;
}
unsafe extern "C" {
    pub fn up_mpris_update(mpris: *mut UpMpris, status: UpMprisStatus, position_us: i64);
}
unsafe extern "C" {
    pub fn up_mpris_seeked(mpris: *mut UpMpris, position_us: i64);
}
unsafe extern "C" {
    pub fn up_mpris_destroy(mpris: *mut UpMpris);
}
