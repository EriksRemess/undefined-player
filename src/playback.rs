use crate::clock::WallClock;
use crate::decoder::VideoFrame;
use crate::media::Media;
use crate::metadata::{VideoInfo, source_display_title, source_title};
use crate::mpris::{Mpris, MprisCommand, PlaybackStatus, seconds_to_microseconds};
use crate::presentation::timeline_text;
use crate::presentation::{
    ChapterHeading, PositionNotice, PresentationStats, TopBar, next_track, subtitle_status_text,
    track_status_text,
};
use crate::renderer::{Renderer, RendererOverlays, VideoFrames};
use crate::source::MediaSource;
use crate::subtitles::SubtitleCue;
use crate::window::{Action, Sdl, WaylandInput, Window, action_for_key, action_shortcut};
use crate::worker::DecodeWorker;
use crate::{Result, ffi};
use std::collections::VecDeque;
use std::ffi::CString;
use std::ptr;
use std::time::{Duration, Instant};

struct Palette {
    open: bool,
    query: String,
    selected: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PaletteSeek {
    Relative(f64),
    Absolute(f64),
}

#[derive(Clone, Debug, PartialEq)]
enum PaletteCommand {
    Action(Action),
    Seek(PaletteSeek),
    Input(&'static str),
}

struct PaletteRow {
    command: PaletteCommand,
    label: String,
    shortcut: &'static str,
}

impl PaletteRow {
    fn input(prefix: &'static str, label: &str) -> Self {
        Self {
            command: PaletteCommand::Input(prefix),
            label: label.into(),
            shortcut: "ENTER",
        }
    }

    fn seek(argument: PaletteSeek, label: String) -> Self {
        Self {
            command: PaletteCommand::Seek(argument),
            label,
            shortcut: "ENTER",
        }
    }
}

impl Palette {
    fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected: 0,
        }
    }

    fn seek_argument(&self) -> Option<(Action, f64, String)> {
        let mut parts = self.query.split_whitespace();
        let first = parts.next()?.to_ascii_lowercase();
        let direction = if first == "seek" {
            parts.next()?.to_ascii_lowercase()
        } else {
            first
        };
        let forward = match direction.as_str() {
            "forward" | "forwards" | "ahead" => true,
            "back" | "backward" | "backwards" => false,
            _ => return None,
        };
        let amount = parts.next()?.parse::<f64>().ok()?;
        if !amount.is_finite() || amount <= 0.0 {
            return None;
        }
        let unit = parts.next().unwrap_or("seconds").to_ascii_lowercase();
        if parts.next().is_some() {
            return None;
        }
        let multiplier = match unit.as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            _ => return None,
        };
        let seconds = amount * multiplier;
        if !seconds.is_finite() {
            return None;
        }
        let label = format!(
            "Seek {} {amount} {}",
            if forward { "forward" } else { "backward" },
            if multiplier == 60.0 {
                "minutes"
            } else {
                "seconds"
            }
        );
        Some((
            if forward {
                Action::SeekForward
            } else {
                Action::SeekBackward
            },
            seconds,
            label,
        ))
    }

    fn jump_argument(&self) -> Option<(f64, String)> {
        let mut parts = self.query.split_whitespace();
        if !parts.next()?.eq_ignore_ascii_case("jump") {
            return None;
        }
        let token = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        let fields = token.split(':').collect::<Vec<_>>();
        let seconds = match fields.as_slice() {
            [value] => value.parse::<f64>().ok()?,
            [minutes, seconds] => {
                let minutes = minutes.parse::<u64>().ok()?;
                let seconds = seconds.parse::<f64>().ok()?;
                if !(0.0..60.0).contains(&seconds) {
                    return None;
                }
                minutes as f64 * 60.0 + seconds
            }
            [hours, minutes, seconds] => {
                let hours = hours.parse::<u64>().ok()?;
                let minutes = minutes.parse::<u32>().ok()?;
                let seconds = seconds.parse::<f64>().ok()?;
                if minutes >= 60 || !(0.0..60.0).contains(&seconds) {
                    return None;
                }
                hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds
            }
            _ => return None,
        };
        if !seconds.is_finite() || seconds < 0.0 {
            return None;
        }
        Some((seconds, format!("Jump {token}")))
    }

    fn all_rows(audio: bool, subtitles: bool) -> Vec<PaletteRow> {
        let mut rows = vec![
            (Action::TogglePause, "Play / Pause"),
            (Action::PreviousChapter, "Previous chapter"),
            (Action::NextChapter, "Next chapter"),
            (Action::ToggleFullscreen, "Toggle fullscreen"),
            (Action::ToggleInfo, "Toggle media information"),
            (Action::ToggleZoom, "Toggle fit / fill"),
            (Action::ToggleCrop, "Toggle autocrop"),
            (Action::CycleDeinterlace, "Cycle deinterlacing"),
            (Action::ToggleLoop, "Toggle looping"),
            (Action::ToggleMute, "Toggle mute"),
        ]
        .into_iter()
        .map(|(action, label)| PaletteRow {
            shortcut: action_shortcut(&action),
            command: PaletteCommand::Action(action),
            label: label.into(),
        })
        .collect::<Vec<_>>();
        rows.insert(1, PaletteRow::input("seek ", "Seek..."));
        rows.insert(2, PaletteRow::input("jump ", "Jump to time..."));
        if audio {
            rows.push(PaletteRow {
                command: PaletteCommand::Action(Action::CycleAudio),
                label: "Next audio track".into(),
                shortcut: "A",
            });
        }
        if subtitles {
            rows.push(PaletteRow {
                command: PaletteCommand::Action(Action::ToggleSubtitles),
                label: "Toggle subtitles".into(),
                shortcut: "S",
            });
            rows.push(PaletteRow {
                command: PaletteCommand::Action(Action::CycleSubtitles),
                label: "Next subtitle track".into(),
                shortcut: "J",
            });
        }
        rows.push(PaletteRow {
            command: PaletteCommand::Action(Action::Quit),
            label: "Quit player".into(),
            shortcut: "Q",
        });
        rows
    }

    fn query_words(&self) -> String {
        self.query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    fn direction_step(&self) -> Option<bool> {
        let query = self.query_words();
        match query.strip_prefix("seek ").unwrap_or(&query) {
            "forward" | "forwards" | "ahead" => Some(true),
            "back" | "backward" | "backwards" => Some(false),
            _ => None,
        }
    }

    fn rows(&self, audio: bool, subtitles: bool) -> Vec<PaletteRow> {
        // Parsed commands are matches without applying the label search again.
        if let Some((action, amount, label)) = self.seek_argument() {
            let offset = if action == Action::SeekForward {
                amount
            } else {
                -amount
            };
            let amount_label = label.splitn(3, ' ').nth(2).unwrap().to_owned();
            return vec![PaletteRow::seek(
                PaletteSeek::Relative(offset),
                amount_label,
            )];
        }
        if let Some((position, label)) = self.jump_argument() {
            let time = label.strip_prefix("Jump ").unwrap();
            let label = if time.contains(':') {
                time.into()
            } else {
                format!("{time} seconds")
            };
            return vec![PaletteRow::seek(PaletteSeek::Absolute(position), label)];
        }
        if let Some(forward) = self.direction_step() {
            return [10, 30, 60, 120]
                .into_iter()
                .map(|seconds| {
                    PaletteRow::seek(
                        PaletteSeek::Relative(if forward {
                            seconds as f64
                        } else {
                            -(seconds as f64)
                        }),
                        format!("{seconds} seconds"),
                    )
                })
                .collect();
        }
        let query = self.query_words();
        if query == "jump" {
            return Vec::new();
        }
        if query.starts_with("seek") {
            return [("seek forward ", "Forward"), ("seek backward ", "Backward")]
                .into_iter()
                .filter(|(prefix, _)| prefix.trim().contains(&query))
                .map(|(prefix, label)| PaletteRow::input(prefix, label))
                .collect();
        }
        Self::all_rows(audio, subtitles)
            .into_iter()
            .filter(|row| row.label.to_lowercase().contains(&query))
            .collect()
    }

    fn selected_action(&self, audio: bool, subtitles: bool) -> Option<PaletteCommand> {
        let rows = self.rows(audio, subtitles);
        rows.get(self.selected.min(rows.len().saturating_sub(1)))
            .map(|row| row.command.clone())
    }

    fn activate(&mut self, audio: bool, subtitles: bool) -> Option<PaletteCommand> {
        match self.selected_action(audio, subtitles)? {
            PaletteCommand::Input(prefix) => {
                self.query = prefix.into();
                self.selected = 0;
                None
            }
            command => Some(command),
        }
    }

    fn display(&mut self, audio: bool, subtitles: bool) -> CString {
        let rows = self.rows(audio, subtitles);
        let stable_width = Self::all_rows(audio, subtitles)
            .iter()
            .map(|row| row.label.chars().count() + row.shortcut.len() + 6)
            .max()
            .unwrap_or(0);
        let query = self.query_words();
        let command = query.split_whitespace().next().unwrap_or("");
        let hint = if command == "jump" && self.jump_argument().is_none() {
            Some("ENTER TIME: jump 123 OR jump 2:03")
        } else if self.direction_step().is_some() {
            Some("CHOOSE SECONDS OR TYPE AN AMOUNT")
        } else if command == "seek" && self.seek_argument().is_none() {
            Some("CHOOSE FORWARD OR BACKWARD")
        } else {
            None
        };
        let mut text = format!("COMMAND PALETTE  > {}\n", self.query);
        if let Some(hint) = hint {
            text.push_str(hint);
            text.push('\n');
        }
        if rows.is_empty() {
            if hint.is_none() {
                text.push_str("NO MATCHES");
            }
            return CString::new(text).unwrap();
        }
        self.selected = self.selected.min(rows.len() - 1);
        let start = self
            .selected
            .saturating_sub(2)
            .min(rows.len().saturating_sub(5));
        for (index, row) in rows.iter().enumerate().skip(start).take(5) {
            text.push_str(if index == self.selected { "> " } else { "  " });
            text.push_str(&row.label);
            text.push_str("  [");
            text.push_str(row.shortcut);
            text.push(']');
            let row_width = row.label.chars().count() + row.shortcut.len() + 6;
            text.push_str(&" ".repeat(stable_width.saturating_sub(row_width)));
            text.push('\n');
        }
        CString::new(text).unwrap()
    }
}

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

pub(crate) unsafe fn run(source: MediaSource, perf_log: bool) -> Result<()> {
    let _sdl = unsafe { Sdl::init()? };
    let metadata_title = CString::new(source_title(&source))
        .map_err(|_| "media title contains a NUL byte".to_string())?;
    let title = CString::new(source_display_title(&source))
        .map_err(|_| "media title contains a NUL byte".to_string())?;
    let window = unsafe { Window::create(&title)? };
    let _wayland_input = match WaylandInput::create(&window) {
        Ok(input) => Some(input),
        Err(error) => {
            eprintln!("warning: {error}; drag-anywhere is unavailable");
            None
        }
    };
    let mut renderer = Renderer::create(&window)?;
    let mut media = unsafe { Media::open(&source, Some(renderer.device()), perf_log)? };

    if let Err(hardware_error) = unsafe { media.fill_initial_queues() } {
        if !media.video.uses_vulkan {
            return Err(hardware_error);
        }
        eprintln!(
            "warning: Vulkan decoding failed ({hardware_error}); restarting with software decoding"
        );
        drop(media);
        media = unsafe { Media::open(&source, None, perf_log)? };
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
    let media_metadata = unsafe { crate::metadata::MediaMetadata::inspect(media.format) };
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
                    chapter_heading.hovered = None;
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_WINDOW_EXPOSED => redraw = true,
                ffi::UpEventType_UP_EVENT_WINDOW_FOCUS_GAINED => {
                    top_bar.set_focused(true);
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_WINDOW_FOCUS_LOST => {
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
                    chapter_heading.hovered = None;
                    redraw = true;
                }
                ffi::UpEventType_UP_EVENT_MOUSE_MOTION => {
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
                        Some(Action::ToggleZoom) => {
                            let enabled = renderer.toggle_zoom();
                            let status = if enabled { "ZOOM: FILL" } else { "ZOOM: FIT" };
                            eprintln!("{}", status.to_ascii_lowercase());
                            position_notice.show_text(CString::new(status).unwrap());
                            redraw = true;
                        }
                        Some(Action::CycleDeinterlace) => {
                            let status = renderer.cycle_deinterlace();
                            eprintln!("{}", status.to_ascii_lowercase());
                            position_notice.show_text(CString::new(status).unwrap());
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
                                .unwrap(),
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
                            muted = requested;
                            if let Some(mpris) = &mpris {
                                mpris.options(muted, looping);
                            }
                            let status = if muted { "AUDIO: MUTED" } else { "AUDIO: ON" };
                            eprintln!("{}", status.to_ascii_lowercase());
                            position_notice.show_text(CString::new(status).unwrap());
                            redraw = true;
                        }
                        Some(Action::ToggleLoop) => {
                            looping = !looping;
                            if let Some(mpris) = &mpris {
                                mpris.options(muted, looping);
                            }
                            let status = if looping { "LOOP: ON" } else { "LOOP: OFF" };
                            eprintln!("{}", status.to_ascii_lowercase());
                            position_notice.show_text(CString::new(status).unwrap());
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
                            renderer.reset_autocrop();
                            previous_video = None;
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
                    info: stats_info.as_deref().map(|text| (text, 1.0)),
                    details: (info_visible && !palette.open).then_some(video_details.as_c_str()),
                    metadata: (!palette.open)
                        .then_some(current_metadata.as_deref())
                        .flatten(),
                    position: position.map(|text| (text, position_alpha)),
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

        let playback_finished = if let Some(media) = decoder.try_lock()? {
            media.eof
                && media.video_queue.is_empty()
                && video_queue.is_empty()
                && decoder.pending_frames()
                    == usize::from(current_video.is_some()) + usize::from(previous_video.is_some())
                && unsafe { media.audio_empty() }
                && current_video.as_ref().is_none_or(|frame| {
                    playback_time >= frame.frame.pts + frame.frame.duration.max(0.1)
                })
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

        unsafe { ffi::up_platform_delay(2) };
    }

    Ok(())
}

#[cfg(test)]
mod palette_tests {
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
            unsafe { ffi::up_av_frame_is_vulkan(background.as_ptr()) },
            0
        );
        assert!(resume_background(true, true).unwrap().is_none());
        assert!(resume_background(false, false).unwrap().is_none());
    }

    #[test]
    fn typed_commands_keep_their_arguments_and_ignore_display_normalization() {
        for (query, expected) in [
            ("seek forward 1", PaletteSeek::Relative(1.0)),
            ("seek forward 30 secs", PaletteSeek::Relative(30.0)),
            ("seek backward 2 mins", PaletteSeek::Relative(-120.0)),
            ("seek backwards 2 mins", PaletteSeek::Relative(-120.0)),
            ("seek ahead 30", PaletteSeek::Relative(30.0)),
            ("back 5", PaletteSeek::Relative(-5.0)),
            ("seek forward 10.0", PaletteSeek::Relative(10.0)),
            ("jump  123", PaletteSeek::Absolute(123.0)),
            ("  JUMP 2:30  ", PaletteSeek::Absolute(150.0)),
            ("jump 1:02:03.5", PaletteSeek::Absolute(3723.5)),
        ] {
            let mut palette = Palette::new();
            palette.query = query.into();
            assert_eq!(palette.rows(true, true).len(), 1, "{query}");
            assert_eq!(
                palette.selected_action(true, true),
                Some(PaletteCommand::Seek(expected)),
                "{query}"
            );
            assert!(
                !palette
                    .display(true, true)
                    .to_string_lossy()
                    .contains("NO MATCHES"),
                "{query}"
            );
        }
    }

    #[test]
    fn malformed_times_and_amounts_cannot_execute() {
        for query in [
            "jump",
            "jump 1:-30",
            "jump 1:60",
            "jump 1:60:00",
            "jump -1",
            "jump NaN",
            "jump inf",
            "jump 1:NaN",
            "seek forward 0",
            "seek back -10",
            "seek forward NaN",
            "seek forward 10 bananas",
        ] {
            let mut palette = Palette::new();
            palette.query = query.into();
            assert!(palette.selected_action(true, true).is_none(), "{query}");
        }
    }

    #[test]
    fn argument_commands_advance_through_steps_before_execution() {
        let mut palette = Palette::new();
        palette.query = "seek".into();
        assert_eq!(
            palette
                .rows(true, true)
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["Forward", "Backward"]
        );
        palette.selected = 1;
        assert_eq!(palette.activate(true, true), None);
        assert_eq!(palette.query, "seek backward ");
        assert_eq!(
            palette
                .rows(true, true)
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["10 seconds", "30 seconds", "60 seconds", "120 seconds"]
        );
        palette.selected = 1;
        assert_eq!(
            palette.activate(true, true),
            Some(PaletteCommand::Seek(PaletteSeek::Relative(-30.0)))
        );
        palette.query = "seek forward 2.5".into();
        palette.selected = 0;
        assert_eq!(
            palette.activate(true, true),
            Some(PaletteCommand::Seek(PaletteSeek::Relative(2.5)))
        );

        palette.query = "jum".into();
        assert_eq!(palette.activate(true, true), None);
        assert_eq!(palette.query, "jump ");
        assert_eq!(palette.activate(true, true), None);
        palette.query.push_str("2:30");
        assert_eq!(
            palette.activate(true, true),
            Some(PaletteCommand::Seek(PaletteSeek::Absolute(150.0)))
        );
    }

    #[test]
    fn navigating_keeps_width_and_selection_consistent() {
        let mut palette = Palette::new();
        let rows = palette.rows(true, true);
        let mut widths = Vec::new();
        for selected in 0..rows.len() {
            palette.selected = selected;
            widths.push(
                palette
                    .display(true, true)
                    .to_string_lossy()
                    .lines()
                    .map(|line| line.chars().count())
                    .max()
                    .unwrap(),
            );
        }
        assert!(widths.iter().all(|width| *width == widths[0]));
        palette.selected = rows.len();
        let before = palette.selected_action(true, true);
        palette.display(true, true);
        assert_eq!(before, palette.selected_action(true, true));
    }
}
