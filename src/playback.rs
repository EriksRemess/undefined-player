use crate::clock::WallClock;
use crate::decoder::VideoFrame;
use crate::media::{AudioTrackError, Media};
use crate::metadata::{VideoInfo, source_display_title, source_title};
use crate::mpris::{Mpris, MprisCommand, PlaybackStatus, seconds_to_microseconds};
use crate::palette::{Palette, PaletteCommand, PaletteSeek};
use crate::presentation::timeline_text;
use crate::presentation::{
    ChapterHeading, PositionNotice, PresentationStats, TopBar, next_track, subtitle_status_text,
    track_status_text,
};
use crate::renderer::{Renderer, RendererOverlays, TextOverlay, VideoFrames};
use crate::source::MediaSource;
use crate::subtitles::SubtitleCue;
use crate::window::{Action, IdleCursor, Window, WindowShape, action_for_key};
use crate::worker::DecodeWorker;
use crate::{Result, ffi};
use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::ptr;
use std::time::{Duration, Instant};

pub(crate) const VIDEO_PRESENTATION_LEAD: f64 = 0.012;
pub(crate) const SEEK_SECONDS: f64 = 10.0;

fn accepts_key_event(key: u32, repeat: bool, palette_open: bool) -> bool {
    !repeat
        || (palette_open
            && matches!(
                key,
                ffi::UpKey_UP_KEY_UP | ffi::UpKey_UP_KEY_DOWN | ffi::UpKey_UP_KEY_BACKSPACE
            ))
}

pub(crate) fn resume_background(
    prompt_pending: bool,
    video_available: bool,
) -> Result<Option<VideoFrame>> {
    if prompt_pending && !video_available {
        VideoFrame::black().map(Some)
    } else {
        Ok(None)
    }
}

fn toggle_fullscreen(window: &Window, fullscreen: &mut bool) -> Result<()> {
    let requested = !*fullscreen;
    window.set_fullscreen(requested)?;
    *fullscreen = requested;
    Ok(())
}

/// # Safety
/// The session must satisfy [`Media`]'s native resource invariants, including
/// keeping SDL and its shared hardware device alive.
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

pub(crate) fn queue_relative_seek(
    pending: &mut Option<f64>,
    current: f64,
    offset: f64,
    duration: Option<f64>,
) {
    let maximum = duration.map_or(f64::MAX, |duration| (duration - 0.05).max(0.0));
    *pending = Some((pending.unwrap_or(current) + offset).clamp(0.0, maximum));
}

pub(crate) fn handle_playback_end(looping: bool, pending_position: &mut Option<f64>) -> bool {
    if looping {
        *pending_position = Some(0.0);
        false
    } else {
        true
    }
}

/// # Safety
/// The session must satisfy [`Media`]'s native resource invariants, including
/// keeping SDL and its shared hardware device alive.
pub(crate) unsafe fn seek_to(
    media: &mut Media,
    clock: &mut WallClock,
    notice: &mut PositionNotice,
    position: f64,
    playback_start: f64,
    duration: Option<f64>,
) -> Result<f64> {
    let maximum = duration.map_or(f64::MAX, |duration| (duration - 0.05).max(0.0));
    let position = position.clamp(0.0, maximum);
    let target = playback_start + position;
    // Timeline seeks can require decoding a long GOP. Leave that work to the
    // decode worker so the window continues dispatching compositor events.
    let target = unsafe { media.seek(target)? };
    clock.seek(target);
    let position = target - playback_start;
    notice.show(position, duration);
    Ok(position)
}

// SDL remains alive across Finder requests while each playback session drops
// its decoder, audio stream, GPU resources and saved resume state normally.
/// # Safety
/// Run on the SDL main thread with SDL initialized for the entire session.
pub(crate) unsafe fn run(source: MediaSource, perf_log: bool) -> Result<Option<MediaSource>> {
    let mut config = crate::config::Config::load();
    let metadata_title = CString::new(source_title(&source))
        .map_err(|_| "media title contains a NUL byte".to_string())?;
    let title = CString::new(source_display_title(&source))
        .map_err(|_| "media title contains a NUL byte".to_string())?;
    let window = unsafe { Window::create(&title)? };
    let window_shape = match WindowShape::create(&window) {
        Ok(input) => input,
        Err(error) => {
            eprintln!(
                "warning: {error}; window dragging, the window menu, and rounded corners are unavailable"
            );
            None
        }
    };
    let mut renderer = Renderer::create(&window)?;
    renderer.set_corner_radius(config.corner_radius);
    let mut media = unsafe { Media::open(&source, renderer.device(), perf_log)? };
    unsafe { media.prefer_initial_audio_language(&config.preferred_audio_language) };
    #[cfg(target_os = "macos")]
    unsafe {
        media.try_native_playback(&source, &mut renderer)?
    };
    let native_playback = media.is_native();

    if let Err(hardware_error) = unsafe { media.fill_initial_queues() } {
        if !media.video.uses_hardware {
            return Err(hardware_error);
        }
        eprintln!(
            "warning: hardware decoding failed ({hardware_error}); restarting with software decoding"
        );
        drop(media);
        media = unsafe { Media::open(&source, None, perf_log)? };
        unsafe { media.prefer_initial_audio_language(&config.preferred_audio_language) };
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
    let mut selected_audio_track = media.selected_audio_track;
    if audio_track_count > 0 {
        eprintln!(
            "audio: track {} selected via {}",
            selected_audio_track + 1,
            if native_playback {
                "AVFoundation".into()
            } else {
                unsafe { CStr::from_ptr(ffi::up_platform_audio_driver()) }.to_string_lossy()
            }
        );
    }
    let mut video_details = video_info.overlay_text(
        audio_labels
            .get(selected_audio_track)
            .map(|label| (selected_audio_track, audio_track_count, label)),
    );
    let media_duration = media.duration();
    let mut resume_session =
        crate::resume::Session::open(&source, media_duration).unwrap_or_else(|error| {
            eprintln!("warning: resume is unavailable: {error}");
            crate::resume::Session::disabled()
        });
    let mut resume_prompt = resume_session.saved.map(crate::resume::Prompt::new);
    let initial_prompt = resume_prompt.as_ref().map(crate::resume::Prompt::text);
    // Audio may fill its queue before the first picture is decoded. The
    // paused startup prompt must still have a background it can render on.
    let resume_background =
        resume_background(resume_prompt.is_some(), !media.video_queue.is_empty())?;
    window.set_minimum_size()?;
    let (mut width, mut height) = window.pixel_size()?;
    renderer.resize(width, height)?;
    if let Some(first_frame) = media.video_queue.front().or(resume_background.as_ref()) {
        renderer.display(
            VideoFrames {
                current: first_frame,
                previous: None,
                next: media.video_queue.get(1),
            },
            width,
            height,
            1.0,
            &title,
            RendererOverlays {
                info: None,
                details: None,
                metadata: None,
                position: None,
                scrubber: None,
                chapter_markers: &[],
                subtitle: None,
                palette: initial_prompt.as_deref().unwrap_or(c""),
            },
        )?;
    }

    let clock_origin = match (media.first_video_pts, unsafe { media.audio_clock() }) {
        (Some(video), Some(audio)) => video.min(audio),
        (video, audio) => audio.or(video).unwrap_or(0.0),
    };
    unsafe { media.sync_audio(clock_origin, resume_prompt.is_some())? };
    let playback_start = clock_origin;
    let mut chapters =
        unsafe { crate::chapters::Chapters::inspect(media.format, playback_start, media_duration) };
    let chapter_markers = chapters.markers(media_duration);
    let mut media_metadata = unsafe { crate::metadata::MediaMetadata::inspect(media.format) };
    media_metadata.apply_source(&source);
    let artwork = unsafe { crate::artwork::Artwork::extract(media.format) };
    let mpris_title = media_metadata
        .title()
        .map(|title| CString::new(title).expect("title has no NUL bytes"));
    let mpris = Mpris::create(
        mpris_title.as_deref().unwrap_or(&metadata_title),
        media_metadata.artist(),
        &source,
        media_duration.map_or(0, seconds_to_microseconds),
        artwork.as_ref().map(|artwork| artwork.path.as_path()),
    );
    if let Some(mpris) = &mpris {
        mpris.navigation(
            chapters.target(0.0, false).is_some(),
            chapters.target(0.0, true).is_some(),
        );
    }
    let audio_control = media.audio_control();
    let decoder = DecodeWorker::start(media, source, perf_log);
    let mut clock = WallClock::new(clock_origin);
    clock.set_paused(resume_prompt.is_some());
    let mut current_video = None;
    let mut previous_video = None;
    let mut video_queue = VecDeque::new();
    let mut subtitle_queue = VecDeque::new();
    let mut subtitle_queues = (0..subtitle_track_count)
        .map(|_| VecDeque::new())
        .collect::<Vec<VecDeque<SubtitleCue>>>();
    let mut current_subtitles = (0..subtitle_track_count)
        .map(|_| None)
        .collect::<Vec<Option<SubtitleCue>>>();
    let mut running = true;
    let mut paused = resume_prompt.is_some();
    let mut muted = false;
    let mut looping = false;
    let mut mpris_stopped = false;
    let mut fullscreen = false;
    let mut info_visible = false;
    let mut palette = Palette::new();
    let mut subtitles_visible = false;
    let mut selected_subtitle_track = 0;
    let mut redraw = true;
    let mut new_frame_pending = true;
    let mut top_bar = TopBar::new();
    let mut mouse_position = None;
    let mut cursor = IdleCursor::new();
    let mut chapter_heading = ChapterHeading::default();
    let mut position_notice = PositionNotice::new();
    let mut scrubbing = false;
    let mut scrub_preview = None;
    let mut pending_position = None;
    let mut pending_seek_anchor = None;
    let mut stats = PresentationStats::new();
    let mut last_info_refresh = Instant::now();
    let mut last_perf_report = Instant::now();
    let mut report_shown = 0;
    let mut report_dropped = 0;
    let mut report_fill_time = Duration::ZERO;
    let mut report_display_time = Duration::ZERO;
    let mut report_display_calls = 0_u64;
    let mut last_video_presented: Option<Instant> = None;
    let mut report_max_frame_gap = Duration::ZERO;
    let mut last_native_overlay = Instant::now();
    while running {
        #[cfg(target_os = "macos")]
        if let Some(source) = crate::macos_app::take_source()? {
            return Ok(Some(source));
        }
        let mut presented_this_iteration = false;
        let mut event: ffi::UpEvent = unsafe { std::mem::zeroed() };
        while unsafe { ffi::up_platform_poll_event(&mut event) } != 0 {
            match event.type_ {
                ffi::UpEventType_UP_EVENT_QUIT | ffi::UpEventType_UP_EVENT_WINDOW_CLOSE => {
                    running = false
                }
                ffi::UpEventType_UP_EVENT_WINDOW_RESIZED => {
                    (width, height) = window.pixel_size()?;
                    renderer.resize(width, height)?;
                    chapter_heading.hovered = None;
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_WINDOW_EXPOSED => redraw = true,
                ffi::UpEventType_UP_EVENT_WINDOW_FOCUS_GAINED => {
                    cursor.activity();
                    top_bar.set_focused(true);
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_WINDOW_FOCUS_LOST => {
                    mouse_position = None;
                    cursor.activity();
                    top_bar.set_focused(false);
                    chapter_heading.hovered = None;
                    if scrubbing {
                        pending_position = scrub_preview.take();
                    }
                    scrubbing = false;
                    unsafe { ffi::up_platform_capture_mouse(0) };
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_MOUSE_LEAVE => {
                    mouse_position = None;
                    cursor.activity();
                    chapter_heading.hovered = None;
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_MOUSE_MOTION => {
                    mouse_position = Some((event.x, event.y));
                    cursor.activity();
                    top_bar.mouse_activity();
                    chapter_heading.hovered =
                        window.chapter_hover(event.x, event.y, &chapter_markers);
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
                    mouse_position = Some((event.x, event.y));
                    cursor.activity();
                    top_bar.mouse_activity();
                    redraw = true;
                    if resume_prompt.is_some() {
                        if event.button == ffi::UP_MOUSE_BUTTON_LEFT as u8
                            && window.close_button_contains(event.x, event.y)
                        {
                            running = false;
                        }
                        continue;
                    }
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
                    cursor.activity();
                    if event.button == ffi::UP_MOUSE_BUTTON_LEFT as u8 && scrubbing {
                        if let Some(target) =
                            window.scrubber_target(event.x, event.y, media_duration)
                        {
                            pending_position = Some(target);
                            position_notice.show(target, media_duration);
                        } else {
                            pending_position = scrub_preview;
                        }
                        scrubbing = false;
                        scrub_preview = None;
                        unsafe { ffi::up_platform_capture_mouse(0) };
                        redraw = true;
                    }
                }
                ffi::UpEventType_UP_EVENT_KEY_DOWN => {
                    if !accepts_key_event(event.key, event.repeat != 0, palette.open) {
                        continue;
                    }
                    if let Some(prompt) = &mut resume_prompt {
                        if action_for_key(event.key) == Some(Action::Quit)
                            || event.key == ffi::UpKey_UP_KEY_ESCAPE
                        {
                            running = false;
                        } else if let Some(resume) = prompt.key(event.key) {
                            resume_session.choose(resume);
                            if resume {
                                pending_position = Some(prompt.position);
                            }
                            resume_prompt = None;
                            unsafe {
                                set_playback_paused(&decoder, &mut clock, &mut paused, false)?;
                            }
                        }
                        redraw = true;
                        continue;
                    }
                    let mut requested_audio_track = None;
                    let (action, seek_command) = if palette.open {
                        let selection = match event.key {
                            ffi::UpKey_UP_KEY_ESCAPE => {
                                palette.open = false;
                                unsafe { ffi::up_platform_start_text_input(0) };
                                None
                            }
                            ffi::UpKey_UP_KEY_RETURN => {
                                let selection =
                                    palette.activate(audio_track_count > 1, subtitles_available);
                                if selection.is_some() {
                                    palette.open = false;
                                    unsafe { ffi::up_platform_start_text_input(0) };
                                }
                                selection
                            }
                            ffi::UpKey_UP_KEY_UP => {
                                palette.selected = palette.selected.saturating_sub(1);
                                None
                            }
                            ffi::UpKey_UP_KEY_DOWN => {
                                let last = palette
                                    .rows(audio_track_count > 1, subtitles_available)
                                    .len()
                                    .saturating_sub(1);
                                palette.selected = palette.selected.saturating_add(1).min(last);
                                None
                            }
                            ffi::UpKey_UP_KEY_BACKSPACE => {
                                palette.query.pop();
                                palette.selected = 0;
                                None
                            }
                            _ => None,
                        };
                        redraw = true;
                        match selection {
                            Some(PaletteCommand::AudioLanguage(language)) => {
                                if let Err(error) = config.set_preferred_audio_language(&language) {
                                    eprintln!("warning: {error}");
                                    position_notice
                                        .show_text(c"COULD NOT SAVE SETTINGS".to_owned());
                                    continue;
                                }
                                requested_audio_track = if language == "auto" {
                                    (!audio_labels.is_empty()).then_some(0)
                                } else {
                                    crate::language::preferred_track(
                                        audio_labels.iter().map(|label| label.language.as_deref()),
                                        &language,
                                    )
                                };
                                position_notice.show_text(
                                    CString::new(format!(
                                        "PREFERRED AUDIO: {language}{}",
                                        if requested_audio_track.is_none() {
                                            " (NO MATCH IN THIS VIDEO)"
                                        } else {
                                            ""
                                        },
                                    ))
                                    .expect("generated status text contains no NUL bytes"),
                                );
                                if requested_audio_track.is_none()
                                    || requested_audio_track == Some(selected_audio_track)
                                {
                                    continue;
                                }
                                (Some(Action::CycleAudio), None)
                            }
                            Some(PaletteCommand::CornerRadius(radius)) => {
                                match config.set_corner_radius(radius) {
                                    Ok(()) => {
                                        renderer.set_corner_radius(radius);
                                        position_notice.show_text(
                                            CString::new(format!("CORNER RADIUS: {radius} PX"))
                                                .expect(
                                                    "generated status text contains no NUL bytes",
                                                ),
                                        );
                                    }
                                    Err(error) => {
                                        eprintln!("warning: {error}");
                                        position_notice
                                            .show_text(c"COULD NOT SAVE SETTINGS".to_owned());
                                    }
                                }
                                continue;
                            }
                            Some(PaletteCommand::Action(action)) => (Some(action), None),
                            Some(PaletteCommand::Seek(argument)) => {
                                (Some(Action::SeekForward), Some(argument))
                            }
                            Some(PaletteCommand::Input(_)) | None => continue,
                        }
                    } else {
                        if event.key == ffi::UpKey_UP_KEY_P && event.modifiers & 3 == 3 {
                            palette.open = true;
                            palette.query.clear();
                            palette.selected = 0;
                            unsafe { ffi::up_platform_start_text_input(1) };
                            redraw = true;
                            continue;
                        }
                        (action_for_key(event.key), None)
                    };
                    match action {
                        #[cfg(target_os = "macos")]
                        Some(Action::ToggleUpscale) => {
                            let enabled = renderer.toggle_upscaling();
                            position_notice.show_text(
                                if enabled {
                                    c"UPSCALE: AUTO"
                                } else {
                                    c"UPSCALE: OFF"
                                }
                                .to_owned(),
                            );
                            redraw = true;
                        }
                        Some(Action::ToggleZoom) => {
                            let enabled = renderer.toggle_zoom();
                            let status = if enabled { "ZOOM: FILL" } else { "ZOOM: FIT" };
                            eprintln!("{}", status.to_ascii_lowercase());
                            position_notice.show_text(
                                CString::new(status)
                                    .expect("generated status text contains no NUL bytes"),
                            );
                            redraw = true;
                        }
                        Some(Action::CycleDeinterlace) => {
                            let status = renderer.cycle_deinterlace();
                            eprintln!("{}", status.to_ascii_lowercase());
                            position_notice.show_text(
                                CString::new(status)
                                    .expect("generated status text contains no NUL bytes"),
                            );
                            redraw = true;
                        }
                        Some(Action::ToggleCrop) => {
                            let enabled = renderer.toggle_autocrop();
                            position_notice.show_text(
                                CString::new(if enabled {
                                    "AUTOCROP: ON"
                                } else {
                                    "AUTOCROP: OFF"
                                })
                                .expect("generated status text contains no NUL bytes"),
                            );
                            redraw = true;
                        }
                        Some(Action::Quit) => running = false,
                        Some(action @ (Action::PreviousChapter | Action::NextChapter)) => {
                            let position = pending_position.unwrap_or_else(|| {
                                chapters.playback_position(
                                    pending_seek_anchor.unwrap_or_else(|| clock.now())
                                        - playback_start,
                                )
                            });
                            if let Some(target) =
                                chapters.target(position, action == Action::NextChapter)
                            {
                                pending_position = Some(target);
                                if let Some(index) = chapters.current(target) {
                                    chapter_heading.show(index, Instant::now());
                                }
                                mpris_stopped = false;
                                top_bar.mouse_activity();
                            }
                        }
                        Some(action @ (Action::SeekBackward | Action::SeekForward)) => {
                            let current = chapters.playback_position(
                                pending_seek_anchor.unwrap_or_else(|| clock.now()) - playback_start,
                            );
                            match seek_command {
                                Some(PaletteSeek::Relative(offset)) => queue_relative_seek(
                                    &mut pending_position,
                                    current,
                                    offset,
                                    media_duration,
                                ),
                                Some(PaletteSeek::Absolute(position)) => {
                                    let maximum = media_duration
                                        .map_or(f64::MAX, |duration| (duration - 0.05).max(0.0));
                                    pending_position = Some(position.clamp(0.0, maximum));
                                }
                                None => queue_relative_seek(
                                    &mut pending_position,
                                    current,
                                    if action == Action::SeekForward {
                                        SEEK_SECONDS
                                    } else {
                                        -SEEK_SECONDS
                                    },
                                    media_duration,
                                ),
                            }
                            mpris_stopped = false;
                        }
                        Some(Action::ToggleFullscreen) => {
                            toggle_fullscreen(&window, &mut fullscreen)?;
                            redraw = true;
                        }
                        Some(Action::ToggleInfo) => {
                            info_visible = !info_visible;
                            redraw = true;
                        }
                        Some(Action::ToggleMute) => {
                            let requested = !muted;
                            audio_control.set_muted(requested)?;
                            if native_playback {
                                decoder.lock()?.native_mute(requested);
                            }
                            muted = requested;
                            if let Some(mpris) = &mpris {
                                mpris.options(muted, looping);
                            }
                            let status = if muted { "AUDIO: MUTED" } else { "AUDIO: ON" };
                            eprintln!("{}", status.to_ascii_lowercase());
                            position_notice.show_text(
                                CString::new(status)
                                    .expect("generated status text contains no NUL bytes"),
                            );
                            redraw = true;
                        }
                        Some(Action::ToggleLoop) => {
                            looping = !looping;
                            if let Some(mpris) = &mpris {
                                mpris.options(muted, looping);
                            }
                            let status = if looping { "LOOP: ON" } else { "LOOP: OFF" };
                            eprintln!("{}", status.to_ascii_lowercase());
                            position_notice.show_text(
                                CString::new(status)
                                    .expect("generated status text contains no NUL bytes"),
                            );
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
                            let next = requested_audio_track.unwrap_or_else(|| {
                                next_track(selected_audio_track, audio_track_count)
                            });
                            let requested_target = clock.now();
                            let mut media = decoder.lock()?;
                            let playback_target =
                                match unsafe { media.select_audio_track(next, requested_target) } {
                                    Ok(target) => target,
                                    Err(AudioTrackError::Unavailable(error)) => {
                                        eprintln!("warning: could not switch audio track: {error}");
                                        position_notice.show_text(
                                            if requested_audio_track.is_some() {
                                                c"PREFERENCE SAVED; AUDIO SWITCH UNAVAILABLE"
                                            } else {
                                                c"AUDIO SWITCH UNAVAILABLE"
                                            }
                                            .to_owned(),
                                        );
                                        redraw = true;
                                        continue;
                                    }
                                    Err(AudioTrackError::Playback(error)) => return Err(error),
                                };
                            // Clear presentation queues only after a successful
                            // switch, while the decoder is still locked.
                            renderer.reset_autocrop();
                            previous_video = None;
                            decoder.clear_frames(&mut video_queue, &mut current_video);
                            decoder.clear_subtitles(
                                &mut subtitle_queue,
                                &mut subtitle_queues,
                                &mut current_subtitles,
                            );
                            clock.seek(playback_target);
                            pending_seek_anchor = Some(playback_target);
                            chapters.seeked(playback_target - playback_start);
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
                ffi::UpEventType_UP_EVENT_TEXT_INPUT if palette.open => {
                    let text =
                        unsafe { std::ffi::CStr::from_ptr(event.text.as_ptr()) }.to_string_lossy();
                    palette.query.push_str(&text);
                    palette.selected = 0;
                    redraw = true;
                }
                _ => {}
            }
        }

        if let Some(mpris) = &mpris {
            mpris.dispatch();
            while let Some(command) = mpris.take_command() {
                if resume_prompt.is_some() && command != MprisCommand::Quit {
                    continue;
                }
                match command {
                    MprisCommand::Quit => running = false,
                    command @ (MprisCommand::NextChapter | MprisCommand::PreviousChapter) => {
                        let position = pending_position.unwrap_or_else(|| {
                            chapters.playback_position(
                                pending_seek_anchor.unwrap_or_else(|| clock.now()) - playback_start,
                            )
                        });
                        if let Some(target) =
                            chapters.target(position, command == MprisCommand::NextChapter)
                        {
                            pending_position = Some(target);
                            if let Some(index) = chapters.current(target) {
                                chapter_heading.show(index, Instant::now());
                            }
                            mpris_stopped = false;
                            top_bar.mouse_activity();
                        }
                    }
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
                            pending_position = Some(0.0);
                        }
                    }
                    MprisCommand::Seek(offset_us) => {
                        let current = chapters.playback_position(
                            pending_seek_anchor.unwrap_or_else(|| clock.now()) - playback_start,
                        );
                        queue_relative_seek(
                            &mut pending_position,
                            current,
                            offset_us as f64 / 1_000_000.0,
                            media_duration,
                        );
                        mpris_stopped = false;
                    }
                    MprisCommand::SetPosition(position_us) => {
                        if media_duration.is_some() {
                            pending_position = Some(position_us.max(0) as f64 / 1_000_000.0);
                        }
                    }
                }
            }
        }
        cursor.update(&window);
        redraw |= renderer.update_window_shape(window_shape.as_ref())?;
        redraw |= renderer.set_close_hovered(
            mouse_position.is_some_and(|(x, y)| window.close_button_contains(x, y)),
        );
        if !running {
            break;
        }
        if let Some(position) = pending_position.take() {
            let mut media = decoder.lock()?;
            renderer.reset_autocrop();
            previous_video = None;
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
                    media_duration,
                )?
            };
            if let Some(mpris) = &mpris {
                mpris.seeked(seconds_to_microseconds(position));
            }
            pending_seek_anchor = Some(playback_start + position);
            chapters.seeked(position);
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
            if let Some(playback_target) =
                unsafe { media.finish_seek(video_pts, decoder.pending_frames(), target) }
            {
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
                && let Some(audio_pts) = unsafe { media.active_clock() }
            {
                if native_playback {
                    clock.seek(audio_pts);
                } else {
                    clock.synchronize(audio_pts);
                }
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
        if pending_seek_anchor.is_none() {
            resume_session.observe(playback_time - playback_start);
        }
        if let Some(mpris) = &mpris {
            let status = if mpris_stopped {
                PlaybackStatus::Stopped
            } else if paused {
                PlaybackStatus::Paused
            } else {
                PlaybackStatus::Playing
            };
            let chapter_position = chapters.playback_position(playback_time - playback_start);
            mpris.navigation(
                chapters.target(chapter_position, false).is_some(),
                chapters.target(chapter_position, true).is_some(),
            );
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

        if paused {
            last_video_presented = None;
        }
        if !paused {
            let mut due_frames = 0;
            if video_queue
                .front()
                .is_some_and(|frame| frame.frame.pts <= playback_time)
            {
                previous_video = std::mem::replace(&mut current_video, video_queue.pop_front());
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
                        previous_video =
                            std::mem::replace(&mut current_video, video_queue.pop_front());
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
                previous_video = std::mem::replace(&mut current_video, video_queue.pop_front());
                due_frames = 1;
                redraw = true;
            }
            if due_frames > 0 {
                stats.drop_frames(due_frames - 1);
                new_frame_pending = true;
            }
        }
        if current_video.is_none() && !video_queue.is_empty() {
            previous_video = std::mem::replace(&mut current_video, video_queue.pop_front());
            redraw = true;
            new_frame_pending = true;
        }

        if native_playback
            && top_bar.alpha > 0.001
            && last_native_overlay.elapsed() >= Duration::from_millis(33)
        {
            redraw = true;
            last_native_overlay = Instant::now();
        }
        let heading_expired = chapter_heading.update(Instant::now());
        if chapter_heading.index().is_some() || heading_expired {
            top_bar.mouse_activity();
        }
        redraw |= heading_expired;
        redraw |= top_bar.update();
        redraw |= position_notice.update();
        if info_visible && last_info_refresh.elapsed() >= Duration::from_millis(100) {
            last_info_refresh = Instant::now();
            redraw = true;
        }

        if let Some(frame) = current_video.as_ref().map(|queued| &queued.frame) {
            redraw |= renderer.update_autocrop(frame, paused);
        }
        if redraw
            && let Some(frame) = current_video
                .as_ref()
                .map(|queued| &queued.frame)
                // Redraw on black after the answer too, so the old prompt
                // clears while audio plays ahead of the first video frame.
                .or(resume_background.as_ref())
        {
            let stats_info = info_visible.then(|| {
                if native_playback {
                    c"PLAYBACK: AVFOUNDATION".to_owned()
                } else {
                    stats.text(video_info.frame_rate)
                }
            });
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
                crate::overlay::Scrubber {
                    progress: progress as f32,
                    alpha: top_bar.alpha,
                }
            });
            let current_metadata = info_visible.then(|| {
                media_metadata.overlay_text(
                    chapters
                        .current(chapters.playback_position(playback_time - playback_start))
                        .and_then(|index| chapters.label(index)),
                )
            });
            let heading = chapter_heading
                .index()
                .and_then(|index| chapters.label(index))
                .unwrap_or(&title);
            let palette_text = palette.display(audio_track_count > 1, subtitles_available);
            let prompt_text = resume_prompt.as_ref().map(crate::resume::Prompt::text);
            let display_started = Instant::now();
            renderer.display(
                VideoFrames {
                    current: frame,
                    previous: previous_video.as_ref().map(|queued| &queued.frame),
                    next: video_queue.front().map(|queued| &queued.frame),
                },
                width,
                height,
                top_bar.alpha,
                heading,
                RendererOverlays {
                    info: stats_info
                        .as_deref()
                        .map(|text| TextOverlay { text, alpha: 1.0 }),
                    details: (info_visible && !palette.open).then_some(video_details.as_c_str()),
                    metadata: (!palette.open)
                        .then_some(current_metadata.as_deref())
                        .flatten(),
                    position: position.map(|text| TextOverlay {
                        text,
                        alpha: position_alpha,
                    }),
                    scrubber,
                    chapter_markers: &chapter_markers,
                    subtitle: subtitles_visible
                        .then(|| current_subtitles[selected_subtitle_track].as_ref())
                        .flatten(),
                    palette: if let Some(text) = prompt_text.as_deref() {
                        text
                    } else if palette.open {
                        palette_text.as_c_str()
                    } else {
                        c""
                    },
                },
            )?;
            if perf_log {
                report_display_time += display_started.elapsed();
                report_display_calls += 1;
            }
            if new_frame_pending {
                if perf_log && !paused {
                    let now = Instant::now();
                    if let Some(previous) = last_video_presented.replace(now) {
                        report_max_frame_gap = report_max_frame_gap.max(now - previous);
                    }
                }
                stats.presented();
                new_frame_pending = false;
            }
            redraw = false;
            presented_this_iteration = true;
        }

        if perf_log && native_playback && last_perf_report.elapsed() >= Duration::from_secs(2) {
            eprintln!("perf: native playback at {:.3} s", playback_time);
            last_perf_report = Instant::now();
        }
        if perf_log && !native_playback && last_perf_report.elapsed() >= Duration::from_secs(2) {
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
            let gap = last_video_presented.map_or(report_max_frame_gap, |last| {
                report_max_frame_gap.max(last.elapsed())
            });
            let video_lag = current_video
                .as_ref()
                .map_or(0.0, |frame| (playback_time - frame.frame.pts).max(0.0));
            eprintln!(
                "perf: {:.0} ms longest frame gap, {:.0} ms video behind",
                gap.as_secs_f64() * 1000.0,
                video_lag * 1000.0
            );
            if let Some(media) = decoder.try_lock()?
                && let Some(audio) = &media.audio
            {
                eprintln!(
                    "perf: {:.0} ms audio queued, {} audio underruns",
                    unsafe { audio.queued_bytes() } as f64 * 1000.0
                        / (crate::audio::AUDIO_RATE as f64
                            * crate::audio::AUDIO_BYTES_PER_FRAME as f64),
                    audio.underruns
                );
            }
            last_perf_report = Instant::now();
            report_shown = total_shown;
            report_dropped = total_dropped;
            report_fill_time = Duration::ZERO;
            report_display_time = Duration::ZERO;
            report_display_calls = 0;
            report_max_frame_gap = Duration::ZERO;
        }

        let playback_finished = if let Some(media) = decoder.try_lock()? {
            if native_playback {
                // AVPlayer owns the final frame. The geometry placeholder may
                // be timestamped within 100 ms of EOF after a seek.
                media.eof
            } else {
                media.eof
                    && media.video_queue.is_empty()
                    && video_queue.is_empty()
                    && decoder.pending_frames()
                        == usize::from(current_video.is_some())
                            + usize::from(previous_video.is_some())
                    && unsafe { media.audio_empty() }
                    && current_video.as_ref().is_none_or(|frame| {
                        playback_time >= frame.frame.pts + frame.frame.duration.max(0.1)
                    })
            }
        } else {
            false
        };
        if playback_finished && resume_prompt.is_none() {
            resume_session.complete();
            if handle_playback_end(looping, &mut pending_position) {
                break;
            }
            mpris_stopped = false;
        }

        // Metal presentation already waits for vsync. Another fixed sleep
        // consumes the next frame's scheduling margin at 60 Hz.
        if !cfg!(target_os = "macos") || !presented_this_iteration {
            unsafe { ffi::up_platform_delay(2) };
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_only_edit_or_navigate_an_open_palette() {
        for key in [
            ffi::UpKey_UP_KEY_UP,
            ffi::UpKey_UP_KEY_DOWN,
            ffi::UpKey_UP_KEY_BACKSPACE,
        ] {
            assert!(accepts_key_event(key, true, true));
            assert!(!accepts_key_event(key, true, false));
        }
        for key in [
            ffi::UpKey_UP_KEY_RETURN,
            ffi::UpKey_UP_KEY_SPACE,
            ffi::UpKey_UP_KEY_P,
            ffi::UpKey_UP_KEY_Y,
            ffi::UpKey_UP_KEY_N,
        ] {
            assert!(!accepts_key_event(key, true, true));
            assert!(!accepts_key_event(key, true, false));
            assert!(accepts_key_event(key, false, true));
            assert!(accepts_key_event(key, false, false));
        }
    }

    #[test]
    fn startup_background_is_available_without_decoded_video() {
        let background = resume_background(true, false).unwrap().unwrap();
        assert_eq!(background.dimensions(), (16, 16));
        assert_eq!(
            unsafe { ffi::up_av_frame_is_hardware(background.as_ptr()) },
            0
        );
        assert!(resume_background(true, true).unwrap().is_none());
        assert!(resume_background(false, false).unwrap().is_none());
    }
}
