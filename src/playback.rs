use crate::clock::WallClock;
use crate::media::Media;
use crate::metadata::{VideoInfo, display_title, media_title};
use crate::mpris::{Mpris, MprisCommand, PlaybackStatus, seconds_to_microseconds};
use crate::presentation::timeline_text;
use crate::presentation::{
    PositionNotice, PresentationStats, TopBar, next_track, subtitle_status_text, track_status_text,
};
use crate::renderer::{Renderer, RendererOverlays};
use crate::subtitles::SubtitleCue;
use crate::window::{Action, Sdl, WaylandInput, Window, action_for_key};
use crate::worker::DecodeWorker;
use crate::{Result, ffi};
use std::collections::VecDeque;
use std::ffi::CString;
use std::path::PathBuf;
use std::ptr;
use std::time::{Duration, Instant};

pub(crate) const VIDEO_PRESENTATION_LEAD: f64 = 0.012;
pub(crate) const SEEK_SECONDS: f64 = 10.0;
fn toggle_fullscreen(window: &Window, fullscreen: &mut bool) -> Result<()> {
    let requested = !*fullscreen;
    window.set_fullscreen(requested)?;
    *fullscreen = requested;
    Ok(())
}

pub(crate) unsafe fn set_playback_paused(
    decoder: &DecodeWorker,
    clock: &mut WallClock,
    paused: &mut bool,
    requested: bool,
) -> Result<()> {
    if *paused == requested {
        return Ok(());
    }
    if requested {
        let mut media = decoder.lock()?;
        unsafe { media.sync_audio(clock.now(), true)? };
    }
    // Resumption is scheduled by the event loop after any pending seek.
    clock.set_paused(requested);
    *paused = requested;
    Ok(())
}

pub(crate) unsafe fn seek_by(
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
    clock.seek(target);
    let position = target - playback_start;
    notice.show(position, duration);
    Ok(position)
}

pub(crate) unsafe fn seek_to(
    media: &mut Media,
    clock: &mut WallClock,
    notice: &mut PositionNotice,
    position: f64,
    playback_start: f64,
    duration: f64,
) -> Result<f64> {
    let position = position.clamp(0.0, (duration - 0.05).max(0.0));
    let target = playback_start + position;
    // Timeline seeks can require decoding a long GOP. Leave that work to the
    // decode worker so the window continues dispatching compositor events.
    let target = unsafe { media.seek(target)? };
    clock.seek(target);
    let position = target - playback_start;
    notice.show(position, Some(duration));
    Ok(position)
}

pub(crate) unsafe fn run(path: PathBuf, perf_log: bool) -> Result<()> {
    let _sdl = unsafe { Sdl::init()? };
    let metadata_title = CString::new(media_title(&path))
        .map_err(|_| "media filename contains a NUL byte".to_string())?;
    let title = CString::new(display_title(&path))
        .map_err(|_| "media filename contains a NUL byte".to_string())?;
    let window = unsafe { Window::create(&title)? };
    let _wayland_input = match WaylandInput::create(&window) {
        Ok(input) => Some(input),
        Err(error) => {
            eprintln!("warning: {error}; drag-anywhere is unavailable");
            None
        }
    };
    let mut renderer = Renderer::create(&window)?;
    let mut media = unsafe { Media::open(&path, Some(renderer.device()), perf_log)? };

    if let Err(hardware_error) = unsafe { media.fill_initial_queues() } {
        if !media.video.uses_vulkan {
            return Err(hardware_error);
        }
        eprintln!(
            "warning: Vulkan decoding failed ({hardware_error}); restarting with software decoding"
        );
        drop(media);
        media = unsafe { Media::open(&path, None, perf_log)? };
        unsafe { media.fill_initial_queues()? };
    }
    if media.video_queue.is_empty() && media.eof {
        return Err("the video decoder produced no frames".into());
    }
    let mut video_info_pending = media.video_queue.is_empty();
    let mut video_info = unsafe {
        VideoInfo::inspect(
            &media,
            media
                .video_queue
                .front()
                .map_or(ptr::null_mut(), |frame| frame.as_ptr()),
        )
    };
    let audio_labels = media
        .audio_tracks
        .iter()
        .map(|track| track.label.clone())
        .collect::<Vec<_>>();
    let subtitle_labels = media
        .subtitle_tracks
        .iter()
        .map(|track| track.label.clone())
        .collect::<Vec<_>>();
    let audio_track_count = audio_labels.len();
    let subtitle_track_count = subtitle_labels.len();
    let subtitles_available = subtitle_track_count > 0;
    let mut selected_audio_track = 0;
    let mut video_details = video_info.overlay_text(
        audio_labels
            .first()
            .map(|label| (selected_audio_track, audio_track_count, label)),
    );
    window.set_minimum_size()?;
    let (mut width, mut height) = window.pixel_size()?;
    renderer.resize(width, height)?;
    if let Some(first_frame) = media.video_queue.front() {
        renderer.display(
            first_frame,
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
        )?;
    }

    let clock_origin = match (media.first_video_pts, unsafe { media.audio_clock() }) {
        (Some(video), Some(audio)) => video.min(audio),
        (video, audio) => audio.or(video).unwrap_or(0.0),
    };
    unsafe { media.sync_audio(clock_origin, false)? };
    let playback_start = clock_origin;
    let media_duration = media.duration();
    let mpris = Mpris::create(
        &metadata_title,
        &path,
        media_duration.map_or(0, seconds_to_microseconds),
    );
    let decoder = DecodeWorker::start(media, path, perf_log);
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
    let mut subtitles_visible = false;
    let mut selected_subtitle_track = 0;
    let mut redraw = true;
    let mut new_frame_pending = true;
    let mut top_bar = TopBar::new();
    let mut position_notice = PositionNotice::new();
    let mut scrubbing = false;
    let mut scrub_preview = None;
    let mut pending_scrub_target = None;
    let mut pending_seek_anchor = None;
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
                    (width, height) = window.pixel_size()?;
                    renderer.resize(width, height)?;
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
                            window.scrubber_target(event.x, event.y, media_duration)
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
                            window.scrubber_target(event.x, event.y, media_duration)
                        {
                            scrubbing = true;
                            scrub_preview = Some(target);
                            position_notice.show(target, media_duration);
                            unsafe { ffi::up_platform_capture_mouse(1) };
                        } else if window.close_button_contains(event.x, event.y) {
                            running = false;
                        } else if event.clicks >= 2 {
                            toggle_fullscreen(&window, &mut fullscreen)?;
                        }
                    }
                }
                ffi::UpEventType_UP_EVENT_MOUSE_BUTTON_UP => {
                    if event.button == ffi::UP_MOUSE_BUTTON_LEFT as u8 && scrubbing {
                        if let Some(target) =
                            window.scrubber_target(event.x, event.y, media_duration)
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
                            pending_seek_anchor = Some(playback_start + position);
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
                            pending_seek_anchor = Some(playback_start + position);
                            redraw = true;
                            new_frame_pending = true;
                        }
                        Some(Action::ToggleFullscreen) => {
                            toggle_fullscreen(&window, &mut fullscreen)?;
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
                        Some(Action::CycleAudio) if audio_track_count > 1 => {
                            let next = next_track(selected_audio_track, audio_track_count);
                            let requested_target = clock.now();
                            let mut media = decoder.lock()?;
                            decoder.clear_frames(&mut video_queue, &mut current_video);
                            decoder.clear_subtitles(
                                &mut subtitle_queue,
                                &mut subtitle_queues,
                                &mut current_subtitles,
                            );
                            let playback_target =
                                unsafe { media.select_audio_track(next, requested_target)? };
                            clock.seek(playback_target);
                            pending_seek_anchor = Some(playback_target);
                            selected_audio_track = next;
                            video_details = video_info.overlay_text(Some((
                                selected_audio_track,
                                audio_track_count,
                                &audio_labels[selected_audio_track],
                            )));
                            position_notice.show_text(track_status_text(
                                "AUDIO",
                                selected_audio_track,
                                audio_track_count,
                                &audio_labels[selected_audio_track],
                            ));
                            if let Some(mpris) = &mpris {
                                mpris.seeked(seconds_to_microseconds(
                                    playback_target - playback_start,
                                ));
                            }
                            redraw = true;
                            new_frame_pending = true;
                        }
                        Some(Action::ToggleSubtitles) if subtitles_available => {
                            subtitles_visible = !subtitles_visible;
                            position_notice.show_text(subtitle_status_text(
                                subtitles_visible,
                                selected_subtitle_track,
                                subtitle_track_count,
                                &subtitle_labels[selected_subtitle_track],
                            ));
                            redraw = true;
                        }
                        Some(Action::CycleSubtitles) if subtitles_available => {
                            selected_subtitle_track =
                                next_track(selected_subtitle_track, subtitle_track_count);
                            subtitles_visible = true;
                            position_notice.show_text(subtitle_status_text(
                                true,
                                selected_subtitle_track,
                                subtitle_track_count,
                                &subtitle_labels[selected_subtitle_track],
                            ));
                            redraw = true;
                        }
                        Some(
                            Action::CycleAudio | Action::ToggleSubtitles | Action::CycleSubtitles,
                        ) => {}
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
                        pending_seek_anchor = Some(playback_start + position);
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
            pending_seek_anchor = Some(playback_start + position);
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

        if let Some(target) = pending_seek_anchor
            && let Some(media) = decoder.try_lock()?
        {
            let video_pts = video_queue.front().map(|frame| frame.frame.pts);
            if let Some(playback_target) = unsafe { media.finish_seek(video_pts, target) } {
                clock.seek(playback_target);
                pending_seek_anchor = None;
            }
        }

        // SDL/PipeWire consumes audio in period-sized chunks (1024 samples on
        // this machine), so the continuous audio-anchored wall clock is used
        // for video presentation instead of the quantized queue counter.
        let mut playback_time = pending_seek_anchor.unwrap_or_else(|| clock.now());
        if let Some(mut media) = decoder.try_lock()? {
            unsafe { media.sync_audio(playback_time, paused || pending_seek_anchor.is_some())? };
            if !paused
                && pending_seek_anchor.is_none()
                && let Some(audio_pts) = media
                    .audio
                    .as_ref()
                    .and_then(|audio| unsafe { audio.active_clock() })
            {
                clock.synchronize(audio_pts);
                playback_time = clock.now();
            }
            if video_info_pending
                && let Some(frame) = video_queue.front().or(current_video.as_ref())
            {
                video_info = unsafe { VideoInfo::inspect(&media, frame.frame.as_ptr()) };
                video_details = video_info.overlay_text(
                    audio_labels
                        .get(selected_audio_track)
                        .map(|label| (selected_audio_track, audio_track_count, label)),
                );
                video_info_pending = false;
            }
        }
        if let Some(mpris) = &mpris {
            let status = if mpris_stopped {
                PlaybackStatus::Stopped
            } else if paused {
                PlaybackStatus::Paused
            } else {
                PlaybackStatus::Playing
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
            renderer.display(
                frame,
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
            )?;
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
            let (total_shown, total_dropped) = stats.counts();
            let shown = total_shown - report_shown;
            report_fill_time += decoder.take_fill_time();
            let fill_ms = report_fill_time.as_secs_f64() * 1000.0 / shown.max(1) as f64;
            let display_ms =
                report_display_time.as_secs_f64() * 1000.0 / report_display_calls.max(1) as f64;
            eprintln!(
                "perf: {:.1} shown/s, {:.1} dropped/s, {:.2} ms fill, {:.2} ms display",
                shown as f64 / elapsed,
                (total_dropped - report_dropped) as f64 / elapsed,
                fill_ms,
                display_ms,
            );
            last_perf_report = Instant::now();
            report_shown = total_shown;
            report_dropped = total_dropped;
            report_fill_time = Duration::ZERO;
            report_display_time = Duration::ZERO;
            report_display_calls = 0;
        }

        if let Some(media) = decoder.try_lock()?
            && media.eof
            && media.video_queue.is_empty()
            && video_queue.is_empty()
            && decoder.pending_frames() == usize::from(current_video.is_some())
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
