use crate::audio::{AUDIO_QUEUE_TARGET_BYTES, AudioOutput};
use crate::clock::completed_seek_anchor;
use crate::decoder::{
    Decoder, VideoFrame, ffmpeg_error, ffmpeg_error_is_again, ffmpeg_error_is_eof,
};
use crate::metadata::TrackLabel;
use crate::subtitles::{SubtitleContent, SubtitleCue, subtitle_dialogue_text};
use crate::{Result, ffi};
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_void};
use std::path::Path;
use std::ptr;

pub(crate) const VIDEO_QUEUE_TARGET: usize = 16;
pub(crate) const VIDEO_QUEUE_MAX: usize = 24;
pub(crate) const AV_NOPTS_VALUE: i64 = i64::MIN;

pub(crate) struct MediaTrack {
    pub(crate) decoder: Decoder,
    pub(crate) label: TrackLabel,
}

pub(crate) struct Media {
    pub(crate) format: *mut ffi::UpAvFormat,
    pub(crate) packet: *mut ffi::UpAvPacket,
    pub(crate) video: Decoder,
    pub(crate) audio_tracks: Vec<MediaTrack>,
    pub(crate) selected_audio_track: usize,
    pub(crate) audio: Option<AudioOutput>,
    pub(crate) subtitle_tracks: Vec<MediaTrack>,
    pub(crate) video_queue: VecDeque<VideoFrame>,
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
    pub(crate) unsafe fn open(
        path: &Path,
        vulkan_device: Option<*mut c_void>,
        log_subtitles: bool,
    ) -> Result<Self> {
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
            let video = match unsafe { Decoder::open(format, video_index, vulkan_device) } {
                Ok(video) => video,
                Err(hardware_error) if vulkan_device.is_some() => {
                    eprintln!(
                        "warning: Vulkan decoder initialization failed ({hardware_error}); trying software decoding"
                    );
                    unsafe { Decoder::open(format, video_index, None)? }
                }
                Err(error) => return Err(error),
            };
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
                eprintln!("audio: track 1 selected via PipeWire");
                if audio_tracks.len() > 1 {
                    eprintln!("audio: A switches tracks");
                }
                Some(unsafe { AudioOutput::create()? })
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

            let packet = unsafe { ffi::up_av_packet_alloc() };
            if packet.is_null() {
                return Err("out of memory while allocating a packet".into());
            }

            Ok(Self {
                format,
                packet,
                video,
                audio_tracks,
                selected_audio_track: 0,
                audio,
                subtitle_tracks,
                video_queue: VecDeque::new(),
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
            if self.video.uses_vulkan && unsafe { ffi::up_av_frame_is_vulkan(frame) } == 0 {
                unsafe { ffi::up_av_frame_free(&mut frame) };
                return Err("the selected codec/profile is not supported by Vulkan Video".into());
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
            self.video_queue
                .push_back(unsafe { VideoFrame::from_raw(frame, pts, duration) });
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
            if unsafe { self.audio_full() }
                || self.video_queue.len() >= VIDEO_QUEUE_MAX
                || (self.video_queue.len() >= VIDEO_QUEUE_TARGET && !audio_needs_data)
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

        unsafe {
            ffi::up_av_packet_unref(self.packet);
            ffi::up_av_decoder_flush(self.video.as_ptr());
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

    pub(crate) unsafe fn select_audio_track(
        &mut self,
        track: usize,
        playback_target: f64,
    ) -> Result<f64> {
        if track >= self.audio_tracks.len() {
            return Err("invalid audio track".into());
        }
        let previous = self.selected_audio_track;
        self.selected_audio_track = track;
        match unsafe { self.seek(playback_target) } {
            Ok(target) => {
                let label = &self.audio_tracks[track].label;
                eprintln!("audio: track {} selected ({})", track + 1, label.codec);
                Ok(target)
            }
            Err(error) => {
                self.selected_audio_track = previous;
                Err(error)
            }
        }
    }

    pub(crate) unsafe fn audio_clock(&self) -> Option<f64> {
        self.audio
            .as_ref()
            .and_then(|audio| unsafe { audio.clock() })
    }

    pub(crate) unsafe fn finish_seek(&self, video_pts: Option<f64>, fallback: f64) -> Option<f64> {
        let audio_clock = unsafe { self.audio_clock() };
        completed_seek_anchor(
            video_pts,
            audio_clock,
            self.audio.is_some(),
            self.eof,
            self.video_queue.len() >= VIDEO_QUEUE_TARGET || unsafe { self.audio_full() },
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
