use crate::audio::{AUDIO_QUEUE_TARGET_BYTES, AudioControl, AudioOutput};
use crate::clock::completed_seek_anchor;
use crate::decoder::{
    Decoder, VideoFrame, ffmpeg_error, ffmpeg_error_is_again, ffmpeg_error_is_eof,
};
use crate::metadata::TrackLabel;
use crate::source::MediaInput;
use crate::subtitles::{SubtitleContent, SubtitleCue, subtitle_dialogue_text};
use crate::{Result, ffi};
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) const VIDEO_QUEUE_TARGET: usize = 16;
pub(crate) const VIDEO_QUEUE_MAX: usize = 24;
pub(crate) const AV_NOPTS_VALUE: i64 = i64::MIN;
const VIDEO_PACKET_MAX: usize = 256;
const VIDEO_PACKET_MAX_BYTES: usize = 32 * 1024 * 1024;

struct VideoPacket(*mut ffi::UpAvPacket);

impl Drop for VideoPacket {
    fn drop(&mut self) {
        unsafe { ffi::up_av_packet_free(&mut self.0) };
    }
}

fn queue_capacity(width: i32, height: i32) -> usize {
    // Bound decoded-surface memory while retaining a short presentation lead.
    let bytes = (width.max(1) as u64) * (height.max(1) as u64) * 4;
    ((256 * 1024 * 1024 / bytes) as usize).clamp(4, VIDEO_QUEUE_MAX)
}

unsafe extern "C" fn interrupt_when_stopped(opaque: *mut c_void) -> i32 {
    let running = unsafe { &*opaque.cast::<AtomicBool>() };
    i32::from(!running.load(Ordering::Acquire))
}

pub(crate) struct MediaTrack {
    pub(crate) decoder: Decoder,
    pub(crate) label: TrackLabel,
}

#[derive(Debug)]
pub(crate) enum AudioTrackError {
    // The source rejected the seek; the previous track remains selected.
    Unavailable(String),
    // Seeking succeeded, but resetting playback failed. State cannot be reused.
    Playback(String),
}

pub(crate) struct Media {
    pub(crate) format: *mut ffi::UpAvFormat,
    interrupt: Arc<AtomicBool>,
    audio_control: AudioControl,
    pub(crate) packet: *mut ffi::UpAvPacket,
    pub(crate) video: Decoder,
    interlace_detector: crate::deinterlace::Detector,
    pub(crate) audio_tracks: Vec<MediaTrack>,
    pub(crate) selected_audio_track: usize,
    pub(crate) audio: Option<AudioOutput>,
    pub(crate) subtitle_tracks: Vec<MediaTrack>,
    pub(crate) video_queue: VecDeque<VideoFrame>,
    video_packets: VecDeque<VideoPacket>,
    video_packet_bytes: usize,
    input_eof: bool,
    video_catching_up: bool,
    pub(crate) subtitle_queue: VecDeque<SubtitleCue>,
    pub(crate) subtitle_serial: u64,
    pub(crate) log_subtitles: bool,
    pub(crate) eof: bool,
    pub(crate) drained: bool,
    pub(crate) first_video_pts: Option<f64>,
    pub(crate) next_video_pts: f64,
    pub(crate) video_seek_target: Option<f64>,
    pub(crate) audio_seek_target: Option<f64>,
    pub(crate) subtitle_seek_target: Option<f64>,
}

impl Media {
    pub(crate) unsafe fn open<S: MediaInput + ?Sized>(
        source: &S,
        hardware_device: Option<*mut c_void>,
        log_subtitles: bool,
    ) -> Result<Self> {
        unsafe {
            Self::open_with_controls(
                source,
                hardware_device,
                log_subtitles,
                Arc::new(AtomicBool::new(true)),
                AudioControl::default(),
            )
        }
    }

    pub(crate) unsafe fn open_with_controls<S: MediaInput + ?Sized>(
        source: &S,
        hardware_device: Option<*mut c_void>,
        log_subtitles: bool,
        interrupt: Arc<AtomicBool>,
        audio_control: AudioControl,
    ) -> Result<Self> {
        let mut selected_hls = 0;
        let result = unsafe {
            Self::open_input(
                source,
                hardware_device,
                log_subtitles,
                Arc::clone(&interrupt),
                audio_control.clone(),
                true,
                &mut selected_hls,
            )
        };
        if result.is_err() && selected_hls != 0 && interrupt.load(Ordering::Acquire) {
            eprintln!("warning: selected HLS rendition failed; retrying full stream discovery");
            return unsafe {
                Self::open_input(
                    source,
                    hardware_device,
                    log_subtitles,
                    interrupt,
                    audio_control,
                    false,
                    &mut selected_hls,
                )
            };
        }
        result
    }

    unsafe fn open_input<S: MediaInput + ?Sized>(
        source: &S,
        hardware_device: Option<*mut c_void>,
        log_subtitles: bool,
        interrupt: Arc<AtomicBool>,
        audio_control: AudioControl,
        select_hls: bool,
        selected_hls: &mut i32,
    ) -> Result<Self> {
        let opened_at = std::time::Instant::now();
        let input = CString::new(source.input_bytes())
            .map_err(|_| "media source contains a NUL byte".to_string())?;
        let mut format = ptr::null_mut();
        let ret = unsafe {
            ffi::up_av_format_open(
                &mut format,
                input.as_ptr(),
                i32::from(source.is_network_input()),
                Arc::as_ptr(&interrupt).cast_mut().cast(),
                Some(interrupt_when_stopped),
                i32::from(select_hls),
                selected_hls,
            )
        };
        if ret < 0 {
            return Err(format!("could not open media: {}", unsafe {
                ffmpeg_error(ret)
            }));
        }

        if log_subtitles {
            eprintln!(
                "perf: input opened in {:.2} s",
                opened_at.elapsed().as_secs_f64()
            );
        }
        let result = (|| {
            let inspected_at = std::time::Instant::now();
            let ret = unsafe { ffi::up_av_format_find_stream_info(format) };
            if log_subtitles {
                eprintln!(
                    "perf: streams inspected in {:.2} s",
                    inspected_at.elapsed().as_secs_f64()
                );
            }
            if ret < 0 {
                return Err(format!("could not inspect media streams: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }

            let mut video_rank = 0;
            let (video_index, video) = loop {
                let video_index = unsafe { ffi::up_av_select_video_stream(format, video_rank) };
                if video_index < 0 {
                    return Err("the input has no usable video stream".into());
                }
                let result = match unsafe { Decoder::open(format, video_index, hardware_device) } {
                    Ok(video) => Ok(video),
                    Err(hardware_error) if hardware_device.is_some() => {
                        eprintln!(
                            "warning: hardware decoder initialization failed ({hardware_error}); trying software decoding"
                        );
                        unsafe { Decoder::open(format, video_index, None) }
                    }
                    Err(error) => Err(error),
                };
                match result {
                    Ok(video) => break (video_index, video),
                    Err(error) => {
                        video_rank += 1;
                        if unsafe { ffi::up_av_select_video_stream(format, video_rank) } < 0 {
                            return Err(error);
                        }
                        eprintln!(
                            "warning: HLS video rendition {video_index} is unavailable ({error}); trying the next lower-quality rendition"
                        );
                    }
                }
            };
            let video_name =
                unsafe { CStr::from_ptr(ffi::up_av_stream_codec_name(format, video_index as u32)) }
                    .to_string_lossy();
            let decode_path = if video.uses_hardware {
                if cfg!(target_os = "macos") {
                    "VideoToolbox hardware decode, Metal presentation"
                } else {
                    "Vulkan Video"
                }
            } else if cfg!(target_os = "macos") {
                "software decode, Metal presentation"
            } else {
                "software decode, Vulkan presentation"
            };
            eprintln!(
                "video: {video_name} {}x{} via {decode_path}",
                unsafe { ffi::up_av_decoder_width(video.as_ptr()) },
                unsafe { ffi::up_av_decoder_height(video.as_ptr()) }
            );

            let preferred_audio_index = unsafe {
                ffi::up_av_find_best_stream(
                    format,
                    ffi::UpMediaType_UP_MEDIA_TYPE_AUDIO,
                    video_index,
                )
            };
            let mut audio_indices = (0..unsafe { ffi::up_av_stream_count(format) } as usize)
                .filter(|&index| unsafe {
                    ffi::up_av_stream_type(format, index as u32)
                        == ffi::UpMediaType_UP_MEDIA_TYPE_AUDIO
                })
                .collect::<Vec<_>>();
            audio_indices.sort_by_key(|&index| index as i32 != preferred_audio_index);
            let mut audio_tracks = Vec::new();
            for stream_index in audio_indices {
                let label = unsafe { TrackLabel::inspect(format, stream_index as u32) };
                match unsafe { Decoder::open(format, stream_index as i32, None) } {
                    Ok(decoder) => {
                        eprintln!(
                            "audio track {}: {}{}{}",
                            audio_tracks.len() + 1,
                            label.codec,
                            label
                                .language
                                .as_deref()
                                .map_or_else(String::new, |value| format!(" [{value}]")),
                            label
                                .title
                                .as_deref()
                                .map_or_else(String::new, |value| format!(" {value}")),
                        );
                        audio_tracks.push(MediaTrack { decoder, label });
                    }
                    Err(error) => eprintln!(
                        "audio stream {stream_index} ({}) unavailable: {error}",
                        label.codec
                    ),
                }
            }
            let audio = if audio_tracks.is_empty() {
                None
            } else {
                if audio_tracks.len() > 1 {
                    eprintln!("audio: A switches tracks");
                }
                Some(unsafe { AudioOutput::create(audio_control.clone())? })
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
            let mut subtitle_tracks = Vec::new();
            for stream_index in subtitle_indices {
                let label = unsafe { TrackLabel::inspect(format, stream_index as u32) };
                match unsafe { Decoder::open(format, stream_index as i32, None) } {
                    Ok(decoder) => {
                        eprintln!(
                            "subtitle track {}: {}{}{}",
                            subtitle_tracks.len() + 1,
                            label.codec,
                            label
                                .language
                                .as_deref()
                                .map_or_else(String::new, |value| format!(" [{value}]")),
                            label
                                .title
                                .as_deref()
                                .map_or_else(String::new, |value| format!(" {value}")),
                        );
                        subtitle_tracks.push(MediaTrack { decoder, label });
                    }
                    Err(error) => eprintln!(
                        "subtitle stream {stream_index} ({}) unavailable: {error}",
                        label.codec
                    ),
                }
            }
            if !subtitle_tracks.is_empty() {
                eprintln!("subtitles: S toggles, J switches tracks");
            }

            unsafe {
                ffi::up_av_select_audio_stream(
                    format,
                    audio_tracks
                        .first()
                        .map_or(-1, |track| track.decoder.stream_index),
                );
            }
            let ret = unsafe { ffi::up_av_hls_finish_discovery(format, video_index) };
            if ret < 0 {
                return Err(format!(
                    "could not restart selected HLS streams: {}",
                    unsafe { ffmpeg_error(ret) }
                ));
            }
            let packet = unsafe { ffi::up_av_packet_alloc() };
            if packet.is_null() {
                return Err("out of memory while allocating a packet".into());
            }

            Ok(Self {
                format,
                interrupt,
                audio_control,
                packet,
                video,
                interlace_detector: crate::deinterlace::Detector::default(),
                audio_tracks,
                selected_audio_track: 0,
                audio,
                subtitle_tracks,
                video_queue: VecDeque::new(),
                video_packets: VecDeque::new(),
                video_packet_bytes: 0,
                input_eof: false,
                video_catching_up: false,
                subtitle_queue: VecDeque::new(),
                subtitle_serial: 0,
                log_subtitles,
                eof: false,
                drained: false,
                first_video_pts: None,
                next_video_pts: 0.0,
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

    pub(crate) fn interrupt_token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.interrupt)
    }

    pub(crate) fn audio_control(&self) -> AudioControl {
        self.audio_control.clone()
    }

    pub(crate) unsafe fn receive_video(&mut self) -> Result<()> {
        loop {
            let mut frame = ptr::null_mut();
            let ret = unsafe { ffi::up_av_decoder_receive_frame(self.video.as_ptr(), &mut frame) };
            if ret < 0 {
                if unsafe { ffmpeg_error_is_again(ret) || ffmpeg_error_is_eof(ret) } {
                    break;
                }
                return Err(format!("video decoding failed: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }
            if self.video.uses_hardware && unsafe { ffi::up_av_frame_is_hardware(frame) } == 0 {
                unsafe { ffi::up_av_frame_free(&mut frame) };
                return Err(
                    "the selected codec/profile is not supported by hardware decoding".into(),
                );
            }
            let timestamp = unsafe { ffi::up_av_frame_timestamp(frame) };
            let pts = if timestamp == AV_NOPTS_VALUE {
                self.next_video_pts
            } else {
                timestamp as f64 * self.video.time_base
            };
            let raw_duration = unsafe { ffi::up_av_frame_duration(frame) };
            let duration = if raw_duration > 0 {
                raw_duration as f64 * self.video.time_base
            } else {
                self.video.frame_duration
            };
            // Decoder timing must survive queue transfers to the render thread.
            self.next_video_pts = pts + duration;
            if self
                .video_seek_target
                .is_some_and(|target| pts + duration.max(1.0 / 120.0) < target)
            {
                unsafe { ffi::up_av_frame_free(&mut frame) };
                continue;
            }
            self.video_seek_target = None;
            self.first_video_pts.get_or_insert(pts);
            let mut frame = unsafe { VideoFrame::from_raw(frame, pts, duration) };
            frame.detected_field = self.interlace_detector.analyze(&frame);
            self.video_queue.push_back(frame);
        }
        Ok(())
    }

    pub(crate) unsafe fn receive_audio(&mut self) -> Result<()> {
        let Some(track) = self.audio_tracks.get(self.selected_audio_track) else {
            return Ok(());
        };
        let decoder = &track.decoder;
        let context = decoder.as_ptr();
        let time_base = decoder.time_base;
        loop {
            let mut frame = ptr::null_mut();
            let ret = unsafe { ffi::up_av_decoder_receive_frame(context, &mut frame) };
            if ret < 0 {
                if unsafe { ffmpeg_error_is_again(ret) || ffmpeg_error_is_eof(ret) } {
                    break;
                }
                return Err(format!("audio decoding failed: {}", unsafe {
                    ffmpeg_error(ret)
                }));
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

    pub(crate) unsafe fn decode_subtitle_packet(&mut self, track: usize) -> Result<()> {
        let decoder = &self.subtitle_tracks[track].decoder;
        let context = decoder.as_ptr();
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

            let video_width = unsafe { ffi::up_av_decoder_width(self.video.as_ptr()) };
            let video_height = unsafe { ffi::up_av_decoder_height(self.video.as_ptr()) };
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
        self.subtitle_seek_target = None;
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

    pub(crate) unsafe fn decode_packet(&mut self) -> Result<()> {
        let stream_index = unsafe { ffi::up_av_packet_stream_index(self.packet) };
        if stream_index == self.video.stream_index {
            let ret = unsafe { ffi::up_av_decoder_send_packet(self.video.as_ptr(), self.packet) };
            if ret < 0 {
                return Err(format!("video decoder rejected a packet: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }
            unsafe { self.receive_video()? };
        } else if self
            .audio_tracks
            .get(self.selected_audio_track)
            .is_some_and(|track| track.decoder.stream_index == stream_index)
        {
            let context = self.audio_tracks[self.selected_audio_track]
                .decoder
                .as_ptr();
            let ret = unsafe { ffi::up_av_decoder_send_packet(context, self.packet) };
            if ret < 0 {
                return Err(format!("audio decoder rejected a packet: {}", unsafe {
                    ffmpeg_error(ret)
                }));
            }
            unsafe { self.receive_audio()? };
        } else if let Some(track) = self
            .subtitle_tracks
            .iter()
            .position(|track| track.decoder.stream_index == stream_index)
        {
            unsafe { self.decode_subtitle_packet(track)? };
        }
        Ok(())
    }

    unsafe fn decode_buffered_video(&mut self) -> Result<()> {
        let packet = self.video_packets.pop_front().unwrap();
        self.video_packet_bytes -= unsafe { ffi::up_av_packet_size(packet.0) }.max(0) as usize;
        let timestamp = unsafe { ffi::up_av_packet_pts(packet.0) };
        let pts = (timestamp != AV_NOPTS_VALUE).then_some(timestamp as f64 * self.video.time_base);
        let audio_clock = self
            .audio
            .as_ref()
            .and_then(|audio| unsafe { audio.active_clock() });
        let late = pts
            .zip(audio_clock)
            .is_some_and(|(pts, clock)| pts < clock - 0.25);
        if late && !self.video_catching_up {
            self.video_catching_up = true;
            eprintln!("video: decoder cannot keep up; skipping to a keyframe to preserve audio");
        }
        // Once references have been skipped, resume only at a current keyframe.
        // Dropping arbitrary compressed packets would corrupt later pictures.
        if self.video_catching_up && !late && unsafe { ffi::up_av_packet_is_key(packet.0) } != 0 {
            self.video_catching_up = false;
        }
        unsafe {
            ffi::up_av_decoder_skip_nonkey(self.video.as_ptr(), i32::from(self.video_catching_up))
        };
        let ret = unsafe { ffi::up_av_decoder_send_packet(self.video.as_ptr(), packet.0) };
        if ret < 0 {
            return Err(format!(
                "video decoder rejected a buffered packet: {}",
                unsafe { ffmpeg_error(ret) }
            ));
        }
        unsafe { self.receive_video() }
    }

    pub(crate) unsafe fn drain(&mut self) -> Result<()> {
        if self.drained {
            return Ok(());
        }
        self.drained = true;
        let video_ret = unsafe { ffi::up_av_decoder_send_packet(self.video.as_ptr(), ptr::null()) };
        if video_ret < 0 && !unsafe { ffmpeg_error_is_eof(video_ret) } {
            return Err(format!("could not drain video decoder: {}", unsafe {
                ffmpeg_error(video_ret)
            }));
        }
        unsafe { self.receive_video()? };
        if let Some(track) = self.audio_tracks.get(self.selected_audio_track) {
            let audio_ret =
                unsafe { ffi::up_av_decoder_send_packet(track.decoder.as_ptr(), ptr::null()) };
            if audio_ret < 0 && !unsafe { ffmpeg_error_is_eof(audio_ret) } {
                return Err(format!("could not drain audio decoder: {}", unsafe {
                    ffmpeg_error(audio_ret)
                }));
            }
            unsafe { self.receive_audio()? };
        }
        if let Some(audio) = self.audio.as_mut() {
            unsafe { audio.drain_converter(self.audio_seek_target)? };
        }
        Ok(())
    }

    pub(crate) unsafe fn fill_queues(&mut self) -> Result<()> {
        unsafe { self.fill_queues_with_budget(None, self.video_queue_capacity()) }
    }

    pub(crate) fn video_queue_capacity(&self) -> usize {
        queue_capacity(
            unsafe { ffi::up_av_decoder_width(self.video.as_ptr()) },
            unsafe { ffi::up_av_decoder_height(self.video.as_ptr()) },
        )
    }

    // Publish frames promptly even when one high-resolution decode is slow.
    // Startup and synchronous callers can still fill their complete buffers.
    pub(crate) unsafe fn fill_queues_step(&mut self, capacity: usize) -> Result<()> {
        unsafe { self.fill_queues_with_budget(Some(std::time::Duration::from_millis(8)), capacity) }
    }

    unsafe fn fill_queues_with_budget(
        &mut self,
        budget: Option<std::time::Duration>,
        capacity: usize,
    ) -> Result<()> {
        let started = std::time::Instant::now();
        unsafe { self.sync_audio_selection() };
        if let Some(audio) = self.audio.as_mut() {
            unsafe { audio.pump()? };
        }
        if self.eof {
            return unsafe { self.drain() };
        }

        for _ in 0..96 {
            let audio_needs_data = self
                .audio
                .as_ref()
                .is_some_and(|audio| unsafe { audio.queued_bytes() } < AUDIO_QUEUE_TARGET_BYTES);
            let video_full = self.video_queue.len() >= capacity;
            let packets_full = self.video_packets.len() >= VIDEO_PACKET_MAX
                || self.video_packet_bytes >= VIDEO_PACKET_MAX_BYTES;
            // Read compressed video ahead while audio is low. Decoding an 8K
            // picture must not prevent demuxing the audio behind that packet.
            let read_audio = audio_needs_data && !self.input_eof && !packets_full;
            if video_full && !read_audio {
                break;
            }
            if !read_audio && !self.video_packets.is_empty() {
                unsafe { self.decode_buffered_video()? };
            } else if self.input_eof {
                self.eof = true;
                unsafe { self.drain()? };
                break;
            } else {
                if unsafe { self.audio_full() }
                    || (!audio_needs_data
                        && self.video_queue.len() >= VIDEO_QUEUE_TARGET.min(capacity))
                {
                    break;
                }
                let ret = unsafe { ffi::up_av_read_frame(self.format, self.packet) };
                if ret < 0 {
                    if unsafe { ffmpeg_error_is_again(ret) } {
                        break;
                    }
                    if !unsafe { ffmpeg_error_is_eof(ret) } {
                        return Err(format!("could not read media packet: {}", unsafe {
                            ffmpeg_error(ret)
                        }));
                    }
                    self.input_eof = true;
                    continue;
                }
                let decode = if unsafe { ffi::up_av_packet_stream_index(self.packet) }
                    == self.video.stream_index
                    && self.audio.is_some()
                {
                    let packet = unsafe { ffi::up_av_packet_clone(self.packet) };
                    if packet.is_null() {
                        Err("out of memory while buffering video packets".into())
                    } else {
                        self.video_packet_bytes +=
                            unsafe { ffi::up_av_packet_size(packet) }.max(0) as usize;
                        self.video_packets.push_back(VideoPacket(packet));
                        Ok(())
                    }
                } else {
                    unsafe { self.decode_packet() }
                };
                unsafe { ffi::up_av_packet_unref(self.packet) };
                decode?;
            }
            if budget.is_some_and(|budget| started.elapsed() >= budget) {
                break;
            }
        }
        Ok(())
    }

    pub(crate) unsafe fn fill_initial_queues(&mut self) -> Result<()> {
        while self.video_queue.is_empty() && !self.eof && !unsafe { self.audio_full() } {
            unsafe { self.fill_queues()? };
        }
        Ok(())
    }

    pub(crate) unsafe fn audio_full(&self) -> bool {
        self.audio
            .as_ref()
            .is_some_and(|audio| unsafe { audio.full() })
    }

    pub(crate) unsafe fn sync_audio(&mut self, playback_time: f64, paused: bool) -> Result<()> {
        if let Some(audio) = self.audio.as_mut() {
            unsafe { audio.pump()? };
            // A delayed track may be buffered well before its first timestamp.
            // Keep it paused until the video clock reaches that point.
            let due = audio.first_pts.is_some_and(|pts| pts <= playback_time);
            unsafe { audio.set_paused(paused || !due)? };
        }
        Ok(())
    }

    pub(crate) fn duration(&self) -> Option<f64> {
        let duration = unsafe { ffi::up_av_format_duration(self.format) };
        (duration.is_finite() && duration > 0.0).then_some(duration)
    }

    pub(crate) unsafe fn seek(&mut self, requested_target: f64) -> Result<f64> {
        unsafe { self.seek_input(requested_target)? };
        unsafe { self.reset_after_seek(requested_target) }
    }

    unsafe fn seek_input(&mut self, requested_target: f64) -> Result<()> {
        unsafe { self.sync_audio_selection() };
        if self.video.time_base <= 0.0 {
            return Err("video stream has an invalid time base".into());
        }
        // av_seek_frame with AVSEEK_FLAG_BACKWARD resolves the usable keyframe
        // for the requested timestamp. Do not preselect a container index
        // entry here: sparse Matroska cues can be many seconds behind it.
        let ret =
            unsafe { ffi::up_av_seek(self.format, self.video.stream_index, requested_target) };
        if ret < 0 {
            return Err(format!("could not seek: {}", unsafe { ffmpeg_error(ret) }));
        }
        Ok(())
    }

    unsafe fn reset_after_seek(&mut self, requested_target: f64) -> Result<f64> {
        unsafe {
            ffi::up_av_packet_unref(self.packet);
            ffi::up_av_decoder_flush(self.video.as_ptr());
            ffi::up_av_decoder_skip_nonkey(self.video.as_ptr(), 0);
        }
        for track in &self.audio_tracks {
            unsafe { ffi::up_av_decoder_flush(track.decoder.as_ptr()) };
        }
        for track in &self.subtitle_tracks {
            unsafe { ffi::up_av_decoder_flush(track.decoder.as_ptr()) };
        }
        if let Some(audio) = self.audio.as_mut() {
            unsafe {
                audio.set_paused(true)?;
                audio.reset()?;
            }
        }
        self.video_queue.clear();
        self.video_packets.clear();
        self.video_packet_bytes = 0;
        self.input_eof = false;
        self.video_catching_up = false;
        self.interlace_detector = crate::deinterlace::Detector::default();
        self.subtitle_queue.clear();
        self.eof = false;
        self.drained = false;
        let playback_target = requested_target;
        self.next_video_pts = playback_target;
        self.video_seek_target = Some(playback_target);
        self.audio_seek_target = self.audio.as_ref().map(|_| playback_target);
        self.subtitle_seek_target = (!self.subtitle_tracks.is_empty()).then_some(playback_target);
        Ok(playback_target)
    }

    unsafe fn sync_audio_selection(&self) {
        let index = self
            .audio_tracks
            .get(self.selected_audio_track)
            .map_or(-1, |track| track.decoder.stream_index);
        unsafe { ffi::up_av_select_audio_stream(self.format, index) };
    }

    // Startup selection must happen before any audio is queued. No seek is
    // necessary, including for non-seekable sources.
    pub(crate) unsafe fn prefer_initial_audio_language(&mut self, language: &str) {
        if let Some(index) = crate::language::preferred_track(
            self.audio_tracks
                .iter()
                .map(|track| track.label.language.as_deref()),
            language,
        ) {
            self.selected_audio_track = index;
            unsafe { self.sync_audio_selection() };
        }
    }

    pub(crate) unsafe fn select_audio_track(
        &mut self,
        track: usize,
        playback_target: f64,
    ) -> std::result::Result<f64, AudioTrackError> {
        if track >= self.audio_tracks.len() {
            return Err(AudioTrackError::Unavailable("invalid audio track".into()));
        }
        let previous = self.selected_audio_track;
        self.selected_audio_track = track;
        if let Err(error) = unsafe { self.seek_input(playback_target) } {
            self.selected_audio_track = previous;
            unsafe { self.sync_audio_selection() };
            return Err(AudioTrackError::Unavailable(error));
        }
        let target =
            unsafe { self.reset_after_seek(playback_target) }.map_err(AudioTrackError::Playback)?;
        let label = &self.audio_tracks[track].label;
        eprintln!("audio: track {} selected ({})", track + 1, label.codec);
        Ok(target)
    }

    pub(crate) unsafe fn audio_clock(&self) -> Option<f64> {
        self.audio
            .as_ref()
            .and_then(|audio| unsafe { audio.clock() })
    }

    pub(crate) unsafe fn finish_seek(
        &self,
        video_pts: Option<f64>,
        outstanding_video: usize,
        fallback: f64,
    ) -> Option<f64> {
        let audio_clock = unsafe { self.audio_clock() };
        completed_seek_anchor(
            video_pts,
            audio_clock,
            self.audio.is_some(),
            self.eof,
            // The worker shares its capacity with frames already handed to
            // playback. Those frames also block further demux/decode work.
            self.video_queue.len().saturating_add(outstanding_video)
                >= VIDEO_QUEUE_TARGET.min(self.video_queue_capacity())
                || unsafe { self.audio_full() },
            fallback,
        )
    }

    pub(crate) unsafe fn audio_empty(&self) -> bool {
        self.audio
            .as_ref()
            .is_none_or(|audio| audio.pending.is_empty() && unsafe { audio.queued_bytes() } == 0)
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

#[cfg(test)]
mod interrupt_tests {
    use super::*;

    #[test]
    fn ffmpeg_interrupt_tracks_worker_lifetime() {
        let running = AtomicBool::new(true);
        let opaque = (&running as *const AtomicBool).cast_mut().cast();
        assert_eq!(unsafe { interrupt_when_stopped(opaque) }, 0);
        running.store(false, Ordering::Release);
        assert_eq!(unsafe { interrupt_when_stopped(opaque) }, 1);
    }
}
